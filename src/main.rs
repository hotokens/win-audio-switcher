//! SwitcherProgramm — an audio output device switcher in the system tray.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod config;
mod hotkey;
mod i18n;
mod icon;
mod policy;
mod toast;
mod tray;
mod version;

use std::collections::HashSet;
use std::time::{Duration, Instant};

use tray_icon::menu::MenuEvent;
use tray_icon::{ClickType, TrayIconEvent};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::POINT;
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Threading::{CreateMutexW, INFINITE};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetCursorPos, GetSystemMetrics, MsgWaitForMultipleObjectsEx, PeekMessageW,
    TranslateMessage, MSG, MWMO_INPUTAVAILABLE, PM_REMOVE, QS_ALLINPUT, SM_CXSCREEN, SM_CYSCREEN,
    WM_HOTKEY, WM_QUIT,
};

/// Name of the named mutex for single-instance.
/// The `Local\` prefix is mandatory: the mutex must live in the current user's session.
const INSTANCE_MUTEX_NAME: PCWSTR = w!("Local\\SwitcherProgramm.Instance");

/// Click burst suppression window: a double click on a Windows icon arrives
/// as `Double` plus two `Left` events, but switching must happen only once.
const ICON_CLICK_DEBOUNCE: Duration = Duration::from_millis(350);

/// Event loop wake-up period while capturing a hotkey (the keyboard is being polled).
const CAPTURE_POLL_MS: u32 = 15;

/// Delay before rebuilding the menu after device events: Windows delivers
/// them in bursts (one event per role for a default change), while enumerating
/// devices and rebuilding the menu are expensive operations.
const REFRESH_DEBOUNCE: Duration = Duration::from_millis(150);

/// Capture state for the hotkey (the user is choosing a combination).
struct CaptureState {
    /// Animation frame index of the dots to the left of the settings field.
    dots: u8,
    /// When the animation was last updated.
    last_tick: Instant,
    /// Keys held during the previous poll — so only new presses are detected.
    prev_down: HashSet<u32>,
}

/// Application state structure.
struct AppState {
    audio: audio::AudioManager,
    favorites: config::Favorites,
    current_device: String,
    cycle_index: usize,
    tray: tray::TrayManager,
    toast: toast::Toast,
    /// Tray icon rectangle (left, top, right, bottom) obtained from the last
    /// click; used to place the toast exactly at the icon.
    last_icon_rect: Option<(i32, i32, i32, i32)>,
    /// Active hotkey capture (None — normal mode).
    capture: Option<CaptureState>,
    /// Time of the last switch by icon click — so a double click
    /// (Windows also sends `Double`, followed by a second `Left`) does not
    /// switch the device twice.
    last_icon_cycle: Option<Instant>,
    /// When devices should be re-read (coalesces a burst of notifications).
    refresh_due: Option<Instant>,
}

impl AppState {
    fn new() -> Self {
        // Load the config.
        let favorites = config::load_favorites();

        // Create the audio manager.
        let audio = audio::AudioManager::new();

        // Get the current default device.
        let current_device = audio.get_default_endpoint().unwrap_or_default();

        // Initial cycle position — the position of the current default among active favourites.
        let current_default = audio.get_default_endpoint().ok();
        let cycle_index = audio
            .get_favorites(&favorites)
            .into_iter()
            .filter(|d| d.state == audio::DeviceState::Active)
            .position(|d| Some(&d.id) == current_default.as_ref())
            .unwrap_or(0);

        // Create the tray manager.
        let tray = tray::TrayManager::new(&audio, &favorites, &current_device, cycle_index);

        // Register the saved hotkey (if one was set).
        if let Some(hk) = &favorites.hotkey {
            match hk.register() {
                Ok(()) => log::info!("Hotkey registered: {}", hk.display()),
                Err(e) => log::warn!("Hotkey {} unavailable: {}", hk.display(), e),
            }
        }

        Self {
            audio,
            favorites,
            current_device,
            cycle_index,
            tray,
            toast: toast::Toast::new(),
            last_icon_rect: None,
            capture: None,
            last_icon_cycle: None,
            refresh_due: None,
        }
    }

    /// Schedule a device re-read (a burst of events collapses into one).
    fn schedule_refresh(&mut self) {
        self.refresh_due = Some(Instant::now() + REFRESH_DEBOUNCE);
    }

