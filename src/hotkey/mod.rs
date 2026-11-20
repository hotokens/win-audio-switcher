//! Global hotkey for switching the output device.
//!
//! Registration via `RegisterHotKey` with `hwnd = NULL`: Windows delivers
//! `WM_HOTKEY` to our thread's message queue, so the combination
//! triggers **from any application**, regardless of what is active,
//! and does not require a global keyboard hook.
//!
//! Availability check: before accepting a combination we actually try to
//! register it. If it is already taken by the system or another application,
//! `RegisterHotKey` returns `ERROR_HOTKEY_ALREADY_REGISTERED` — and
//! the combination is rejected.
//!
//! Capturing a combination during setup — polling `GetAsyncKeyState`: it sees
//! the key state globally (even when focus is in another window), and a short
//! setup period is enough for us.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL,
    MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};

/// Hotkey identifier in `RegisterHotKey` (we have only one).
const HOTKEY_ID: i32 = 1;

/// `ERROR_HOTKEY_ALREADY_REGISTERED` as an HRESULT (0x8007_0581).
const HRESULT_HOTKEY_ALREADY_REGISTERED: u32 = 0x8007_0581;

/// Hotkey: a set of modifiers + a virtual key code.
///
/// The modifiers are a bit mask of Win32 `MOD_*` (`MOD_CONTROL` = 2,
/// `MOD_SHIFT` = 4, `MOD_ALT` = 1, `MOD_WIN` = 8). Stored in the config
/// as numbers — so the combination survives any code changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hotkey {
    /// Bit mask `MOD_CONTROL | MOD_SHIFT | MOD_ALT | MOD_WIN`.
    pub modifiers: u32,
    /// Virtual key code (`VK_*`, for example 0x55 — `U`).
    pub vk: u32,
}

/// Hotkey registration errors.
#[derive(Debug, Error)]
pub enum HotkeyError {
    #[error("combination is taken by Windows or another app")]
    Conflict,
    #[error("registration failed: {0}")]
    Os(String),
}

impl Hotkey {
    /// Register the combination globally.
    ///
    /// The old combination with the same ID is first unregistered (`UnregisterHotKey`).
    /// `MOD_NOREPEAT` disables auto-repeat while a key is held —
    /// without it switching would fire many times in a row.
    pub fn register(&self) -> Result<(), HotkeyError> {
        unsafe {
            let _ = UnregisterHotKey(None, HOTKEY_ID);
            let modifiers = HOT_KEY_MODIFIERS(self.modifiers | MOD_NOREPEAT.0);
            match RegisterHotKey(None, HOTKEY_ID, modifiers, self.vk) {
                Ok(()) => Ok(()),
                Err(e) => {
                    if e.code().0 as u32 == HRESULT_HOTKEY_ALREADY_REGISTERED {
                        Err(HotkeyError::Conflict)
                    } else {
                        Err(HotkeyError::Os(e.message()))
                    }
                }
            }
        }
    }

    /// Unregister (if it was registered).
    pub fn unregister_all() {
        unsafe {
            let _ = UnregisterHotKey(None, HOTKEY_ID);
        }
    }

    /// Human-readable form: "Ctrl + Shift + U".
    pub fn display(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        // Order as in the Windows UI: Win, Ctrl, Alt, Shift.
        if self.modifiers & MOD_WIN.0 != 0 {
            parts.push("Win");
        }
        if self.modifiers & MOD_CONTROL.0 != 0 {
            parts.push("Ctrl");
        }
        if self.modifiers & MOD_ALT.0 != 0 {
            parts.push("Alt");
        }
        if self.modifiers & MOD_SHIFT.0 != 0 {
            parts.push("Shift");
        }
        let name = key_name(self.vk);
        if parts.is_empty() {
            name.to_string()
        } else {
            format!("{} + {}", parts.join(" + "), name)
        }
    }
}

/// Is the key pressed right now? (high bit of `GetAsyncKeyState`).
fn is_down(vk: u16) -> bool {
    unsafe { (GetAsyncKeyState(vk as i32) as u16) & 0x8000 != 0 }
}

/// Current modifiers as a `MOD_*` bit mask.
pub fn modifiers_down() -> u32 {
    let mut modifiers = 0u32;
    if is_down(VK_CONTROL.0) {
        modifiers |= MOD_CONTROL.0;
    }
    if is_down(VK_SHIFT.0) {
        modifiers |= MOD_SHIFT.0;
    }
    if is_down(VK_MENU.0) {
        modifiers |= MOD_ALT.0;
    }
    if is_down(VK_LWIN.0) || is_down(VK_RWIN.0) {
        modifiers |= MOD_WIN.0;
    }
    modifiers
}

