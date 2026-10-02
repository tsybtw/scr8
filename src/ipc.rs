//! Link between the background service and the settings window, which run
//! as separate processes so the settings UI can be unloaded when closed.
//!
//! The service listens on a loopback TCP port and writes the port and a
//! random token to `ipc.json` in the data folder; the settings process reads
//! it and connects. Messages are JSON, one per line.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// Settings window → service.
#[derive(Serialize, Deserialize, Debug)]
pub enum ToService {
    Hello {
        token: String,
    },
    /// The settings file changed; reload it and re-register hotkeys.
    Reload,
    /// Recording a hotkey: keys must reach the settings window.
    PauseHotkeys,
    ResumeHotkeys,
    ClearErrors,
    /// Whether the settings window is on screen.
    Visible(bool),
}

/// Service → settings window.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ToSettings {
    Status(Status),
    /// The service changed the settings file (e.g. an area edited by hotkey).
    Reload,
    Show,
    /// Hide for a moment (the area picker is about to freeze the screen).
    Hide,
    Exit,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub saved: u64,
    pub last_error: Option<String>,
    pub bind_errors: HashMap<u64, String>,
    pub hotkeys_available: bool,
}

#[derive(Serialize, Deserialize)]
struct Endpoint {
    port: u16,
    token: String,
}

fn endpoint_path() -> Option<PathBuf> {
    Some(crate::config::data_dir()?.join("ipc.json"))
}

/// One end of a connection: sends whole lines, safe to share.
#[derive(Clone)]
pub struct Sender(Arc<Mutex<TcpStream>>);

impl Sender {
    pub fn send<T: Serialize>(&self, msg: &T) -> bool {
        let Ok(mut line) = serde_json::to_vec(msg) else {
            return false;
        };
        line.push(b'\n');
        self.0.lock().unwrap().write_all(&line).is_ok()
    }
}

/// Reads messages until the connection closes, handing each to `on_msg`.
fn read_loop<T: for<'de> Deserialize<'de>>(stream: TcpStream, mut on_msg: impl FnMut(T)) {
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        if let Ok(msg) = serde_json::from_str(&line) {
            on_msg(msg);
        }
    }
}

/// Service side: accepts settings windows that know the token.
pub struct Server {
    listener: TcpListener,
    token: String,
}

impl Server {
    pub fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let token = random_token();
        let endpoint = Endpoint {
            port: listener.local_addr()?.port(),
            token: token.clone(),
        };
        if let Some(path) = endpoint_path() {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            std::fs::write(path, serde_json::to_vec(&endpoint)?)?;
        }
        Ok(Self { listener, token })
    }

    /// Runs on a background thread. For each authenticated connection calls
    /// `on_connect` with a sender, then `on_msg` per message and
    /// `on_disconnect` when it closes.
    pub fn serve(
        self,
        on_connect: impl Fn(Sender) + Send + Sync + 'static,
        on_msg: impl Fn(ToService) + Send + Sync + 'static,
        on_disconnect: impl Fn() + Send + Sync + 'static,
    ) {
        let handlers = Arc::new((on_connect, on_msg, on_disconnect));
        std::thread::Builder::new()
            .name("scr8-ipc".into())
            .spawn(move || {
                for stream in self.listener.incoming().flatten() {
                    let token = self.token.clone();
                    let handlers = handlers.clone();
                    std::thread::spawn(move || {
                        let Ok(write_half) = stream.try_clone() else { return };
                        let mut lines = BufReader::new(stream).lines();
                        // The first line must carry the token.
                        let authed = matches!(
                            lines.next().and_then(|l| l.ok()).and_then(|l| serde_json::from_str(&l).ok()),
                            Some(ToService::Hello { token: t }) if t == token
                        );
                        if !authed {
                            return;
                        }
                        (handlers.0)(Sender(Arc::new(Mutex::new(write_half))));
                        // Keep the same reader: it may already hold the next lines.
                        for line in lines {
                            let Ok(line) = line else { break };
                            if let Ok(msg) = serde_json::from_str(&line) {
                                (handlers.1)(msg);
                            }
                        }
                        (handlers.2)();
                    });
                }
            })
            .ok();
    }
}

/// Settings side: connects to the running service.
pub fn connect(on_msg: impl FnMut(ToSettings) + Send + 'static) -> Option<Sender> {
    let raw = std::fs::read(endpoint_path()?).ok()?;
    let ep: Endpoint = serde_json::from_slice(&raw).ok()?;
    let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, ep.port)).ok()?;
    let _ = stream.set_nodelay(true);
    let sender = Sender(Arc::new(Mutex::new(stream.try_clone().ok()?)));
    if !sender.send(&ToService::Hello { token: ep.token }) {
        return None;
    }
    std::thread::Builder::new()
        .name("scr8-ipc".into())
        .spawn(move || read_loop(stream, on_msg))
        .ok()?;
    Some(sender)
}

/// A token nobody else can guess, from the OS's randomized hasher seeds.
fn random_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    (0..4)
        .map(|i| {
            let mut h = RandomState::new().build_hasher();
            h.write_u64(i ^ std::process::id() as u64);
            format!("{:016x}", h.finish())
        })
        .collect()
}
