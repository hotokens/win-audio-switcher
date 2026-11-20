//! Popup hint shown when the output device is switched.
//!
//! The same Win32 mechanism as the system tooltip shown when hovering over a
//! tray icon: a window of class `tooltips_class32`, shown through
//! `TTM_TRACKACTIVATE` + `TTM_TRACKPOSITION` in
//! `TTF_TRACK | TTF_ABSOLUTE` mode (it appears exactly at the given screen point).
//!
//! Auto-hide via `SetTimer` (1 s): the timer sends `WM_TIMER` to our hidden
//! owner window, and in its `WndProc` we deactivate the tooltip.

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

/// Auto-hide timer ID.
const TIMER_ID: usize = 1;
/// How long the hint stays visible.
const DURATION_MS: u32 = 1000;
/// ID of the "tool" inside the tooltip (arbitrary; we have one tool for the whole lifetime).
const TOOL_ID: usize = 1;
/// Size of the UTF-16 text buffer (single buffer, reused).
const MAX_TEXT: usize = 512;
/// Tooltip width for line wrapping (in pixels).
const MAX_TIP_WIDTH: i32 = 320;
/// Offset of the hint from the icon/cursor.
const TOAST_GAP: i32 = 6;
/// Size of the V2 `TOOLINFO` structure — without the trailing `lpReserved` field.
///
/// With comctl32 v5 (no manifest) the full V3 size is rejected: `TTM_ADDTOOLW`
/// returns FALSE and the tooltip never appears. After the v6 manifest is
/// enabled the full size is accepted, but V2 is compatible with both comctl32
/// versions — so we keep it. The V2 size = full size minus one pointer and is
/// correct on any bitness.
const TOOLINFO_SIZE: u32 =
    (std::mem::size_of::<TTTOOLINFOW>() - std::mem::size_of::<*mut std::ffi::c_void>()) as u32;

/// HWND of the tooltip and the owner window — needed in WndProc (the timer
/// arrives there without a reference to the structure). Everything lives on
/// the main thread; the atomics exist to satisfy the static requirement.
static TOOLTIP_HWND: AtomicUsize = AtomicUsize::new(0);
static OWNER_HWND: AtomicUsize = AtomicUsize::new(0);

/// Deactivate the tooltip (in the timer handler).
///
/// SAFETY: called only from `toast_wndproc` (main thread).
unsafe fn deactivate_tooltip() {
    let tt = TOOLTIP_HWND.load(Ordering::Relaxed);
    if tt == 0 {
        return;
    }
    let tooltip = HWND(tt as *mut _);
    let owner = HWND(OWNER_HWND.load(Ordering::Relaxed) as *mut _);

    // TTM_TRACKACTIVATE needs a toolinfo identifying the tool:
    // cbSize/uFlags/hwnd/uId; lpszText is not required.
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

/// Is the dark theme for Windows apps enabled?
///
/// Undocumented `ShouldAppsUseDarkMode` from `uxtheme.dll` (ordinal 132).
/// Any error or missing function → light theme (safe default).
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

/// Apply the system theme (dark or light) to the tooltip.
///
/// SAFETY: `tooltip` is a valid window of class `tooltips_class32`.
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
    // Windows theme changed — reapply so no restart is required.
    // (Message-only windows do not receive broadcast messages, so the theme
    // is also refreshed in `show_anchored` before every display.)
    if msg == WM_SETTINGCHANGE || msg == WM_THEMECHANGED {
        let tooltip = TOOLTIP_HWND.load(Ordering::Relaxed);
        if tooltip != 0 {
            apply_tooltip_theme(HWND(tooltip as *mut _));
        }
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// Pack screen coordinates into an `LPARAM` for `TTM_TRACKPOSITION`:
/// low word is x, high word is y (equivalent to the MAKELONG macro).
fn makelong(x: i32, y: i32) -> isize {
    (((y as u32 as u16 as u32) << 16) | (x as u32 as u16 as u32)) as isize
}

/// Work area of the monitor nearest to the point (x, y), in screen
/// coordinates: `(left, top, right, bottom)` without the taskbar.
///
/// SAFETY: a plain Win32 query with no side effects.
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
        // Fallback: the whole screen, ignoring the taskbar.
        (
            0,
            0,
            GetSystemMetrics(SM_CXSCREEN),
            GetSystemMetrics(SM_CYSCREEN),
        )
    }
}

