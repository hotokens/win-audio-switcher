//! Management of the system tray icon and the context menu.

use std::collections::HashMap;

use tray_icon::menu::accelerator::Accelerator;
use tray_icon::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::TrayIconBuilder;

use crate::audio::{AudioDevice, AudioManager, FormFactor};
use crate::config::Favorites;
use crate::i18n;

/// System tray manager.
pub struct TrayManager {
    tray: Option<tray_icon::TrayIcon>,
}

impl TrayManager {
    /// Create a new TrayManager.
    pub fn new(
        audio: &AudioManager,
        favorites: &Favorites,
        current_device: &str,
        cycle_index: usize,
    ) -> Self {
        let (tooltip, menu) = Self::build_ui(audio, favorites, current_device, cycle_index);

        let tray = TrayIconBuilder::new()
            .with_icon(crate::icon::get_icon())
            // By default the left click opens the menu; we need left click = switch,
            // and the menu only on right click.
            .with_menu_on_left_click(false)
            .with_menu(Box::new(menu))
            .with_tooltip(tooltip.as_str())
            .build()
            .ok();

        Self { tray }
    }

    /// Rebuild the context menu and update the tray.
    pub fn update(
        &mut self,
        audio: &AudioManager,
        favorites: &Favorites,
        current_device: &str,
        cycle_index: usize,
    ) {
        let (tooltip, menu) = Self::build_ui(audio, favorites, current_device, cycle_index);

        if let Some(ref tray) = self.tray {
            // set_menu returns no Result — the rebuilt menu replaces the previous one entirely.
            tray.set_menu(Some(Box::new(menu)));
            let _ = tray.set_tooltip(Some(tooltip.as_str()));
        }
    }

    /// Symbol-icon for the device type (in the spirit of the Windows settings icons).
    ///
    /// `muda` cannot draw real icons in menu items, so the device type is shown
    /// as an emoji symbol before the name.
    fn device_symbol(form_factor: FormFactor) -> &'static str {
        match form_factor {
            FormFactor::Speakers => "🔊",
            FormFactor::Headphones => "🎧",
            FormFactor::DisplayPort => "🖥",
            FormFactor::Digital => "🔈",
            FormFactor::LineLevel => "🔉",
            FormFactor::Remote => "🌐",
            FormFactor::Other => "🔊",
        }
    }

    /// Tooltip text shown when hovering over the tray icon.
    ///
    /// Shell limit: `szTip` in `NOTIFYICONDATAW` displays at most
    /// 64 UTF-16 characters — a longer text is silently truncated by the Shell
    /// mid-string. That is why the format is compact and the device name
    /// is trimmed with an ellipsis so that everything fits the limit.
    fn build_tooltip_text(device_name: &str, position: usize, total: usize) -> String {
        const MAX_CHARS: usize = 63; // the 64th character is taken by the null terminator

        let first = i18n::lang().device_position(position, total);
        // +1 for '\n' between the lines.
        let name_budget = MAX_CHARS.saturating_sub(first.chars().count() + 1);
        if name_budget == 0 {
            // Exotic case: the first line itself is longer than the limit.
            return first.chars().take(MAX_CHARS).collect();
        }
        let name_len = device_name.chars().count();
        if name_len <= name_budget {
            return format!("{}\n{}", first, device_name);
        }
        // Truncate the name, leaving room for the ellipsis.
        let mut name: String = device_name.chars().take(name_budget - 1).collect();
        name.push('…');
        format!("{}\n{}", first, name)
    }

    /// Build the tooltip and the menu in a single pass (one device enumeration).
    ///
    /// Returns `(tooltip, menu)`. The tooltip gets the friendly name of the current
    /// device instead of its endpoint ID (that one is too long to read).
    fn build_ui(
        audio: &AudioManager,
        favorites: &Favorites,
        current_device: &str,
        cycle_index: usize,
    ) -> (String, Menu) {
        let menu = Menu::new();

        // A single device enumeration for the whole menu and the tooltip.
        let all_devices = audio.list_devices();
        let by_id: HashMap<String, AudioDevice> = all_devices
            .iter()
            .map(|d| (d.id.clone(), d.clone()))
            .collect();

        // Name of the current device for the tooltip (fallback — the ID itself).
        let current_name = by_id
            .get(current_device)
            .map(|d| d.name.clone())
            .unwrap_or_else(|| current_device.to_string());

        // Favourite devices in config order.
        let fav_devices: Vec<AudioDevice> = favorites
            .devices
            .iter()
            .filter_map(|id| by_id.get(id).cloned())
            .collect();

        // "Switch" item (cyclic switching).
        if !fav_devices.is_empty() {
            let cycle_label = i18n::lang().switch_item(cycle_index + 1, fav_devices.len());
            let cycle = MenuItem::with_id("cycle", cycle_label, true, None::<Accelerator>);
            let _ = menu.append(&cycle);
        } else {
            let _ = menu.append(&PredefinedMenuItem::separator());
            // Hint item: disabled so the user does not "click into the void".
            let note = MenuItem::with_id(
                "empty",
                i18n::lang().empty_hint(),
                false,
                None::<Accelerator>,
            );
            let _ = menu.append(&note);
        }

        // "Hotkey" item — a click starts capturing the combination.
        let hotkey_label = match &favorites.hotkey {
            Some(hk) => i18n::lang().hotkey_item(&hk.display()),
            None => i18n::lang().hotkey_unset(),
        };
        let hotkey_item = MenuItem::with_id("hotkey", hotkey_label, true, None::<Accelerator>);
        let _ = menu.append(&hotkey_item);

        // Separator.
        let _ = menu.append(&PredefinedMenuItem::separator());

        // "Favorites" submenu: all devices listed, a checkmark = favourite.
        // Clicking an item adds/removes the device (switching is done via
        // the top item, the tray icon or the hotkey).
        let favorites_submenu = Submenu::new(i18n::lang().favorites_label(), true);

        let existing: std::collections::HashSet<&String> = favorites.devices.iter().collect();
        let mut ordered: Vec<&AudioDevice> = fav_devices.iter().collect();
        for device in &all_devices {
            if !existing.contains(&device.id) {
                ordered.push(device);
            }
        }

        for device in ordered {
            let active_marker = if device.id == current_device {
                "▶ "
            } else {
                ""
            };
            let label = format!(
                "{} {}{}",
                Self::device_symbol(device.form_factor),
                active_marker,
                device.name
            );
            let item = CheckMenuItem::with_id(
                format!("toggle_{}", device.id),
                label,
                true,
                existing.contains(&device.id),
                None::<Accelerator>,
            );
            let _ = favorites_submenu.append(&item);
        }

        let _ = menu.append(&favorites_submenu);

        // Separator.
        let _ = menu.append(&PredefinedMenuItem::separator());

        // Application version: a disabled line above "Exit".
        let version_item = MenuItem::with_id(
            "version",
            i18n::lang().version_label(&crate::version::full()),
            false,
            None::<Accelerator>,
        );
        let _ = menu.append(&version_item);

        // "Exit": terminates the application (handled in the event loop by the "exit" id).
        let exit = MenuItem::with_id("exit", i18n::lang().exit_label(), true, None::<Accelerator>);
        let _ = menu.append(&exit);

        // Return: tooltip + menu.
        (
            Self::build_tooltip_text(
                &current_name,
                if fav_devices.is_empty() {
                    0
                } else {
                    cycle_index + 1
                },
                fav_devices.len(),
            ),
            menu,
        )
    }
}