    /// Perform the deferred refresh if the pause after events has already elapsed.
    fn flush_due_refresh(&mut self) {
        if let Some(due) = self.refresh_due {
            if Instant::now() >= due {
                self.refresh_due = None;
                self.refresh_from_system();
            }
        }
    }

    /// How long to wait for the next event (ms): accounts for hotkey capture
    /// and the deferred device refresh.
    fn wait_timeout_ms(&self) -> u32 {
        if self.capture.is_some() {
            return CAPTURE_POLL_MS;
        }
        match self.refresh_due {
            Some(due) => {
                let now = Instant::now();
                if due <= now {
                    0
                } else {
                    (due - now).as_millis().min(u32::MAX as u128) as u32
                }
            }
            None => INFINITE,
        }
    }

    /// Re-read the system state: current device, cycle position,
    /// menu and tooltip. Called when devices are added/removed or the
    /// default was changed externally.
    fn refresh_from_system(&mut self) {
        if let Ok(default) = self.audio.get_default_endpoint() {
            self.current_device = default;
        }
        self.sync_cycle_index();
        self.tray.update(
            &self.audio,
            &self.favorites,
            &self.current_device,
            self.cycle_index,
        );
        log::debug!("State refreshed after device notification");
    }

    /// Anchor for the toast: the tray icon rectangle, otherwise the cursor position.
    fn toast_anchor(&self) -> (i32, i32, i32, i32) {
        self.last_icon_rect.unwrap_or_else(|| {
            let mut p = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut p);
            }
            (p.x, p.y, p.x, p.y + 16)
        })
    }

    /// Synchronize cycle_index with the real default device.
    ///
    /// Index = position of the current default among ACTIVE favourites;
    /// if the current one is not a favourite (or not active) — 0.
    /// Returns `(index, total active)` — for the “N of M” labels.
    fn sync_cycle_index(&mut self) -> (usize, usize) {
        let current = self.audio.get_default_endpoint().ok();
        let active: Vec<String> = self
            .audio
            .get_favorites(&self.favorites)
            .into_iter()
            .filter(|d| d.state == audio::DeviceState::Active)
            .map(|d| d.id)
            .collect();
        let total = active.len();
        let pos = active
            .iter()
            .position(|id| Some(id) == current.as_ref())
            .unwrap_or(0);
        self.cycle_index = pos;
        (pos, total)
    }

    /// Show a balloon tip about the new active device
    /// (at the tray icon, as on hover; disappears after 1 s).
    fn show_switch_toast(&mut self, device_name: &str, position: usize, total: usize) {
        let text = format!(
            "{}\n{}",
            device_name,
            i18n::lang().device_position(position, total)
        );
        let anchor = self.toast_anchor();
        self.toast.show_anchored(&text, anchor);
    }

    /// Short message in the toast (without switching the device).
    fn flash_toast(&mut self, text: &str) {
        let anchor = self.toast_anchor();
        self.toast.show_anchored(text, anchor);
    }

    /// Start capturing a new combination.
    ///
    /// The current combination is unregistered so that pressing the previous
    /// one does not switch the device during configuration.
    fn start_capture(&mut self) {
        hotkey::Hotkey::unregister_all();
        self.capture = Some(CaptureState {
            dots: 0,
            last_tick: Instant::now(),
            prev_down: HashSet::new(),
        });
        self.show_capture_toast();
        log::info!("Hotkey capture started");
    }

    /// Capture toast with animated dots to the left of the settings field.
    fn show_capture_toast(&mut self) {
        let dots = match self.capture.as_ref().map(|c| c.dots % 3) {
            Some(1) => "·  ●  ·",
            Some(2) => "·  ·  ●",
            _ => "●  ·  ·",
        };
        let lang = i18n::lang();
        let text = format!(
            "{}   {}: {}",
            dots,
            lang.hotkey_label(),
            lang.capture_hint()
        );
        let anchor = self.toast_anchor();
        self.toast.show_anchored(&text, anchor);
    }

    /// Abort the capture (Esc): restore the previous combination if there was one.
    fn cancel_capture(&mut self) {
        let lang = i18n::lang();
        self.capture = None;
        if let Some(hk) = self.favorites.hotkey {
            if let Err(e) = hk.register() {
                log::warn!("Failed to restore hotkey: {}", e);
            }
            self.flash_toast(lang.hotkey_unchanged());
        } else {
            self.flash_toast(lang.hotkey_not_set());
        }
    }

    /// Accept the new combination: register, save, update the menu.
    fn finish_capture(&mut self, hk: hotkey::Hotkey) {
        self.capture = None;
        self.favorites.hotkey = Some(hk);
        if let Err(e) = config::save_favorites(&self.favorites) {
            log::error!("Failed to save hotkey: {}", e);
        }
        log::info!("Hotkey set: {}", hk.display());
        self.flash_toast(&i18n::lang().hotkey_item(&hk.display()));
        self.tray.update(
            &self.audio,
            &self.favorites,
            &self.current_device,
            self.cycle_index,
        );
    }

    /// Capture step: dot animation + reaction to a new combination.
    ///
    /// Called from the event loop while `capture.is_some()`.
    fn tick_capture(&mut self) {
        if self.capture.is_none() {
            return;
        }

        let down = hotkey::down_keys();

        let (newly, animate) = {
            let Some(cap) = self.capture.as_mut() else {
                return;
            };
            let newly = hotkey::newly_pressed(&down, &cap.prev_down);
            cap.prev_down = down.into_iter().collect();

            let mut animate = false;
            if cap.last_tick.elapsed() >= Duration::from_millis(250) {
                cap.dots = (cap.dots + 1) % 3;
                cap.last_tick = Instant::now();
                animate = true;
            }
            (newly, animate)
        };

        if let Some(vk) = newly {
            // Esc — cancel configuration.
            if vk == 0x1B {
                self.cancel_capture();
                return;
            }

            let modifiers = hotkey::modifiers_down();
            let lang = i18n::lang();
            if modifiers == 0 {
                // Without a modifier the combination would intercept a plain key
                // press in all applications — not allowed.
                self.flash_toast(lang.need_modifier());
            } else {
                let hk = hotkey::Hotkey { modifiers, vk };
                match hk.register() {
                    Ok(()) => {
                        self.finish_capture(hk);
                        return;
                    }
                    Err(hotkey::HotkeyError::Conflict) => {
                        // Taken by the system or another application — report it
                        // and keep waiting for another combination.
                        self.flash_toast(lang.hotkey_conflict());
                    }
                    Err(e) => {
                        self.flash_toast(&lang.hotkey_failed(&e.to_string()));
                    }
                }
            }
        }

        if animate {
            self.show_capture_toast();
        }
    }

    /// Switch to the next device in the cycle.
    fn cycle_next(&mut self) {
        // Skip disabled devices: the cycle only walks active ones.
        let active_devices: Vec<_> = self
            .audio
            .get_favorites(&self.favorites)
            .into_iter()
            .filter(|d| d.state == audio::DeviceState::Active)
            .collect();

        // 0 devices — nothing to do; 1 device — nowhere to switch.
        if active_devices.len() < 2 {
            return;
        }

        // Next one — after the current default (the position is taken from the
        // system, not from the stored index: the user may have changed the device
        // manually via the system panel).
        let current = self.audio.get_default_endpoint().ok();
        let current_pos = current
            .as_ref()
            .and_then(|id| active_devices.iter().position(|d| &d.id == id));
        let next_pos = match current_pos {
            Some(pos) => (pos + 1) % active_devices.len(),
            None => 0,
        };

        let target = &active_devices[next_pos];
        if let Err(e) = policy::set_default_endpoint(&target.id) {
            log::error!("Failed to set default endpoint: {}", e);
            return;
        }

        let device_name = target.name.clone();
        let position = next_pos + 1;
        let total = active_devices.len();

        self.current_device = target.id.clone();
        self.sync_cycle_index();
        self.tray.update(
            &self.audio,
            &self.favorites,
            &self.current_device,
            self.cycle_index,
        );
        self.show_switch_toast(&device_name, position, total);
    }

    /// Add or remove a device from favourites (click on a checkbox item).
    fn toggle_favorite(&mut self, endpoint_id: &str) {
        if let Some(index) = self
            .favorites
            .devices
            .iter()
            .position(|id| id == endpoint_id)
        {
            self.favorites.devices.remove(index);
        } else {
            self.favorites.devices.push(endpoint_id.to_string());
        }

        // Save the config on every favourites change (AGENTS.md requirement).
        if let Err(e) = config::save_favorites(&self.favorites) {
            log::error!("Failed to save favorites: {}", e);
        }

        // The current device was removed from the cycle — the position may have changed.
        self.sync_cycle_index();
        self.tray.update(
            &self.audio,
            &self.favorites,
            &self.current_device,
            self.cycle_index,
        );
    }
}

