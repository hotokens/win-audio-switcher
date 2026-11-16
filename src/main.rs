//! SwitcherProgramm — переключатель устройств вывода в системном трее.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod config;
mod hotkey;
mod i18n;
mod icon;
mod policy;
mod toast;
mod tray;

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

/// Имя именованного мьютекса для single-instance.
/// Префикс `Local\` обязателен: мьютекс должен жить в сессии текущего пользователя.
const INSTANCE_MUTEX_NAME: PCWSTR = w!("Local\\SwitcherProgramm.Instance");

/// Окно подавления серии кликов: двойной клик по иконке Windows присылает
/// как `Double` плюс два `Left`, а переключение должно быть одно.
const ICON_CLICK_DEBOUNCE: Duration = Duration::from_millis(350);

/// Период просыпания event loop во время захвата хоткея (идёт опрос клавиатуры).
const CAPTURE_POLL_MS: u32 = 15;

/// Пауза перед перестройкой меню после событий устройств: Windows присылает
/// их сериями (смена default — по событию на роль), а перечисление устройств
/// и пересборка меню — дорогие операции.
const REFRESH_DEBOUNCE: Duration = Duration::from_millis(150);

/// Состояние захвата горячей клавиши (пользователь выбирает комбинацию).
struct CaptureState {
    /// Индекс кадра анимации точек слева от поля настройки.
    dots: u8,
    /// Когда последний раз обновляли анимацию.
    last_tick: Instant,
    /// Клавиши, зажатые на прошлом опросе — чтобы ловить только новые нажатия.
    prev_down: HashSet<u32>,
}

/// Структура состояния приложения.
struct AppState {
    audio: audio::AudioManager,
    favorites: config::Favorites,
    current_device: String,
    cycle_index: usize,
    tray: tray::TrayManager,
    toast: toast::Toast,
    /// Прямоугольник иконки трея (left, top, right, bottom), полученный
    /// из последнего клика; нужен, чтобы поставить тост ровно у иконки.
    last_icon_rect: Option<(i32, i32, i32, i32)>,
    /// Активный захват хоткея (None — обычный режим).
    capture: Option<CaptureState>,
    /// Момент последнего переключения кликом по иконке — чтобы двойной клик
    /// (Windows присылает ещё и `Double`, а следом второй `Left`) не
    /// переключал устройство дважды.
    last_icon_cycle: Option<Instant>,
    /// Когда пора перечитать устройства (склеивает серию уведомлений).
    refresh_due: Option<Instant>,
}

