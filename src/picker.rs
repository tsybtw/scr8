//! Shows the area picker and waits for the result. Windows draws it in the
//! calling process with GDI; macOS runs it as a child process (`--select`).

use crate::config::Region;

pub fn pick_area(initial: Option<Region>) -> Option<Region> {
    #[cfg(windows)]
    {
        crate::overlay_win::pick(initial)
    }
    #[cfg(not(windows))]
    {
        use std::process::{Command, Stdio};
        let exe = std::env::current_exe().ok()?;
        let mut cmd = Command::new(exe);
        cmd.arg("--select");
        if let Some(r) = initial {
            cmd.arg(r.to_arg());
        }
        let child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        *CHILD.lock().unwrap() = Some(child.id());
        let out = child.wait_with_output().ok();
        *CHILD.lock().unwrap() = None;
        out.filter(|o| o.status.success())
            .and_then(|o| Region::parse(String::from_utf8_lossy(&o.stdout).trim()))
    }
}

#[cfg(not(windows))]
static CHILD: std::sync::Mutex<Option<u32>> = std::sync::Mutex::new(None);

/// Closes an open picker as if Esc was pressed (used by the self-test).
pub fn cancel() {
    #[cfg(windows)]
    crate::overlay_win::cancel();
    #[cfg(not(windows))]
    if let Some(pid) = *CHILD.lock().unwrap() {
        let _ = std::process::Command::new("kill")
            .arg(pid.to_string())
            .status();
    }
}
