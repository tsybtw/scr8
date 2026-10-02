//! The background service: hotkeys, capture, the tray icon and the area
//! picker for edit hotkeys. It has no window of its own; the settings window
//! is a separate process (see `app`) that it starts on demand and talks to
//! over `ipc`. With "keep settings in memory" off, that process exits when
//! its window closes, leaving only this small one running.

use std::collections::{HashMap, HashSet};
use std::process::{Child, Command};
use std::sync::atomic::Ordering;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use crate::config::{Config, Region};
use crate::engine::{Engine, Target};
use crate::hotkey::Hotkey;
use crate::ipc::{self, Status, ToService, ToSettings};
use crate::tray::{Tray, TrayCmd};
use crate::{autostart, capture, picker, single};

/// Scripted check for CI, only in builds made with `--features selftest`.
#[cfg(feature = "selftest")]
mod selftest;

/// Hands an event to the service loop, from any thread.
type Waker = Arc<dyn Fn(Wake) + Send + Sync>;

/// Everything that wakes the service loop.
enum Wake {
    Tray(TrayCmd),
    /// An edit hotkey for this bind was pressed.
    Edit(u64),
    /// scr8 was launched again: show the settings window.
    ShowSettings,
    Connected(ipc::Sender),
    Message(ToService),
    Disconnected,
    Picked {
        id: u64,
        region: Option<Region>,
        reopen: bool,
    },
    /// Something changed in the engine (e.g. an error).
    Engine,
    #[cfg(feature = "selftest")]
    SelfTest(selftest::Step),
}

pub fn run(open_settings: bool, instance: single::Primary) {
    #[cfg(windows)]
    win_loop::run(open_settings, instance);
    #[cfg(target_os = "macos")]
    mac_loop::run(open_settings, instance);
}

struct Service {
    wake: Waker,
    /// Set to leave the loop.
    quit: bool,
    cfg: Config,
    engine: Engine,
    key_tx: Sender<GlobalHotKeyEvent>,
    manager: Option<GlobalHotKeyManager>,
    registered: Vec<HotKey>,
    /// Shot-hotkey id → bind id, for the self-test.
    shot_ids: HashMap<u64, u32>,
    edit_keys: Arc<RwLock<HashMap<u32, u64>>>,
    bind_errors: HashMap<u64, String>,
    tray: Option<Tray>,
    errors_seen: u64,
    last_toast: Option<Instant>,
    /// Hotkeys are off while the settings window records a new one.
    paused: bool,
    picking: bool,
    settings: Option<Child>,
    link: Option<ipc::Sender>,
    settings_visible: bool,
    /// Show the settings window as soon as it connects.
    show_on_connect: bool,
    open_on_start: bool,
    instance: Option<single::Primary>,
    last_status: Option<Status>,
    started: bool,
}

impl Service {
    fn new(wake: Waker, open_on_start: bool, instance: single::Primary) -> Self {
        let cfg = Config::load();
        let w = wake.clone();
        let (engine, key_tx) = Engine::start(cfg.png_level, move || w(Wake::Engine));
        Self {
            wake,
            quit: false,
            cfg,
            engine,
            key_tx,
            manager: None,
            registered: Vec::new(),
            shot_ids: HashMap::new(),
            edit_keys: Arc::default(),
            bind_errors: HashMap::new(),
            tray: None,
            errors_seen: 0,
            last_toast: None,
            paused: false,
            picking: false,
            settings: None,
            link: None,
            settings_visible: false,
            show_on_connect: false,
            open_on_start,
            instance: Some(instance),
            last_status: None,
            started: false,
        }
    }