fn main() {
    // Logging initialization (debug build — console, logs are visible).
    #[cfg(debug_assertions)]
    env_logger::init();

    // CLI diagnostics mode (--list / --switch <N>): before the mutex and the tray.
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        std::process::exit(cli_mode(&args));
    }

    // COM initialization (STA): must happen before any COM calls.
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if hr.is_err() {
        log::error!("Failed to initialize COM: 0x{:08x}", hr.0);
        return;
    }
    // RAII guard: CoUninitialize is called when main exits (after AppState is dropped).
    let _com_guard = ComUninitializer;

    // Single-instance: named mutex.
    unsafe {
        // A HANDLE in windows-rs has no Drop and is not closed on drop:
        // the handle stays open until the process exits, so the
        // named mutex object lives for the whole application lifetime.
        let _mutex_handle = match CreateMutexW(None, true, INSTANCE_MUTEX_NAME) {
            Ok(h) => h,
            Err(e) => {
                log::error!("Failed to create mutex: {}", e);
                return;
            }
        };

        // GetLastError() must be called immediately after CreateMutexW.
        if GetLastError() == ERROR_ALREADY_EXISTS {
            log::info!("Another instance is already running.");
            return;
        }
    }

    // Create the application state.
    let mut state = AppState::new();

    // Start the event loop.
    run_event_loop(&mut state);
}