/// Popup hint manager.
///
/// The owner window is created right away (hidden, message-only); the tooltip
/// is created lazily on first display; the text is kept in a pinned UTF-16 buffer
/// (the tooltip's `lpszText` must remain valid between messages).
pub struct Toast {
    owner: Option<HWND>,
    tooltip: Option<HWND>,
    text_buf: Option<&'static mut [u16]>,
}

impl Toast {
    /// Create the manager (registers the class + hidden owner window).
    pub fn new() -> Self {
        unsafe {
            let hinstance = match GetModuleHandleW(None) {
                Ok(h) => HINSTANCE(h.0), // HMODULE and HINSTANCE are the same pointer
                Err(e) => {
                    log::error!("Toast: GetModuleHandleW failed: {}", e);
                    return Self::empty();
                }
            };

            // The tooltips_class32 class is registered by comctl32; make sure
            // common controls are loaded.
            InitCommonControls();

            let class_name = w!("SwitcherProgramm.ToastOwner");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(toast_wndproc),
                lpszClassName: class_name,
                hInstance: hinstance,
                ..WNDCLASSW::default()
            };
            RegisterClassW(&wc);

            // Message-only window (parent HWND_MESSAGE): visible nowhere,
            // needed only as the tool owner and the WM_TIMER recipient.
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

    /// Lazily create the tooltip window and register a "tool" in it.
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

            // TTM_ADDTOOLW returns TRUE/FALSE as its result.
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
            // Allow multi-line text.
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

    /// Show the hint next to the `anchor` rectangle (left, top, right,
    /// bottom) in screen coordinates — usually the tray icon.
    ///
    /// Prefers the position below the rectangle; if there is no room there
    /// (the typical case: taskbar at the bottom edge of the screen) — above it.
    /// The final position is always clamped into the monitor work area,
    /// so the hint can never "drift" off the screen edge.
    pub fn show_anchored(&mut self, text: &str, anchor: (i32, i32, i32, i32)) {
        let Some(owner) = self.owner else { return };
        let Some(tooltip) = self.ensure_tooltip() else {
            return;
        };
        let Some(buf) = self.text_buf.as_deref_mut() else {
            return;
        };

        // Text to UTF-16 + null terminator, truncated to fit the buffer.
        let mut len = 0usize;
        for unit in text.encode_utf16().take(MAX_TEXT - 1) {
            buf[len] = unit;
            len += 1;
        }
        buf[len] = 0;

        unsafe {
            // The theme may have changed since the window was created — refresh it.
            apply_tooltip_theme(tooltip);

            let ti = TTTOOLINFOW {
                cbSize: TOOLINFO_SIZE,
                uFlags: TTF_TRACK | TTF_ABSOLUTE,
                hwnd: owner,
                uId: TOOL_ID,
                lpszText: PWSTR(buf.as_mut_ptr()),
                ..TTTOOLINFOW::default()
            };

            // Update the text.
            SendMessageW(
                tooltip,
                TTM_UPDATETIPTEXTW,
                WPARAM(0),
                LPARAM(&ti as *const _ as isize),
            );

            let (left, top, right, bottom) = anchor;
            let center_x = (left + right) / 2;

            // Activate at a temporary point — the window size is known only
            // after activation (the tooltip sizes itself to fit the text).
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

            // Measure the window and place it for real: below the anchor if it
            // fits, otherwise above it; all within the work area.
            let mut wr = RECT::default();
            if GetWindowRect(tooltip, &mut wr).is_ok() {
                let w = wr.right - wr.left;
                let h = wr.bottom - wr.top;
                let (work_left, work_top, work_right, work_bottom) = work_area_at(center_x, bottom);

                let mut x = center_x - w / 2;
                let mut y = bottom + TOAST_GAP;
                if y + h > work_bottom {
                    // Does not fit below (icon at the bottom edge) — place above the anchor.
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

            // (Re)start the auto-hide timer: SetTimer with the same ID
            // resets the previous timer.
            let timer_id = SetTimer(owner, TIMER_ID, DURATION_MS, None);
            if timer_id == 0 {
                log::error!("Toast: SetTimer failed");
            }
        }
    }

    /// Diagnostics (`--toast-test`): is the tooltip visible right now.
    pub fn debug_is_visible(&self) -> bool {
        match self.tooltip {
            Some(hwnd) => unsafe { IsWindowVisible(hwnd).as_bool() },
            None => false,
        }
    }

    /// Diagnostics (`--toast-test`): the tooltip window rectangle
    /// (left, top, right, bottom) in screen coordinates.
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