    /// Runs once the event loop is live (macOS needs it for the tray).
    fn start(&mut self) {
        if std::mem::replace(&mut self.started, true) {
            return;
        }
        capture::ensure_permission();
        if self.cfg.autostart && !crate::config::is_dev() {
            let _ = autostart::apply(true);
        }

        // Screenshot hotkeys go straight to the capture thread; edit
        // hotkeys come to this loop to open the area picker.
        let (keys, key_tx, p) = (
            self.edit_keys.clone(),
            self.key_tx.clone(),
            self.wake.clone(),
        );
        GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
            match keys.read().unwrap().get(&e.id) {
                Some(&bind) => {
                    if e.state == HotKeyState::Pressed {
                        p(Wake::Edit(bind));
                    }
                }
                None => {
                    let _ = key_tx.send(e);
                }
            }
        }));
        self.manager = GlobalHotKeyManager::new().ok();
        self.sync_hotkeys();

        let p = self.wake.clone();
        self.tray = Tray::create(Arc::new(move |cmd| p(Wake::Tray(cmd))));

        if let Some(instance) = self.instance.take() {
            let p = self.wake.clone();
            instance.listen(move || {
                p(Wake::ShowSettings);
            });
        }

        match ipc::Server::start() {
            Ok(server) => {
                let (p1, p2, p3) = (self.wake.clone(), self.wake.clone(), self.wake.clone());
                server.serve(
                    move |s| p1(Wake::Connected(s)),
                    move |m| {
                        p2(Wake::Message(m));
                    },
                    move || {
                        p3(Wake::Disconnected);
                    },
                );
            }
            Err(e) => {
                crate::render::show_error(&format!("scr8 can't start its settings link: {e}"))
            }
        }

        if self.open_on_start {
            self.open_settings();
        } else if self.cfg.keep_settings_open {
            self.spawn_settings(true);
        }

        #[cfg(feature = "selftest")]
        selftest::start(self.wake.clone());
    }

    fn shutdown(&mut self) {
        if let Some(link) = &self.link {
            link.send(&ToSettings::Exit);
        }
        if let Some(mut child) = self.settings.take() {
            // Give it a moment to exit by itself, then make sure.
            for _ in 0..20 {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            let _ = child.kill();
        }
    }

    // --- settings window process ----------------------------------------

    fn settings_alive(&mut self) -> bool {
        // A connected window is running even if it isn't our child any more
        // (it may have relaunched itself in software rendering mode).
        if self.link.is_some() {
            return true;
        }
        match &mut self.settings {
            Some(child) => match child.try_wait() {
                Ok(None) => true,
                _ => {
                    self.settings = None;
                    false
                }
            },
            None => false,
        }
    }

    fn spawn_settings(&mut self, hidden: bool) {
        if self.settings_alive() {
            return;
        }
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let mut cmd = Command::new(exe);
        cmd.arg("--settings");
        if hidden {
            cmd.arg("--hidden");
        }
        self.settings = cmd.spawn().ok();
    }

    fn open_settings(&mut self) {
        allow_foreground();
        if let Some(link) = &self.link {
            link.send(&ToSettings::Show);
        } else if self.settings_alive() {
            // Still starting up (or preloading hidden): show once connected.
            self.show_on_connect = true;
        } else {
            self.spawn_settings(false);
        }
    }

    fn send_status(&mut self, force: bool) {
        let status = Status {
            saved: self.engine.saved.load(Ordering::Relaxed),
            last_error: self.engine.last_error.lock().unwrap().clone(),
            bind_errors: self.bind_errors.clone(),
            hotkeys_available: self.manager.is_some(),
        };
        if !force && self.last_status.as_ref() == Some(&status) {
            return;
        }
        if let Some(link) = &self.link {
            link.send(&ToSettings::Status(status.clone()));
        }
        self.last_status = Some(status);
    }

    // --- hotkeys ------------------------------------------------------------

    /// Re-registers the hotkeys of every enabled bind and publishes the
    /// screenshot targets to the engine.
    fn sync_hotkeys(&mut self) {
        let Some(manager) = &self.manager else {
            return;
        };
        let _ = manager.unregister_all(&self.registered);
        self.registered.clear();
        self.bind_errors.clear();
        self.shot_ids.clear();
        let mut targets = HashMap::new();
        let mut edits = HashMap::new();
        let mut used = HashSet::new();
        if !self.paused {
            for b in self.cfg.binds.iter().filter(|b| b.enabled) {
                let mut errors = Vec::new();
                let mut register = |hk: &Hotkey, what: &str| -> Option<u32> {
                    let g = hk.to_global()?;
                    let result = if used.insert(g.id()) {
                        manager.register(g)
                    } else {
                        Err(global_hotkey::Error::AlreadyRegistered(g))
                    };
                    match result {
                        Ok(()) => {
                            self.registered.push(g);
                            Some(g.id())
                        }
                        Err(global_hotkey::Error::AlreadyRegistered(_)) => {
                            errors.push(format!("{what} is taken by another bind or app"));
                            None
                        }
                        Err(e) => {
                            errors.push(format!("Can't register {what}: {e}"));
                            None
                        }
                    }
                };
                if let (Some(hk), Some(region), Some(folder)) = (&b.hotkey, b.region, &b.folder)
                    && let Some(id) = register(hk, "Hotkey")
                {
                    targets.insert(
                        id,
                        Target {
                            name: b.name.clone(),
                            region,
                            folder: folder.clone(),
                        },
                    );
                    self.shot_ids.insert(b.id, id);
                }
                if let Some(hk) = &b.edit_hotkey
                    && let Some(id) = register(hk, "Edit hotkey")
                {
                    edits.insert(id, b.id);
                }
                if !errors.is_empty() {
                    self.bind_errors.insert(b.id, errors.join(". "));
                }
            }
        }
        *self.engine.targets.write().unwrap() = targets;
        *self.edit_keys.write().unwrap() = edits;
    }

    fn reload(&mut self) {
        self.cfg = Config::load();
        self.engine.set_level(self.cfg.png_level);
        self.sync_hotkeys();
        self.send_status(true);
    }

    // --- errors -------------------------------------------------------------

    /// Turns new engine failures into a red tray icon and, at most every
    /// few seconds, a system notification.
    fn check_errors(&mut self) {
        let n = self.engine.errors.load(Ordering::Relaxed);
        if n == self.errors_seen {
            return;
        }
        self.errors_seen = n;
        let Some(msg) = self.engine.last_error.lock().unwrap().clone() else {
            return;
        };
        let Some(tray) = self.tray.as_mut() else {
            return;
        };
        tray.set_error(Some(&msg));
        if self
            .last_toast
            .is_none_or(|t| t.elapsed() > Duration::from_secs(10))
        {
            self.last_toast = Some(Instant::now());
            tray.notify("Screenshot not saved", &msg);
        }
    }

    fn clear_errors(&mut self) {
        *self.engine.last_error.lock().unwrap() = None;
        if let Some(tray) = self.tray.as_mut() {
            tray.set_error(None);
        }
    }

    // --- area editing by hotkey ---------------------------------------------

    fn edit_area(&mut self, id: u64) {
        let Some(bind) = self.cfg.binds.iter().find(|b| b.id == id) else {
            return;
        };
        if self.picking || self.paused || !bind.enabled {
            return;
        }
        self.picking = true;
        let initial = bind.region;
        // Keep the settings window out of the frozen screenshot.
        let reopen = self.settings_visible && self.link.is_some();
        if reopen && let Some(link) = &self.link {
            link.send(&ToSettings::Hide);
        }
        let p = self.wake.clone();
        std::thread::spawn(move || {
            if reopen {
                std::thread::sleep(Duration::from_millis(250));
            }
            let region = picker::pick_area(initial);
            p(Wake::Picked { id, region, reopen });
        });
    }

    fn picked(&mut self, id: u64, region: Option<Region>, reopen: bool) {
        self.picking = false;
        if let Some(region) = region {
            // The settings window may have saved meanwhile: start from disk.
            self.cfg = Config::load();
            if let Some(b) = self.cfg.binds.iter_mut().find(|b| b.id == id) {
                b.region = Some(region);
            }
            let _ = self.cfg.save();
            self.sync_hotkeys();
            if let Some(link) = &self.link {
                link.send(&ToSettings::Reload);
            }
        }
        if reopen && let Some(link) = &self.link {
            allow_foreground();
            link.send(&ToSettings::Show);
        }
    }
}

