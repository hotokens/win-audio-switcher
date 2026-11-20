//! UI localization.
//!
//! The language is chosen from the Windows UI language (`GetUserDefaultUILanguage`):
//! Russian for the Russian locale, English for everything else.
//! Strings are collected in one place so they are not scattered across the code.

use std::sync::OnceLock;

use windows::Win32::Globalization::GetUserDefaultUILanguage;

/// UI language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Ru,
}

impl Lang {
    /// Determine the language from a LANGID (the low 10 bits are the primary language ID).
    ///
    /// Pure function — easy to test without Win32.
    pub fn from_langid(langid: u16) -> Self {
        const LANG_RUSSIAN: u16 = 0x19;
        if langid & 0x3FF == LANG_RUSSIAN {
            Self::Ru
        } else {
            Self::En
        }
    }

    /// Menu item “Switch (N of M)”.
    pub fn switch_item(self, position: usize, total: usize) -> String {
        match self {
            Self::En => format!("Switch ({} of {})", position, total),
            Self::Ru => format!("Переключить ({} из {})", position, total),
        }
    }

    /// Hotkey setting name — “Hotkey”.
    pub fn hotkey_label(self) -> &'static str {
        match self {
            Self::En => "Hotkey",
            Self::Ru => "Hot-key переключатель",
        }
    }

    /// Menu item with the current combination: “Hotkey: Ctrl + Shift + U”.
    pub fn hotkey_item(self, combination: &str) -> String {
        format!("{}: {}", self.hotkey_label(), combination)
    }

    /// Menu item when no combination is set.
    pub fn hotkey_unset(self) -> String {
        match self {
            Self::En => format!("{}: not set", self.hotkey_label()),
            Self::Ru => format!("{}: не задан", self.hotkey_label()),
        }
    }

    /// Submenu of favourite devices.
    pub fn favorites_label(self) -> &'static str {
        match self {
            Self::En => "Favorites",
            Self::Ru => "Избранное",
        }
    }

    /// Hint when favourites are empty.
    pub fn empty_hint(self) -> &'static str {
        match self {
            Self::En => "Add devices to favorites",
            Self::Ru => "Добавьте устройства в избранное",
        }
    }

    /// Exit item.
    pub fn exit_label(self) -> &'static str {
        match self {
            Self::En => "Exit",
            Self::Ru => "Выход",
        }
    }

    /// Device position line: “Device: 1 of 3”.
    pub fn device_position(self, position: usize, total: usize) -> String {
        match self {
            Self::En => format!("Device: {} of {}", position, total),
            Self::Ru => format!("Устройство: {} из {}", position, total),
        }
    }

    /// Hint while capturing a combination.
    pub fn capture_hint(self) -> &'static str {
        match self {
            Self::En => "press a key combination…",
            Self::Ru => "нажмите комбинацию…",
        }
    }

    /// A combination without a modifier is not allowed.
    pub fn need_modifier(self) -> &'static str {
        match self {
            Self::En => "Add Ctrl, Alt, Shift or Win",
            Self::Ru => "Добавьте Ctrl, Alt, Shift или Win",
        }
    }

    /// The combination is taken by the system or another application.
    pub fn hotkey_conflict(self) -> &'static str {
        match self {
            Self::En => "Combination is taken by Windows or another app",
            Self::Ru => "Комбинация занята системой или другим приложением",
        }
    }

    /// Other registration errors.
    pub fn hotkey_failed(self, reason: &str) -> String {
        match self {
            Self::En => format!("Cannot register: {}", reason),
            Self::Ru => format!("Не удалось зарегистрировать: {}", reason),
        }
    }

    /// Capture cancelled — the previous combination was kept.
    pub fn hotkey_unchanged(self) -> &'static str {
        match self {
            Self::En => "Hotkey not changed",
            Self::Ru => "Hot-key не изменён",
        }
    }

    /// Capture cancelled and there was no combination.
    pub fn hotkey_not_set(self) -> &'static str {
        match self {
            Self::En => "Hotkey not set",
            Self::Ru => "Hot-key не задан",
        }
    }

    /// Version line in the menu: “Version 0.1.0+build.42”.
    pub fn version_label(self, version: &str) -> String {
        match self {
            Self::En => format!("Version {}", version),
            Self::Ru => format!("Версия {}", version),
        }
    }
}

static LANG: OnceLock<Lang> = OnceLock::new();

/// UI language of the current user (determined once).
///
/// The `SWITCHER_LANG=en|ru` environment variable overrides the system language —
/// used to check localization without changing the Windows language.
pub fn lang() -> Lang {
    *LANG.get_or_init(|| {
        if let Ok(value) = std::env::var("SWITCHER_LANG") {
            match value.trim().to_ascii_lowercase().as_str() {
                "en" => {
                    log::info!("UI language forced by SWITCHER_LANG: En");
                    return Lang::En;
                }
                "ru" => {
                    log::info!("UI language forced by SWITCHER_LANG: Ru");
                    return Lang::Ru;
                }
                _ => log::warn!("Unknown SWITCHER_LANG value: {:?}", value),
            }
        }
        // SAFETY: GetUserDefaultUILanguage has no preconditions.
        let langid = unsafe { GetUserDefaultUILanguage() };
        let lang = Lang::from_langid(langid);
        log::info!("UI language: {:?} (LANGID 0x{:04x})", lang, langid);
        lang
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn russian_langid_is_detected() {
        // 0x0419 — ru-RU.
        assert_eq!(Lang::from_langid(0x0419), Lang::Ru);
    }

    #[test]
    fn other_langids_fall_back_to_english() {
        assert_eq!(Lang::from_langid(0x0409), Lang::En); // en-US
        assert_eq!(Lang::from_langid(0x0407), Lang::En); // de-DE
        assert_eq!(Lang::from_langid(0x0000), Lang::En);
    }

    #[test]
    fn formatted_strings() {
        assert_eq!(Lang::En.switch_item(1, 3), "Switch (1 of 3)");
        assert_eq!(Lang::Ru.switch_item(1, 3), "Переключить (1 из 3)");
        assert_eq!(Lang::En.device_position(2, 4), "Device: 2 of 4");
        assert_eq!(Lang::En.hotkey_item("Ctrl + U"), "Hotkey: Ctrl + U");
        assert_eq!(Lang::Ru.hotkey_unset(), "Hot-key переключатель: не задан");
        assert_eq!(
            Lang::En.version_label("0.1.0+build.7"),
            "Version 0.1.0+build.7"
        );
        assert_eq!(
            Lang::Ru.version_label("0.1.0+build.7"),
            "Версия 0.1.0+build.7"
        );
    }
}
