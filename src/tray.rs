use crossbeam_channel::Sender;
use eframe::egui;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::icon::{self, Style};

const TOOLTIP: &str = "scr8 — screenshots";
const MAC: bool = cfg!(target_os = "macos");

pub enum TrayCmd {
    Show,
    Quit,
}

pub struct Tray {
    icon: TrayIcon,
    error: bool,
}

impl Tray {
    pub fn create(ctx: &egui::Context, tx: Sender<TrayCmd>) -> Option<Self> {
        let open = MenuItem::new("Open scr8", true, None);
        let quit = MenuItem::new("Quit", true, None);
        let menu = Menu::new();
        menu.append_items(&[&open, &PredefinedMenuItem::separator(), &quit])
            .ok()?;
        let (open_id, quit_id) = (open.id().clone(), quit.id().clone());

        let c = ctx.clone();
        let t = tx.clone();
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let cmd = if e.id == open_id {
                TrayCmd::Show
            } else if e.id == quit_id {
                TrayCmd::Quit
            } else {
                return;
            };
            let _ = t.send(cmd);
            c.request_repaint();
        }));

        // Left click opens the window on Windows; macOS shows the menu instead.
        let c = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = e
            {
                let _ = tx.send(TrayCmd::Show);
                c.request_repaint();
            }
        }));

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(MAC)
            .with_tooltip(TOOLTIP)
            .with_icon(tray_image(if MAC { Style::Template } else { Style::App })?)
            .with_icon_as_template(MAC)
            .build()
            .ok()?;
        Some(Self { icon, error: false })
    }

    /// Switches the icon to red with the message as tooltip, or back to normal.
    pub fn set_error(&mut self, message: Option<&str>) {
        if message.is_none() && !self.error {
            return;
        }
        self.error = message.is_some();
        let style = match (message, MAC) {
            (Some(_), _) => Style::Error,
            (None, true) => Style::Template,
            (None, false) => Style::App,
        };
        let _ = self
            .icon
            .set_icon_with_as_template(tray_image(style), style == Style::Template);
        let tooltip = message.map_or(TOOLTIP.to_owned(), |m| format!("scr8: {m}"));
        // Windows limits tooltips to 127 characters.
        let _ = self.icon.set_tooltip(Some(truncate(&tooltip, 120)));
    }

    /// Shows a system notification next to the tray icon (Windows only;
    /// on macOS the red menu bar icon is the notification).
    pub fn notify(&self, title: &str, text: &str) {
        #[cfg(windows)]
        balloon(&self.icon, title, text);
        #[cfg(not(windows))]
        let _ = (title, text);
    }
}

fn tray_image(style: Style) -> Option<tray_icon::Icon> {
    let size = if MAC { 36 } else { 32 };
    tray_icon::Icon::from_rgba(icon::rgba(size, style), size, size).ok()
}

fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

/// Windows 10/11 show a notify-icon balloon as a regular toast, attributed to
/// the exe's FileDescription, with no app registration needed.
#[cfg(windows)]
fn balloon(icon: &TrayIcon, title: &str, text: &str) {
    use windows_sys::Win32::UI::Shell::{
        NIF_INFO, NIIF_WARNING, NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
    };

    fn copy_wide(dst: &mut [u16], s: &str) {
        let n = dst.len() - 1;
        for (d, c) in dst.iter_mut().zip(s.encode_utf16().take(n)) {
            *d = c;
        }
    }

    let hwnd = icon.window_handle();
    let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uFlags = NIF_INFO;
    nid.dwInfoFlags = NIIF_WARNING;
    copy_wide(&mut nid.szInfoTitle, title);
    copy_wide(&mut nid.szInfo, text);
    // tray-icon doesn't expose the icon's uID, but each icon owns its hidden
    // window, so only the right id matches this hwnd.
    for id in 1..=32 {
        nid.uID = id;
        if unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) } != 0 {
            break;
        }
    }
}