impl Service {
    fn handle(&mut self, event: Wake) {
        match event {
            Wake::Tray(TrayCmd::Show) | Wake::ShowSettings => self.open_settings(),
            Wake::Tray(TrayCmd::Quit) => self.quit = true,
            Wake::Edit(id) => self.edit_area(id),
            Wake::Connected(link) => {
                self.link = Some(link);
                self.send_status(true);
                if std::mem::take(&mut self.show_on_connect)
                    && let Some(link) = &self.link
                {
                    link.send(&ToSettings::Show);
                }
            }
            Wake::Disconnected => {
                self.link = None;
                self.settings_visible = false;
                // A window that went away mid-recording must not leave the
                // hotkeys off.
                if self.paused {
                    self.paused = false;
                    self.sync_hotkeys();
                }
                self.settings_alive();
            }
            Wake::Message(msg) => match msg {
                ToService::Hello { .. } => {}
                ToService::Reload => {
                    let keep_before = self.cfg.keep_settings_open;
                    self.reload();
                    // Turning "keep in memory" on preloads the window for next time.
                    if self.cfg.keep_settings_open && !keep_before {
                        self.spawn_settings(true);
                    }
                }
                ToService::PauseHotkeys => {
                    self.paused = true;
                    self.sync_hotkeys();
                }
                ToService::ResumeHotkeys => {
                    self.paused = false;
                    self.sync_hotkeys();
                    self.send_status(true);
                }
                ToService::ClearErrors => {
                    self.clear_errors();
                    self.send_status(true);
                }
                ToService::Visible(v) => self.settings_visible = v,
            },
            Wake::Picked { id, region, reopen } => self.picked(id, region, reopen),
            Wake::Engine => {}
            #[cfg(feature = "selftest")]
            Wake::SelfTest(step) => self.self_test_step(step),
        }
        self.check_errors();
        self.send_status(false);
    }

