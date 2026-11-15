//! Управление иконкой в системном трее и контекстным меню.

use std::collections::HashMap;

use tray_icon::menu::accelerator::Accelerator;
use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::TrayIconBuilder;

use crate::audio::{AudioDevice, AudioManager};
use crate::config::Favorites;

/// Менеджер системного трея.
pub struct TrayManager {
    tray: Option<tray_icon::TrayIcon>,
}

impl TrayManager {
    /// Создать новый TrayManager.
    pub fn new(
        audio: &AudioManager,
        favorites: &Favorites,
        current_device: &str,
        cycle_index: usize,
    ) -> Self {
        let (tooltip, menu) = Self::build_ui(audio, favorites, current_device, cycle_index);

        let tray = TrayIconBuilder::new()
            .with_icon(crate::icon::get_icon())
            // По умолчанию ЛКМ открывает меню; нам нужен ЛКМ = переключение,
            // меню — только по ПКМ.
            .with_menu_on_left_click(false)
            .with_menu(Box::new(menu))
            .with_tooltip(tooltip.as_str())
            .build()
            .ok();

        Self { tray }
    }

    /// Перестроить контекстное меню и обновить трей.
    pub fn update(
        &mut self,
        audio: &AudioManager,
        favorites: &Favorites,
        current_device: &str,
        cycle_index: usize,
    ) {
        let (tooltip, menu) = Self::build_ui(audio, favorites, current_device, cycle_index);

        if let Some(ref tray) = self.tray {
            // set_menu не возвращает Result — перестроенное меню заменяется целиком.
            tray.set_menu(Some(Box::new(menu)));
            let _ = tray.set_tooltip(Some(tooltip.as_str()));
        }
    }

    /// Текст тултипа при наведении на иконку трея.
    ///
    /// Лимит Shell: `szTip` в `NOTIFYICONDATAW` отображается максимум
    /// в 64 UTF-16 символах — более длинный текст Shell молча обрезает
    /// посреди строки. Поэтому формат компактный, а имя устройства
    /// урезается с многоточием так, чтобы всё уложилось в лимит.
    fn build_tooltip_text(device_name: &str, position: usize, total: usize) -> String {
        const MAX_CHARS: usize = 63; // 64-й символ занят нулевым терминатором

        let first = format!("Устройство: {} из {}", position, total);
        // +1 на '\n' между строками.
        let name_budget = MAX_CHARS.saturating_sub(first.chars().count() + 1);
        if name_budget == 0 {
            // Экзотический случай: первая строка сама длиннее лимита.
            return first.chars().take(MAX_CHARS).collect();
        }
        let name_len = device_name.chars().count();
        if name_len <= name_budget {
            return format!("{}\n{}", first, device_name);
        }
        // Обрезаем имя, оставляя символ под многоточие.
        let mut name: String = device_name.chars().take(name_budget - 1).collect();
        name.push('…');
        format!("{}\n{}", first, name)
    }

    /// Построить тултип и меню за один проход (одно перечисление устройств).
    ///
    /// Возвращает `(тултип, меню)`. В тултип идёт дружелюбное имя текущего
    /// устройства, а не его endpoint ID (тот слишком длинный для чтения).
    fn build_ui(
        audio: &AudioManager,
        favorites: &Favorites,
        current_device: &str,
        cycle_index: usize,
    ) -> (String, Menu) {
        let menu = Menu::new();

        // Одно перечисление устройств на всё меню и на тултип.
        let all_devices = audio.list_devices();
        let by_id: HashMap<String, AudioDevice> = all_devices
            .iter()
            .map(|d| (d.id.clone(), d.clone()))
            .collect();

        // Имя текущего устройства для тултипа (fallback — сам ID).
        let current_name = by_id
            .get(current_device)
            .map(|d| d.name.clone())
            .unwrap_or_else(|| current_device.to_string());

        // Избранные устройства в порядке конфига.
        let fav_devices: Vec<AudioDevice> = favorites
            .devices
            .iter()
            .filter_map(|id| by_id.get(id).cloned())
            .collect();

        // Пункт "Переключить" (циклическое переключение).
        if !fav_devices.is_empty() {
            let cycle_label = format!("Переключить ({} из {})", cycle_index + 1, fav_devices.len());
            let cycle = MenuItem::with_id("cycle", cycle_label, true, None::<Accelerator>);
            let _ = menu.append(&cycle);
        } else {
            let _ = menu.append(&PredefinedMenuItem::separator());
            // Пункт-подсказка: отключён, чтобы не «кликать в пустоту».
            let note = MenuItem::with_id(
                "empty",
                "Добавьте устройства в избранное",
                false,
                None::<Accelerator>,
            );
            let _ = menu.append(&note);
        }

        // Пункт «Hot-key переключатель» — клик запускает захват комбинации.
        let hotkey_label = match &favorites.hotkey {
            Some(hk) => format!("Hot-key переключатель: {}", hk.display()),
            None => "Hot-key переключатель: не задан".to_string(),
        };
        let hotkey_item = MenuItem::with_id("hotkey", hotkey_label, true, None::<Accelerator>);
        let _ = menu.append(&hotkey_item);

        // Разделитель.
        let _ = menu.append(&PredefinedMenuItem::separator());

        // Подменю "Избранное": избранные (с маркером текущего) + остальные устройства.
        let favorites_submenu = Submenu::new("Избранное", true);

        for device in &fav_devices {
            let is_active = device.id == current_device;
            let label = format!("{} {}", if is_active { "▶" } else { "  " }, device.name);
            let item = MenuItem::with_id(
                format!("fav_{}", device.id),
                label,
                true,
                None::<Accelerator>,
            );
            let _ = favorites_submenu.append(&item);
        }

        // Устройства, ещё не добавленные в избранное.
        let existing: std::collections::HashSet<&String> = favorites.devices.iter().collect();
        for device in &all_devices {
            if !existing.contains(&device.id) {
                let label = format!("+ {}", device.name);
                let item = MenuItem::with_id(
                    format!("add_{}", device.id),
                    label,
                    true,
                    None::<Accelerator>,
                );
                let _ = favorites_submenu.append(&item);
            }
        }

        let _ = menu.append(&favorites_submenu);

        // Разделитель.
        let _ = menu.append(&PredefinedMenuItem::separator());

        // «Выход»: завершает приложение (обрабатывается в event loop по id "exit").
        let exit = MenuItem::with_id("exit", "Выход", true, None::<Accelerator>);
        let _ = menu.append(&exit);

        // Возврат: тултип + меню.
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
