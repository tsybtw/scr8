use auto_launch::{AutoLaunch, AutoLaunchBuilder, MacOSLaunchMode, WindowsEnableMode};

fn launcher() -> Option<AutoLaunch> {
    let exe = std::env::current_exe().ok()?;
    AutoLaunchBuilder::new()
        .set_app_name("scr8")
        .set_app_path(exe.to_str()?)
        .set_args(&["--hidden"])
        .set_macos_launch_mode(MacOSLaunchMode::LaunchAgent)
        .set_windows_enable_mode(WindowsEnableMode::CurrentUser)
        .build()
        .ok()
}

/// Enables or disables launch at login. Enabling again refreshes the stored
/// path, so moving the app keeps autostart working.
pub fn apply(enabled: bool) -> Result<(), String> {
    let l = launcher().ok_or("can't resolve executable path")?;
    if enabled {
        l.enable().map_err(|e| e.to_string())
    } else if l.is_enabled().unwrap_or(false) {
        l.disable().map_err(|e| e.to_string())
    } else {
        Ok(())
    }
}
