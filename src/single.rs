//! Keeps a single running copy. A second launch asks the first one to show
//! its window and exits, instead of fighting it for the same hotkeys.

pub enum Instance {
    /// We are the only copy; call [`Primary::listen`] once the UI is up.
    Primary(Primary),
    /// Another copy is running (and was asked to show itself if requested).
    Secondary,
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
    use windows_sys::Win32::System::Threading::{
        CreateEventW, INFINITE, SetEvent, WaitForSingleObject,
    };

    use super::Instance;

    // Per-session, so different Windows users each get their own copy.
    const NAME: &str = "Local\\scr8-screenshots-show-window";

    pub struct Primary(HANDLE);

    // The event handle is only waited on and signalled; both are thread-safe.
    unsafe impl Send for Primary {}

    pub fn acquire(show_existing: bool, dev: bool) -> Instance {
        // Development builds run next to the real copy.
        if dev {
            return Instance::Primary(Primary(std::ptr::null_mut()));
        }
        let name: Vec<u16> = NAME.encode_utf16().chain(Some(0)).collect();
        // Auto-reset event: one signal wakes the listener once.
        let h = unsafe { CreateEventW(std::ptr::null(), 0, 0, name.as_ptr()) };
        if h.is_null() {
            // Can't coordinate; better to run than to refuse.
            return Instance::Primary(Primary(h));
        }
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            if show_existing {
                unsafe { SetEvent(h) };
            }
            return Instance::Secondary;
        }
        Instance::Primary(Primary(h))
    }

    impl Primary {
        pub fn listen(self, on_show: impl Fn() + Send + 'static) {
            if self.0.is_null() {
                return;
            }
            std::thread::Builder::new()
                .name("scr8-single".into())
                .spawn(move || {
                    let h = self;
                    loop {
                        if unsafe { WaitForSingleObject(h.0, INFINITE) } != 0 {
                            return;
                        }
                        on_show();
                    }
                })
                .ok();
        }
    }
}

#[cfg(unix)]
mod imp {
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;

    use super::Instance;

    pub struct Primary(Option<UnixListener>);

    fn socket_path() -> Option<PathBuf> {
        let dir = crate::config::data_dir()?;
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir.join("instance.sock"))
    }

    pub fn acquire(show_existing: bool, _dev: bool) -> Instance {
        // Development builds use their own data dir, hence their own socket.
        let Some(path) = socket_path() else {
            return Instance::Primary(Primary(None));
        };
        if let Ok(mut s) = UnixStream::connect(&path) {
            let _ = s.write_all(if show_existing { b"show" } else { b"ping" });
            return Instance::Secondary;
        }
        // Nobody answered: the socket file, if any, is left from a crash.
        let _ = std::fs::remove_file(&path);
        Instance::Primary(Primary(UnixListener::bind(&path).ok()))
    }

    impl Primary {
        pub fn listen(self, on_show: impl Fn() + Send + 'static) {
            let Some(listener) = self.0 else {
                return;
            };
            std::thread::Builder::new()
                .name("scr8-single".into())
                .spawn(move || {
                    for mut stream in listener.incoming().flatten() {
                        let mut msg = [0u8; 4];
                        if stream.read_exact(&mut msg).is_ok() && &msg == b"show" {
                            on_show();
                        }
                    }
                })
                .ok();
        }
    }
}

pub use imp::{Primary, acquire};