/// Main event loop: message loop + handling of tray and menu events.
///
/// tray-icon and muda deliver events through global channels
/// (`TrayIconEvent::receiver()` / `MenuEvent::receiver()`), which are
/// filled from the window procedures of their hidden windows — therefore the
/// channels must be drained after every pass of the message loop.
fn run_event_loop(state: &mut AppState) {
    loop {
        // 1. Pump all accumulated messages.
        //    WM_HOTKEY arrives in the thread queue (RegisterHotKey with hwnd = NULL) —
        //    this is the firing of the global combination from any application.
        let mut msg = MSG::default();
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    return;
                }
                if msg.message == WM_HOTKEY {
                    state.cycle_next();
                    continue;
                }
                if msg.message == audio::WM_DEVICES_CHANGED {
                    // The callback has already set the flag; the message merely woke us up.
                    audio::take_refresh_request();
                    state.schedule_refresh();
                    continue;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        // 2. Clicks on the icon itself. Any click carries the icon coordinates —
        // cache the rectangle for the toast when switching via the menu.
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            let rect = event.icon_rect;
            state.last_icon_rect = Some((
                rect.left as i32,
                rect.top as i32,
                rect.right as i32,
                rect.bottom as i32,
            ));

            // Windows sends a double click as `Double` + two `Left` events,
            // so only `Left` is handled and the burst is suppressed by time.
            if matches!(event.click_type, ClickType::Left) {
                let now = Instant::now();
                let too_soon = state
                    .last_icon_cycle
                    .is_some_and(|prev| now.duration_since(prev) < ICON_CLICK_DEBOUNCE);
                if !too_soon {
                    state.last_icon_cycle = Some(now);
                    state.cycle_next();
                }
            }
        }

        // 3. Menu items. "Exit" — break the loop.
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if dispatch_menu_event(state, &event.id.0) {
                return;
            }
        }

        // 4. Devices added/removed or the default changed externally.
        if audio::take_refresh_request() {
            state.schedule_refresh();
        }
        state.flush_due_refresh();

        // 5. Hotkey capture (if in progress): dot animation and key reading.
        state.tick_capture();

        // Wait for the next message instead of `sleep`: Windows wakes the thread
        // as soon as input appears (a click, WM_HOTKEY, a COM notification).
        // The timeout is needed for keyboard polling during capture and for the deferred
        // device refresh; the rest of the time we sleep indefinitely.
        let timeout = state.wait_timeout_ms();
        unsafe {
            MsgWaitForMultipleObjectsEx(None, timeout, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
        }
    }
}