    /// While a settings window is connected, its counter is refreshed on a
    /// timer; otherwise the loop sleeps until something happens.
    fn wants_tick(&self) -> bool {
        self.link.is_some()
    }

    fn tick(&mut self) {
        self.send_status(false);
    }
}

/// Lets the settings process bring its window to the front. Windows only
/// allows that to the app the user just interacted with (a tray click, a new
/// launch, the area picker), which is us, so pass the right on.
fn allow_foreground() {
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(
            windows_sys::Win32::UI::WindowsAndMessaging::ASFW_ANY,
        );
    }
}

// ---------------------------------------------------------------------------
// Event loops

/// Windows: a plain message loop. Wakes go to a hidden message-only window,
/// so they are handled even while the tray menu runs its own modal loop.
#[cfg(windows)]
mod win_loop {
    use std::cell::RefCell;
    use std::ptr::{null, null_mut};
    use std::sync::Arc;

    use crossbeam_channel::{Receiver, unbounded};
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    use super::{Service, Wake};
    use crate::single;

    const WM_WAKE: u32 = WM_APP + 1;
    const TICK: usize = 1;

    thread_local! {
        static LOOP: RefCell<Option<(Service, Receiver<Wake>, HWND)>> = const { RefCell::new(None) };
    }

    pub fn run(open_settings: bool, instance: single::Primary) {
        // Per-monitor DPI awareness, so capture and the picker use real pixels.
        unsafe {
            windows_sys::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
                windows_sys::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            );
        }
        let hwnd = create_window();
        if hwnd.is_null() {
            crate::render::show_error("scr8 can't start its background window.");
            return;
        }
        let (tx, rx) = unbounded::<Wake>();
        let target = hwnd as isize;
        let wake: super::Waker = Arc::new(move |w| {
            let _ = tx.send(w);
            unsafe { PostMessageW(target as HWND, WM_WAKE, 0, 0) };
        });
        let service = Service::new(wake, open_settings, instance);
        LOOP.with_borrow_mut(|l| *l = Some((service, rx, hwnd)));
        with_service(|s| s.start());