/// Keys that make sense as a hotkey at all.
///
/// Modifiers (`VK_SHIFT`, `VK_CONTROL`, `VK_MENU`, `VK_LWIN`, `VK_RWIN`)
/// are deliberately excluded here: they are part of the combination, not its key.
fn candidate_keys() -> impl Iterator<Item = u32> {
    // A-Z, 0-9, F1-F24 plus commonly used named keys.
    const EXTRA: &[u32] = &[
        0x08, // Backspace
        0x09, // Tab
        0x0D, // Enter
        0x13, // Pause
        0x14, // Caps Lock
        0x1B, // Escape
        0x20, // Space
        0x21, // Page Up
        0x22, // Page Down
        0x23, // End
        0x24, // Home
        0x25, // ←
        0x26, // ↑
        0x27, // →
        0x28, // ↓
        0x2C, // Print Screen
        0x2D, // Insert
        0x2E, // Delete
        0x6A, // Num *
        0x6B, // Num +
        0x6D, // Num -
        0x6E, // Num .
        0x6F, // Num /
        0xBA, // ;
        0xBB, // =
        0xBC, // ,
        0xBD, // -
        0xBE, // .
        0xBF, // /
        0xC0, // `
        0xDB, // [
        0xDC, // \
        0xDD, // ]
        0xDE, // '
    ];
    (0x41..=0x5A)
        .chain(0x30..=0x39)
        .chain(0x70..=0x87)
        .chain(EXTRA.iter().copied())
}

/// All "non-modifier" keys pressed right now.
pub fn down_keys() -> Vec<u32> {
    candidate_keys().filter(|&vk| is_down(vk as u16)).collect()
}

/// Difference between two polls: keys pressed during the latest interval.
pub fn newly_pressed(down: &[u32], prev: &HashSet<u32>) -> Option<u32> {
    down.iter().copied().find(|vk| !prev.contains(vk))
}

/// Key name by virtual code, for display in the UI.
pub fn key_name(vk: u32) -> &'static str {
    match vk {
        0x08 => "Backspace",
        0x09 => "Tab",
        0x0D => "Enter",
        0x13 => "Pause",
        0x14 => "CapsLock",
        0x1B => "Esc",
        0x20 => "Space",
        0x21 => "PageUp",
        0x22 => "PageDown",
        0x23 => "End",
        0x24 => "Home",
        0x25 => "←",
        0x26 => "↑",
        0x27 => "→",
        0x28 => "↓",
        0x2C => "PrintScreen",
        0x2D => "Insert",
        0x2E => "Delete",
        0x6A => "Num *",
        0x6B => "Num +",
        0x6D => "Num -",
        0x6E => "Num .",
        0x6F => "Num /",
        0xBA => ";",
        0xBB => "=",
        0xBC => ",",
        0xBD => "-",
        0xBE => ".",
        0xBF => "/",
        0xC0 => "`",
        0xDB => "[",
        0xDC => "\\",
        0xDD => "]",
        0xDE => "'",
        0x30..=0x39 => match vk {
            0x30 => "0",
            0x31 => "1",
            0x32 => "2",
            0x33 => "3",
            0x34 => "4",
            0x35 => "5",
            0x36 => "6",
            0x37 => "7",
            0x38 => "8",
            _ => "9",
        },
        0x41..=0x5A => match vk {
            0x41 => "A",
            0x42 => "B",
            0x43 => "C",
            0x44 => "D",
            0x45 => "E",
            0x46 => "F",
            0x47 => "G",
            0x48 => "H",
            0x49 => "I",
            0x4A => "J",
            0x4B => "K",
            0x4C => "L",
            0x4D => "M",
            0x4E => "N",
            0x4F => "O",
            0x50 => "P",
            0x51 => "Q",
            0x52 => "R",
            0x53 => "S",
            0x54 => "T",
            0x55 => "U",
            0x56 => "V",
            0x57 => "W",
            0x58 => "X",
            0x59 => "Y",
            _ => "Z",
        },
        0x70..=0x87 => match vk {
            0x70 => "F1",
            0x71 => "F2",
            0x72 => "F3",
            0x73 => "F4",
            0x74 => "F5",
            0x75 => "F6",
            0x76 => "F7",
            0x77 => "F8",
            0x78 => "F9",
            0x79 => "F10",
            0x7A => "F11",
            0x7B => "F12",
            0x7C => "F13",
            0x7D => "F14",
            0x7E => "F15",
            0x7F => "F16",
            0x80 => "F17",
            0x81 => "F18",
            0x82 => "F19",
            0x83 => "F20",
            0x84 => "F21",
            0x85 => "F22",
            0x86 => "F23",
            _ => "F24",
        },
        _ => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_ctrl_shift_u() {
        let hk = Hotkey {
            modifiers: MOD_CONTROL.0 | MOD_SHIFT.0,
            vk: 0x55,
        };
        assert_eq!(hk.display(), "Ctrl + Shift + U");
    }

    #[test]
    fn display_win_key() {
        let hk = Hotkey {
            modifiers: MOD_WIN.0 | MOD_ALT.0,
            vk: 0x70,
        };
        assert_eq!(hk.display(), "Win + Alt + F1");
    }

    #[test]
    fn newly_pressed_detects_edge() {
        let prev: HashSet<u32> = [0x41u32, 0x42].into_iter().collect();
        assert_eq!(newly_pressed(&[0x41, 0x42, 0x43], &prev), Some(0x43));
        assert_eq!(newly_pressed(&[0x41, 0x42], &prev), None);
    }

    #[test]
    fn key_names_cover_main_ranges() {
        assert_eq!(key_name(0x55), "U");
        assert_eq!(key_name(0x31), "1");
        assert_eq!(key_name(0x7B), "F12");
        assert_eq!(key_name(0x20), "Space");
    }
}
