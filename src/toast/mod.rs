//! Всплывающая подсказка при переключении устройства.
//!
//! Тот же Win32-механизм, что и системный тултип при наведении на иконку
//! в трее: окно класса `tooltips_class32`, показываемое через
//! `TTM_TRACKACTIVATE` + `TTM_TRACKPOSITION` в режиме
//! `TTF_TRACK | TTF_ABSOLUTE` (появляется ровно в заданной точке экрана).
//!
//! Авто-скрытие через `SetTimer` (1 с): таймер посылает `WM_TIMER` нашему
//! скрытому окну-владельцу, в его `WndProc` мы деактивируем тултип.

use std::sync::atomic::{AtomicUsize, Ordering};

use windows::core::{w, PCSTR, PWSTR};
use windows::Win32::Foundation::{BOOL, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Controls::{
    InitCommonControls, SetWindowTheme, TOOLTIPS_CLASS, TTF_ABSOLUTE, TTF_TRACK, TTM_ADDTOOLW,
    TTM_SETMAXTIPWIDTH, TTM_TRACKACTIVATE, TTM_TRACKPOSITION, TTM_UPDATETIPTEXTW, TTS_ALWAYSTIP,
    TTS_NOPREFIX, TTTOOLINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetSystemMetrics, GetWindowRect,
    IsWindowVisible, KillTimer, RegisterClassW, SendMessageW, SetTimer, HWND_MESSAGE, SM_CXSCREEN,
    SM_CYSCREEN, WINDOW_EX_STYLE, WINDOW_STYLE, WM_SETTINGCHANGE, WM_THEMECHANGED, WM_TIMER,
    WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

/// ID таймера авто-скрытия.
const TIMER_ID: usize = 1;
/// Сколько времени висит подсказка.
const DURATION_MS: u32 = 1000;
/// ID «tool» внутри тултипа (любое; у нас один tool на всё время жизни).
const TOOL_ID: usize = 1;
/// Размер UTF-16 буфера текста (буфер один, переиспользуется).
const MAX_TEXT: usize = 512;
/// Ширина тултипа для переноса строк (в пикселях).
const MAX_TIP_WIDTH: i32 = 320;
/// Отступ подсказки от иконки/курсора.
const TOAST_GAP: i32 = 6;
/// Размер структуры `TOOLINFO` версии V2 — без финального поля `lpReserved`.
///
/// С comctl32 v5 (без манифеста) полный размер V3 отвергается: `TTM_ADDTOOLW`
/// возвращает FALSE, и тултип никогда не появляется. После включения манифеста
/// v6 полный размер принимается, но V2 совместим с обеими версиями comctl32 —
/// поэтому оставляем его. V2-размер = полный минус один указатель и корректен
/// на любой разрядности.
const TOOLINFO_SIZE: u32 =
    (std::mem::size_of::<TTTOOLINFOW>() - std::mem::size_of::<*mut std::ffi::c_void>()) as u32;

/// HWND тултипа и окна-владельца — нужны в WndProc (таймер приходит туда
/// без ссылки на структуру). Всё живёт в главном потоке, атомики — чтобы
/// обойти требование static.
static TOOLTIP_HWND: AtomicUsize = AtomicUsize::new(0);
static OWNER_HWND: AtomicUsize = AtomicUsize::new(0);

/// Деактивировать тултип (в обработчике таймера).
///
/// SAFETY: вызывается только из `toast_wndproc` (главный поток).
unsafe fn deactivate_tooltip() {
    let tt = TOOLTIP_HWND.load(Ordering::Relaxed);
    if tt == 0 {
        return;
    }
    let tooltip = HWND(tt as *mut _);
    let owner = HWND(OWNER_HWND.load(Ordering::Relaxed) as *mut _);

    // Для TTM_TRACKACTIVATE нужен toolinfo, идентифицирующий tool:
    // cbSize/uFlags/hwnd/uId; lpszText не требуется.
    let ti = TTTOOLINFOW {
        cbSize: TOOLINFO_SIZE,
        uFlags: TTF_TRACK | TTF_ABSOLUTE,
        hwnd: owner,
        uId: TOOL_ID,
        ..TTTOOLINFOW::default()
    };
    SendMessageW(
        tooltip,
        TTM_TRACKACTIVATE,
        WPARAM(0),
        LPARAM(&ti as *const _ as isize),
    );
}

/// Тёмная тема приложений Windows включена?
///
/// Недокументированный `ShouldAppsUseDarkMode` из `uxtheme.dll` (ординал 132).
/// Любая ошибка/отсутствие функции → светлая тема (безопасный дефолт).
fn apps_use_dark_mode() -> bool {
    type ShouldAppsUseDarkMode = unsafe extern "system" fn() -> BOOL;
    unsafe {
        let Ok(module) = LoadLibraryW(w!("uxtheme.dll")) else {
            return false;
        };
        let Some(proc) = GetProcAddress(module, PCSTR(132 as *const u8)) else {
            return false;
        };
        let query: ShouldAppsUseDarkMode = std::mem::transmute(proc);
        query().as_bool()
    }
}

/// Применить к тултипу системную тему (тёмную или светлую).
///
/// SAFETY: `tooltip` — валидное окно класса `tooltips_class32`.
unsafe fn apply_tooltip_theme(tooltip: HWND) {
    let sub_app = if apps_use_dark_mode() {
        w!("DarkMode_Explorer")
    } else {
        w!("")
    };
    if let Err(e) = SetWindowTheme(tooltip, sub_app, w!("")) {
        log::debug!("Toast: SetWindowTheme failed: {}", e);
    }
}

unsafe extern "system" fn toast_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_TIMER && wparam.0 == TIMER_ID {
        deactivate_tooltip();
        let _ = KillTimer(hwnd, TIMER_ID);
        return LRESULT(0);
    }
    // Смена темы Windows — переприменяем, чтобы не требовался перезапуск.
    // (Message-only окна широковещательные сообщения не получают, поэтому
    // тема также обновляется в `show_anchored` перед каждым показом.)
    if msg == WM_SETTINGCHANGE || msg == WM_THEMECHANGED {
        let tooltip = TOOLTIP_HWND.load(Ordering::Relaxed);
        if tooltip != 0 {
            apply_tooltip_theme(HWND(tooltip as *mut _));
        }
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// Упаковать экранные координаты в `LPARAM` для `TTM_TRACKPOSITION`:
/// младшее слово — x, старшее — y (аналог макроса MAKELONG).
fn makelong(x: i32, y: i32) -> isize {
    (((y as u32 as u16 as u32) << 16) | (x as u32 as u16 as u32)) as isize
}

/// Рабочая область монитора, ближайшего к точке (x, y), в экранных
/// координатах: `(left, top, right, bottom)` без панели задач.
///
/// SAFETY: обычный запрос к Win32, без побочных эффектов.
unsafe fn work_area_at(x: i32, y: i32) -> (i32, i32, i32, i32) {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let monitor = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
    if GetMonitorInfoW(monitor, &mut info).as_bool() {
        (
            info.rcWork.left,
            info.rcWork.top,
            info.rcWork.right,
            info.rcWork.bottom,
        )
    } else {
        // Fallback: весь экран без учёта панели задач.
        (
            0,
            0,
            GetSystemMetrics(SM_CXSCREEN),
            GetSystemMetrics(SM_CYSCREEN),
        )
    }
}

/// Менеджер всплывающей подсказки.
///
/// Окно-владелец создаётся сразу (скрытое, message-only), тултип — лениво
/// при первом показе; текст хранится в закреплённом UTF-16 буфере
/// (`lpszText` тултипа обязан оставаться валидным между сообщениями).
pub struct Toast {
    owner: Option<HWND>,
    tooltip: Option<HWND>,
    text_buf: Option<&'static mut [u16]>,
}

impl Toast {
    /// Создать менеджер (регистрирует класс + скрытое окно-владелец).
    pub fn new() -> Self {
        unsafe {
            let hinstance = match GetModuleHandleW(None) {
                Ok(h) => HINSTANCE(h.0), // HMODULE и HINSTANCE — один и тот же указатель
                Err(e) => {
                    log::error!("Toast: GetModuleHandleW failed: {}", e);
                    return Self::empty();
                }
            };

            // Класс tooltips_class32 регистрируется comctl32; гарантированно
            // загружаем common controls.
            InitCommonControls();

            let class_name = w!("SwitcherProgramm.ToastOwner");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(toast_wndproc),
                lpszClassName: class_name,
                hInstance: hinstance,
                ..WNDCLASSW::default()
            };
            RegisterClassW(&wc);

            // Message-only окно (родитель HWND_MESSAGE): не видно нигде,
            // нужно только как владелец tool и получатель WM_TIMER.
            let owner = match CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class_name,
                w!(""),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                None,
                hinstance,
                None,
            ) {
                Ok(h) => h,
                Err(e) => {
                    log::error!("Toast: failed to create owner window: {}", e);
                    return Self::empty();
                }
            };

            OWNER_HWND.store(owner.0 as usize, Ordering::Relaxed);
            Self {
                owner: Some(owner),
                tooltip: None,
                text_buf: None,
            }
        }
    }

    fn empty() -> Self {
        Self {
            owner: None,
            tooltip: None,
            text_buf: None,
        }
    }

    /// Лениво создать окно тултипа и зарегистрировать в нём «tool».
    fn ensure_tooltip(&mut self) -> Option<HWND> {
        if self.tooltip.is_some() {
            return self.tooltip;
        }
        let owner = self.owner?;

        unsafe {
            let hinstance = match GetModuleHandleW(None) {
                Ok(h) => HINSTANCE(h.0),
                Err(e) => {
                    log::error!("Toast: GetModuleHandleW failed: {}", e);
                    return None;
                }
            };
            let tooltip = match CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                TOOLTIPS_CLASS,
                w!(""),
                WINDOW_STYLE(WS_POPUP.0 | TTS_ALWAYSTIP | TTS_NOPREFIX),
                0,
                0,
                0,
                0,
                None,
                None,
                hinstance,
                None,
            ) {
                Ok(h) => h,
                Err(e) => {
                    log::error!("Toast: failed to create tooltip window: {}", e);
                    return None;
                }
            };

            let buf: &'static mut [u16] = Box::leak(vec![0u16; MAX_TEXT].into_boxed_slice());
            let ti = TTTOOLINFOW {
                cbSize: TOOLINFO_SIZE,
                uFlags: TTF_TRACK | TTF_ABSOLUTE,
                hwnd: owner,
                uId: TOOL_ID,
                lpszText: PWSTR(buf.as_mut_ptr()),
                ..TTTOOLINFOW::default()
            };

            // TTM_ADDTOOLW возвращает TRUE/FALSE в своём результате.
            let added = SendMessageW(
                tooltip,
                TTM_ADDTOOLW,
                WPARAM(0),
                LPARAM(&ti as *const _ as isize),
            );
            if added.0 == 0 {
                log::error!(
                    "Toast: TTM_ADDTOOLW failed (last_os_error: {:?})",
                    std::io::Error::last_os_error()
                );
            }
            // Разрешаем многострочный текст.
            SendMessageW(
                tooltip,
                TTM_SETMAXTIPWIDTH,
                WPARAM(0),
                LPARAM(MAX_TIP_WIDTH as isize),
            );

            TOOLTIP_HWND.store(tooltip.0 as usize, Ordering::Relaxed);
            apply_tooltip_theme(tooltip);
            self.text_buf = Some(buf);
            self.tooltip = Some(tooltip);
            Some(tooltip)
        }
    }

    /// Показать подсказку рядом с прямоугольником `anchor` (left, top,
    /// right, bottom) в экранных координатах — обычно это иконка в трее.
    ///
    /// Предпочитает позицию под прямоугольником; если там нет места
    /// (типичный случай: панель задач у нижней кромки экрана) — над ним.
    /// Итоговая позиция всегда зажимается в рабочую область монитора,
    /// поэтому подсказка не может «уехать» за границу экрана.
    pub fn show_anchored(&mut self, text: &str, anchor: (i32, i32, i32, i32)) {
        let Some(owner) = self.owner else { return };
        let Some(tooltip) = self.ensure_tooltip() else {
            return;
        };
        let Some(buf) = self.text_buf.as_deref_mut() else {
            return;
        };

        // Текст в UTF-16 + нулевой терминатор, с усечением под буфер.
        let mut len = 0usize;
        for unit in text.encode_utf16().take(MAX_TEXT - 1) {
            buf[len] = unit;
            len += 1;
        }
        buf[len] = 0;

        unsafe {
            // Тема могла смениться с момента создания окна — обновляем.
            apply_tooltip_theme(tooltip);

            let ti = TTTOOLINFOW {
                cbSize: TOOLINFO_SIZE,
                uFlags: TTF_TRACK | TTF_ABSOLUTE,
                hwnd: owner,
                uId: TOOL_ID,
                lpszText: PWSTR(buf.as_mut_ptr()),
                ..TTTOOLINFOW::default()
            };

            // Обновляем текст.
            SendMessageW(
                tooltip,
                TTM_UPDATETIPTEXTW,
                WPARAM(0),
                LPARAM(&ti as *const _ as isize),
            );

            let (left, top, right, bottom) = anchor;
            let center_x = (left + right) / 2;

            // Активируем в временной точке — размер окна известен только
            // после активации (тултип сам подбирает ширину/высоту под текст).
            SendMessageW(
                tooltip,
                TTM_TRACKPOSITION,
                WPARAM(0),
                LPARAM(makelong(center_x, bottom)),
            );
            SendMessageW(
                tooltip,
                TTM_TRACKACTIVATE,
                WPARAM(1),
                LPARAM(&ti as *const _ as isize),
            );

            // Измеряем окно и ставим его по-настоящему: под якорем, если
            // помещается, иначе над ним; всё — внутри рабочей области.
            let mut wr = RECT::default();
            if GetWindowRect(tooltip, &mut wr).is_ok() {
                let w = wr.right - wr.left;
                let h = wr.bottom - wr.top;
                let (work_left, work_top, work_right, work_bottom) = work_area_at(center_x, bottom);

                let mut x = center_x - w / 2;
                let mut y = bottom + TOAST_GAP;
                if y + h > work_bottom {
                    // Снизу не помещается (иконка у нижней кромки) — над якорем.
                    y = top - h - TOAST_GAP;
                }
                x = x.clamp(work_left, (work_right - w).max(work_left));
                y = y.clamp(work_top, (work_bottom - h).max(work_top));

                SendMessageW(
                    tooltip,
                    TTM_TRACKPOSITION,
                    WPARAM(0),
                    LPARAM(makelong(x, y)),
                );
                log::debug!(
                    "Toast: shown at ({}, {}) size {}x{} (anchor {:?})",
                    x,
                    y,
                    w,
                    h,
                    anchor
                );
            }

            // (Пере)запуск таймера авто-скрытия: SetTimer с тем же ID
            // сбрасывает предыдущий таймер.
            let timer_id = SetTimer(owner, TIMER_ID, DURATION_MS, None);
            if timer_id == 0 {
                log::error!("Toast: SetTimer failed");
            }
        }
    }

    /// Диагностика (`--toast-test`): виден ли тултип прямо сейчас.
    pub fn debug_is_visible(&self) -> bool {
        match self.tooltip {
            Some(hwnd) => unsafe { IsWindowVisible(hwnd).as_bool() },
            None => false,
        }
    }

    /// Диагностика (`--toast-test`): прямоугольник окна тултипа
    /// (left, top, right, bottom) в экранных координатах.
    pub fn debug_rect(&self) -> Option<(i32, i32, i32, i32)> {
        let hwnd = self.tooltip?;
        let mut r = RECT::default();
        unsafe {
            GetWindowRect(hwnd, &mut r).ok()?;
        }
        Some((r.left, r.top, r.right, r.bottom))
    }
}

impl Drop for Toast {
    fn drop(&mut self) {
        unsafe {
            if let Some(tooltip) = self.tooltip {
                let _ = DestroyWindow(tooltip);
                TOOLTIP_HWND.store(0, Ordering::Relaxed);
            }
            if let Some(owner) = self.owner {
                let _ = KillTimer(owner, TIMER_ID);
                let _ = DestroyWindow(owner);
                OWNER_HWND.store(0, Ordering::Relaxed);
            }
        }
    }
}