/// Handle a menu event by item ID.
///
/// Returns `true` if application exit was requested.
///
/// ID scheme (see `tray::build_ui`):
/// - `"cycle"` — cyclic switching;
/// - `"hotkey"` — start capturing a global hotkey;
/// - `"version"` — disabled line with the version;
/// - `"exit"` — exit;
/// - `"toggle_<endpoint_id>"` — add/remove a device from favourites.
fn dispatch_menu_event(state: &mut AppState, id: &str) -> bool {
    match id {
        "cycle" => state.cycle_next(),
        "hotkey" => state.start_capture(),
        "version" => {} // disabled line, no events are delivered
        "exit" => return true,
        "empty" => {} // hint item, disabled — no events should be delivered
        _ if id.starts_with("toggle_") => state.toggle_favorite(&id["toggle_".len()..]),
        _ => log::warn!("Unknown menu item id: {}", id),
    }
    false
}

/// CLI diagnostics mode: verify device enumeration and default device switching
/// directly, without the tray or the event loop.
///
/// ```text
/// SwitcherProgramm.exe --list          # list devices, markers the current one
/// SwitcherProgramm.exe --switch <N>    # make device N the default device
/// ```
///
/// Separate COM initialization: this mode is invoked BEFORE main's initialization.
fn cli_mode(args: &[String]) -> i32 {
    unsafe {
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        if hr.is_err() {
            eprintln!("COM init failed: 0x{:08x}", hr.0);
            return 1;
        }
    }
    let _com_guard = ComUninitializer;

    let audio = audio::AudioManager::new();
    let devices = audio.list_devices();

    match args.get(1).map(|s| s.as_str()) {
        Some("--list") => {
            let default = audio.get_default_endpoint().ok();
            for (i, d) in devices.iter().enumerate() {
                let marker = if Some(&d.id) == default.as_ref() {
                    "  <- default"
                } else {
                    ""
                };
                println!(
                    "[{}] (state: {:?}, form: {:?}) {}{}",
                    i, d.state, d.form_factor, d.name, marker
                );
            }
            if devices.is_empty() {
                println!("No render devices found.");
            }
            0
        }
        Some("--switch") => {
            let Some(idx) = args.get(2).and_then(|s| s.parse::<usize>().ok()) else {
                eprintln!("Usage: SwitcherProgramm.exe --switch <index> (see --list)");
                return 2;
            };
            let Some(device) = devices.get(idx) else {
                eprintln!("No device with index {} (see --list)", idx);
                return 2;
            };
            match policy::set_default_endpoint(&device.id) {
                Ok(()) => {
                    println!("OK: switched default to [{}] {}", idx, device.name);
                    0
                }
                Err(e) => {
                    eprintln!("FAILED to switch to [{}] {}: {}", idx, device.name, e);
                    1
                }
            }
        }
        Some("--toast-test") => {
            // Toast diagnostics: show it at the bottom edge of the screen (like the tray
            // icon — the main real-world scenario), check that the window is visible
            // and fully on screen, then that the timer hid it.
            use std::time::{Duration, Instant};

            let mut toast = toast::Toast::new();
            let (sw, sh) =
                unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };

            // Simulate a tray icon: a small square at the bottom edge.
            let anchor = (sw - 60, sh - 48, sw - 20, sh - 8);
            let text = format!("Test notification\n{}", i18n::lang().device_position(1, 2));
            toast.show_anchored(&text, anchor);
            let visible_after_show = toast.debug_is_visible();
            let rect = toast.debug_rect();
            println!("screen:  0,0 {}x{}", sw, sh);
            println!("anchor:  {:?}", anchor);
            println!("rect:    {:?}", rect);

            let mut ok = visible_after_show;
            match rect {
                Some((l, t, r, b)) => {
                    let on_screen = l >= 0 && t >= 0 && r <= sw && b <= sh;
                    println!("on screen: {}", on_screen);
                    ok &= on_screen;
                }
                None => {
                    println!("on screen: unknown (no tooltip window)");
                    ok = false;
                }
            }

            // Pump messages for 1.5 s: WM_TIMER must deactivate the tooltip.
            let start = Instant::now();
            unsafe {
                let mut msg = MSG::default();
                while start.elapsed() < Duration::from_millis(1500) {
                    if PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    } else {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
            }
            let visible_after_timer = toast.debug_is_visible();
            println!("visible after show:  {}", visible_after_show);
            println!("visible after 1.5 s: {}", visible_after_timer);
            if ok && !visible_after_timer {
                println!("PASS");
                0
            } else {
                println!("FAIL");
                1
            }
        }
        Some("--hotkey-test") => {
            // Verify the hotkey mechanics without manual involvement:
            //  1) system combinations must be rejected (Conflict);
            //  2) Ctrl+Shift+U must register and deliver WM_HOTKEY
            //     (the press is synthesized with SendInput).
            use windows::Win32::UI::Input::KeyboardAndMouse::{
                SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
                KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL, VK_SHIFT,
            };
            use windows::Win32::UI::WindowsAndMessaging::WM_HOTKEY;

            let mut failed = false;

            // 1. Known system combinations.
            for (name, modifiers, vk) in [
                ("Win + L", 0x0008u32, 0x4Cu32),
                ("Alt + Tab", 0x0001, 0x09),
                ("Ctrl + Esc", 0x0002, 0x1B),
            ] {
                let hk = hotkey::Hotkey { modifiers, vk };
                match hk.register() {
                    Ok(()) => {
                        println!("{:<12} -> registered (unexpected!)", name);
                        hotkey::Hotkey::unregister_all();
                        failed = true;
                    }
                    Err(e) => println!("{:<12} -> rejected: {}", name, e),
                }
            }

            // 2. Our own combination: Ctrl + Shift + U.
            let hk = hotkey::Hotkey {
                modifiers: 0x0002 | 0x0004,
                vk: 0x55,
            };
            match hk.register() {
                Ok(()) => println!("{} -> registered", hk.display()),
                Err(e) => {
                    println!("{} -> FAILED: {}", hk.display(), e);
                    return 1;
                }
            }

            // Synthesize the press: Ctrl↓ Shift↓ U↓ U↑ Shift↑ Ctrl↑.
            let key = |vk: u16, up: bool| INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(vk),
                        wScan: 0,
                        dwFlags: if up {
                            KEYEVENTF_KEYUP
                        } else {
                            KEYBD_EVENT_FLAGS(0)
                        },
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };
            let inputs = [
                key(VK_CONTROL.0, false),
                key(VK_SHIFT.0, false),
                key(0x55, false),
                key(0x55, true),
                key(VK_SHIFT.0, true),
                key(VK_CONTROL.0, true),
            ];
            unsafe {
                SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
            }

            // Wait for WM_HOTKEY.
            let mut hits = 0u32;
            let start = Instant::now();
            unsafe {
                let mut msg = MSG::default();
                while start.elapsed() < Duration::from_millis(1200) {
                    while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                        if msg.message == WM_HOTKEY {
                            hits += 1;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            hotkey::Hotkey::unregister_all();

            println!("WM_HOTKEY received: {}", hits);
            if hits == 0 {
                println!("FAIL: hotkey did not fire");
                failed = true;
            } else {
                println!("PASS");
            }
            if failed {
                1
            } else {
                0
            }
        }
        Some("--lang") => {
            // Localization diagnostics: which language is selected and how the strings look.
            let lang = i18n::lang();
            println!("lang: {:?}", lang);
            println!("menu cycle:      {}", lang.switch_item(1, 3));
            println!("menu hotkey:     {}", lang.hotkey_item("Ctrl + Shift + U"));
            println!("menu hotkey unset: {}", lang.hotkey_unset());
            println!("menu favorites:  {}", lang.favorites_label());
            println!("menu exit:       {}", lang.exit_label());
            println!("toast:           {}", lang.device_position(1, 3));
            println!("menu version:    {}", lang.version_label(&version::full()));
            0
        }
        Some("--version") => {
            println!("{}", version::full());
            0
        }
        _ => {
            eprintln!("Usage: SwitcherProgramm.exe --list | --switch <index> | --toast-test | --hotkey-test | --lang | --version");
            2
        }
    }
}

/// RAII guard for CoUninitialize.
struct ComUninitializer;

impl Drop for ComUninitializer {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}