impl AppState {
    fn new() -> Self {
        // Загрузка конфига.
        let favorites = config::load_favorites();

        // Создание менеджера аудио.
        let audio = audio::AudioManager::new();

        // Получение текущего устройства по умолчанию.
        let current_device = audio.get_default_endpoint().unwrap_or_default();

        // Начальная позиция цикла — позиция текущего default среди активных избранных.
        let current_default = audio.get_default_endpoint().ok();
        let cycle_index = audio
            .get_favorites(&favorites)
            .into_iter()
            .filter(|d| d.state == audio::DeviceState::Active)
            .position(|d| Some(&d.id) == current_default.as_ref())
            .unwrap_or(0);

        // Создание менеджера трея.
        let tray = tray::TrayManager::new(&audio, &favorites, &current_device, cycle_index);

        // Регистрация сохранённого хоткея (если он был задан).
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

    /// Запланировать перечитывание устройств (серия событий схлопнется в одно).
    fn schedule_refresh(&mut self) {
        self.refresh_due = Some(Instant::now() + REFRESH_DEBOUNCE);
    }

    /// Выполнить отложенное обновление, если пауза после событий уже прошла.
    fn flush_due_refresh(&mut self) {
        if let Some(due) = self.refresh_due {
            if Instant::now() >= due {
                self.refresh_due = None;
                self.refresh_from_system();
            }
        }
    }

    /// Сколько ждать следующего события (мс): учитывает захват хоткея
    /// и отложенное обновление устройств.
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

    /// Перечитать состояние системы: текущее устройство, позицию в цикле,
    /// меню и тултип. Вызывается, когда устройства добавились/удалились или
    /// default сменили извне.
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

    /// Якорь для тоста: прямоугольник иконки трея, иначе точка у курсора.
    fn toast_anchor(&self) -> (i32, i32, i32, i32) {
        self.last_icon_rect.unwrap_or_else(|| {
            let mut p = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut p);
            }
            (p.x, p.y, p.x, p.y + 16)
        })
    }

    /// Синхронизировать cycle_index с реальным default-устройством.
    ///
    /// Индекс = позиция текущего default среди АКТИВНЫХ избранных;
    /// если текущее не в избранном (или не активно) — 0.
    /// Возвращает `(индекс, всего активных)` — для подписей «N из M».
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

    /// Дружелюбное имя устройства по endpoint ID (пустое — если не найдено).
    fn device_name(&self, endpoint_id: &str) -> String {
        self.audio
            .list_devices()
            .into_iter()
            .find(|d| d.id == endpoint_id)
            .map(|d| d.name)
            .unwrap_or_default()
    }

    /// Показать всплывающую подсказку о новом активном устройстве
    /// (у иконки трея, как при наведении; исчезает через 1 с).
    fn show_switch_toast(&mut self, device_name: &str, position: usize, total: usize) {
        let text = format!(
            "{}\n{}",
            device_name,
            i18n::lang().device_position(position, total)
        );
        let anchor = self.toast_anchor();
        self.toast.show_anchored(&text, anchor);
    }

    /// Короткое сообщение в тосте (без смены устройства).
    fn flash_toast(&mut self, text: &str) {
        let anchor = self.toast_anchor();
        self.toast.show_anchored(text, anchor);
    }

    /// Начать захват новой комбинации.
    ///
    /// Текущая комбинация снимается с регистрации, чтобы нажатие прежней
    /// не переключало устройство во время настройки.
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

    /// Тост захвата с анимированными точками слева от поля настройки.
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

    /// Прервать захват (Esc): вернуть прежнюю комбинацию, если она была.
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

    /// Принять новую комбинацию: регистрация, сохранение, обновление меню.
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

    /// Шаг захвата: анимация точек + реакция на новую комбинацию.
    ///
    /// Вызывается из event loop, пока `capture.is_some()`.
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
            // Esc — отмена настройки.
            if vk == 0x1B {
                self.cancel_capture();
                return;
            }

            let modifiers = hotkey::modifiers_down();
            let lang = i18n::lang();
            if modifiers == 0 {
                // Без модификатора комбинация перехватила бы обычное нажатие
                // клавиши во всех приложениях — не разрешаем.
                self.flash_toast(lang.need_modifier());
            } else {
                let hk = hotkey::Hotkey { modifiers, vk };
                match hk.register() {
                    Ok(()) => {
                        self.finish_capture(hk);
                        return;
                    }
                    Err(hotkey::HotkeyError::Conflict) => {
                        // Занята системой или другим приложением — сообщаем
                        // и продолжаем ждать другую комбинацию.
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

    /// Переключить на следующее устройство в цикле.
    fn cycle_next(&mut self) {
        // Пропускаем отключённые устройства: цикл идёт только по активным.
        let active_devices: Vec<_> = self
            .audio
            .get_favorites(&self.favorites)
            .into_iter()
            .filter(|d| d.state == audio::DeviceState::Active)
            .collect();

        // 0 устройств — делать нечего; 1 устройство — переключать некуда.
        if active_devices.len() < 2 {
            return;
        }

        // Следующий — после текущего default (позиция берётся из системы,
        // а не из хранимого индекса: пользователь мог сменить устройство
        // вручную через системную панель).
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

    /// Переключиться на конкретное устройство.
    fn switch_to(&mut self, endpoint_id: &str) {
        if let Err(e) = policy::set_default_endpoint(endpoint_id) {
            log::error!("Failed to set default endpoint: {}", e);
            return;
        }

        self.current_device = endpoint_id.to_string();
        let (pos, total) = self.sync_cycle_index();
        let device_name = self.device_name(endpoint_id);
        self.tray.update(
            &self.audio,
            &self.favorites,
            &self.current_device,
            self.cycle_index,
        );
        self.show_switch_toast(&device_name, pos + 1, total);
    }

    /// Добавить устройство в избранное (клик по пункту «+ имя» в меню).
    fn add_favorite(&mut self, endpoint_id: &str) {
        if !self.favorites.devices.iter().any(|id| id == endpoint_id) {
            self.favorites.devices.push(endpoint_id.to_string());

            // Сохраняем конфиг при каждом изменении избранного (требование AGENTS.md).
            if let Err(e) = config::save_favorites(&self.favorites) {
                log::error!("Failed to save favorites: {}", e);
            }
        }

        self.tray.update(
            &self.audio,
            &self.favorites,
            &self.current_device,
            self.cycle_index,
        );
    }
}

fn main() {
    // Инициализация логирования (debug-сборка — консольная, логи видны).
    #[cfg(debug_assertions)]
    env_logger::init();

    // CLI-режим диагностики (--list / --switch <N>): до мьютекса и трея.
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        std::process::exit(cli_mode(&args));
    }

    // COM-инициализация (STA): обязана быть до любых COM-вызовов.
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if hr.is_err() {
        log::error!("Failed to initialize COM: 0x{:08x}", hr.0);
        return;
    }
    // RAII-гард: CoUninitialize вызовется при выходе из main (после drop AppState).
    let _com_guard = ComUninitializer;

    // Single-instance: именованный мьютекс.
    unsafe {
        // HANDLE в windows-rs не имеет Drop и не закрывается при дропе:
        // хендл остаётся открытым до завершения процесса, поэтому
        // именованный мьютекс-объект живёт всё время работы приложения.
        let _mutex_handle = match CreateMutexW(None, true, INSTANCE_MUTEX_NAME) {
            Ok(h) => h,
            Err(e) => {
                log::error!("Failed to create mutex: {}", e);
                return;
            }
        };

        // GetLastError() обязан вызываться сразу после CreateMutexW.
        if GetLastError() == ERROR_ALREADY_EXISTS {
            log::info!("Another instance is already running.");
            return;
        }
    }

    // Создание состояния приложения.
    let mut state = AppState::new();

    // Запуск event loop.
    run_event_loop(&mut state);
}

/// Основная event loop: message loop + обработка событий трея и меню.
///
/// tray-icon и muda доставляют события через глобальные каналы
/// (`TrayIconEvent::receiver()` / `MenuEvent::receiver()`), которые
/// наполняются из оконных процедур их скрытых окон — поэтому каналы
/// надо опустошать после каждого прогона message loop.
fn run_event_loop(state: &mut AppState) {
    loop {
        // 1. Прокачиваем все накопленные сообщения.
        //    WM_HOTKEY приходит в очередь потока (RegisterHotKey с hwnd = NULL) —
        //    это и есть срабатывание глобальной комбинации из любого приложения.
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
                    // Колбэк уже выставил флаг; сообщение лишь разбудило нас.
                    audio::take_refresh_request();
                    state.schedule_refresh();
                    continue;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        // 2. Клики по самой иконке. Любой клик несёт координаты иконки —
        // кэшируем прямоугольник для тоста при переключении через меню.
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            let rect = event.icon_rect;
            state.last_icon_rect = Some((
                rect.left as i32,
                rect.top as i32,
                rect.right as i32,
                rect.bottom as i32,
            ));

            // Двойной клик Windows присылает как `Double` + два `Left`,
            // поэтому обрабатываем только `Left` и гасим серию по времени.
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

        // 3. Пункты меню. «Выход» — прерываем цикл.
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if dispatch_menu_event(state, &event.id.0) {
                return;
            }
        }

        // 4. Устройства добавились/удалились или default сменили извне.
        if audio::take_refresh_request() {
            state.schedule_refresh();
        }
        state.flush_due_refresh();

        // 5. Захват хоткея (если идёт): анимация точек и чтение клавиш.
        state.tick_capture();

        // Ждём следующего сообщения вместо `sleep`: Windows будит поток,
        // как только появляется ввод (клик, WM_HOTKEY, COM-уведомление).
        // Таймаут нужен для опроса клавиатуры при захвате и для отложенного
        // обновления устройств; в остальное время спим бесконечно.
        let timeout = state.wait_timeout_ms();
        unsafe {
            MsgWaitForMultipleObjectsEx(None, timeout, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
        }
    }
}

/// Обработать событие меню по ID пункта.
///
/// Возвращает `true`, если запрошен выход из приложения.
///
/// Схема ID (см. `tray::build_ui`):
/// - `"cycle"` — циклическое переключение;
/// - `"hotkey"` — начать захват глобальной горячей клавиши;
/// - `"exit"` — выход;
/// - `"fav_<endpoint_id>"` — переключиться на избранное устройство;
/// - `"add_<endpoint_id>"` — добавить устройство в избранное.
fn dispatch_menu_event(state: &mut AppState, id: &str) -> bool {
    match id {
        "cycle" => state.cycle_next(),
        "hotkey" => state.start_capture(),
        "exit" => return true,
        "empty" => {} // пункт-подсказка, отключён — событий не должно приходить
        _ if id.starts_with("fav_") => state.switch_to(&id["fav_".len()..]),
        _ if id.starts_with("add_") => state.add_favorite(&id["add_".len()..]),
        _ => log::warn!("Unknown menu item id: {}", id),
    }
    false
}

/// CLI-режим диагностики: проверить перечисление и смену default-устройства
/// напрямую, без трея и event loop.
///
/// ```text
/// SwitcherProgramm.exe --list          # список устройств, маркер текущего
/// SwitcherProgramm.exe --switch <N>    # сделать устройство N устройством по умолчанию
/// ```
///
/// Отдельная COM-инициализация: режим вызывается ДО основной инициализации main.
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
                println!("[{}] (state: {:?}) {}{}", i, d.state, d.name, marker);
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
            // Диагностика тоста: показываем у нижней кромки экрана (как иконка
            // в трее — главный реальный сценарий), проверяем, что окно видимо
            // и целиком внутри экрана, затем что таймер его спрятал.
            use std::time::{Duration, Instant};

            let mut toast = toast::Toast::new();
            let (sw, sh) =
                unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };

            // Имитация иконки в трее: маленький квадрат у нижней кромки.
            let anchor = (sw - 60, sh - 48, sw - 20, sh - 8);
            toast.show_anchored("Тест тоста\nУстройство: 1 из 2", anchor);
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

            // Качаем сообщения 1.5 с: WM_TIMER должен деактивировать тултип.
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
            // Проверка механики хоткея без ручного участия:
            //  1) системные комбинации должны отклоняться (Conflict);
            //  2) Ctrl+Shift+U должен регистрироваться и доставлять WM_HOTKEY
            //     (нажатие синтезируется SendInput).
            use windows::Win32::UI::Input::KeyboardAndMouse::{
                SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
                KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL, VK_SHIFT,
            };
            use windows::Win32::UI::WindowsAndMessaging::WM_HOTKEY;

            let mut failed = false;

            // 1. Заведомо системные комбинации.
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

            // 2. Своя комбинация: Ctrl + Shift + U.
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

            // Синтез нажатия: Ctrl↓ Shift↓ U↓ U↑ Shift↑ Ctrl↑.
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

            // Ждём WM_HOTKEY.
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
            // Диагностика локализации: какой язык выбран и как выглядят строки.
            let lang = i18n::lang();
            println!("lang: {:?}", lang);
            println!("menu cycle:      {}", lang.switch_item(1, 3));
            println!("menu hotkey:     {}", lang.hotkey_item("Ctrl + Shift + U"));
            println!("menu hotkey unset: {}", lang.hotkey_unset());
            println!("menu favorites:  {}", lang.favorites_label());
            println!("menu exit:       {}", lang.exit_label());
            println!("toast:           {}", lang.device_position(1, 3));
            0
        }
        _ => {
            eprintln!("Usage: SwitcherProgramm.exe --list | --switch <index> | --toast-test | --hotkey-test | --lang");
            2
        }
    }
}

/// RAII-гард для CoUninitialize.
struct ComUninitializer;

impl Drop for ComUninitializer {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}