        let mut msg: MSG = unsafe { std::mem::zeroed() };
        while unsafe { GetMessageW(&mut msg, null_mut(), 0, 0) } > 0 {
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if let Some((mut service, _, hwnd)) = LOOP.with_borrow_mut(|l| l.take()) {
            service.shutdown();
            unsafe { DestroyWindow(hwnd) };
        }
    }

    /// Runs `f` on the service unless it is already busy (a re-entrant
    /// wake; it will be picked up when the outer call drains the queue).
    fn with_service(f: impl FnOnce(&mut Service)) {
        LOOP.with(|cell| {
            let Ok(mut guard) = cell.try_borrow_mut() else {
                return;
            };
            let Some((service, rx, hwnd)) = guard.as_mut() else {
                return;
            };
            f(service);
            while let Ok(w) = rx.try_recv() {
                service.handle(w);
            }
            unsafe {
                if service.wants_tick() {
                    SetTimer(*hwnd, TICK, 500, None);
                } else {
                    KillTimer(*hwnd, TICK);
                }
            }
            if service.quit {
                unsafe { PostQuitMessage(0) };
            }
        });
    }

    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_WAKE => {
                with_service(|_| {});
                0
            }
            WM_TIMER if wparam == TICK => {
                with_service(|s| s.tick());
                0
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    fn create_window() -> HWND {
        let name: Vec<u16> = "scr8-service".encode_utf16().chain(Some(0)).collect();
        unsafe {
            let instance = GetModuleHandleW(null());
            let class = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: instance,
                lpszClassName: name.as_ptr(),
                ..std::mem::zeroed()
            };
            RegisterClassW(&class);
            CreateWindowExW(
                0,
                name.as_ptr(),
                name.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                null_mut(),
                instance,
                null(),
            )
        }
    }
}

/// macOS: winit runs the AppKit loop (no windows; the app stays in the menu
/// bar only).
#[cfg(target_os = "macos")]
mod mac_loop {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use winit::application::ApplicationHandler;
    use winit::event::WindowEvent;
    use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
    use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
    use winit::window::WindowId;

    use super::{Service, Wake};
    use crate::single;

    pub fn run(open_settings: bool, instance: single::Primary) {
        let mut builder = EventLoop::<Wake>::with_user_event();
        // Menu-bar app: no Dock icon, no menu bar of its own.
        builder.with_activation_policy(ActivationPolicy::Accessory);
        builder.with_default_menu(false);
        let event_loop = match builder.build() {
            Ok(l) => l,
            Err(e) => {
                crate::render::show_error(&format!("scr8 can't start: {e}"));
                return;
            }
        };
        event_loop.set_control_flow(ControlFlow::Wait);
        let proxy = event_loop.create_proxy();
        let wake: super::Waker = Arc::new(move |w| {
            let _ = proxy.send_event(w);
        });
        let mut app = App(Service::new(wake, open_settings, instance));
        let _ = event_loop.run_app(&mut app);
        app.0.shutdown();
    }

    struct App(Service);

    impl App {
        fn after(&mut self, event_loop: &ActiveEventLoop) {
            if self.0.quit {
                event_loop.exit();
            }
        }
    }

    impl ApplicationHandler<Wake> for App {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            self.0.start();
            self.after(event_loop);
        }

        fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Wake) {
            self.0.handle(event);
            self.after(event_loop);
        }

        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            if self.0.wants_tick() {
                self.0.tick();
                event_loop.set_control_flow(ControlFlow::WaitUntil(
                    Instant::now() + Duration::from_millis(500),
                ));
            } else {
                event_loop.set_control_flow(ControlFlow::Wait);
            }
        }

        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
    }
}
