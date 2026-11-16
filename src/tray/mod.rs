//! Управление иконкой в системном трее и контекстным меню.

use std::collections::HashMap;

use tray_icon::menu::accelerator::Accelerator;
use tray_icon::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::TrayIconBuilder;

use crate::audio::{AudioDevice, AudioManager, FormFactor};
use crate::config::Favorites;
use crate::i18n;

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

    /// Символ-иконка по типу устройства (в духе значков в настройках Windows).
    ///
    /// `muda` не умеет рисовать настоящие иконки в пунктах меню, поэтому тип
    /// устройства показывается эмодзи-символом перед названием.
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

    /// Текст тултипа при наведении на иконку трея.
    ///
    /// Лимит Shell: `szTip` в `NOTIFYICONDATAW` отображается максимум
    /// в 64 UTF-16 символах — более длинный текст Shell молча обрезает
    /// посреди строки. Поэтому формат компактный, а имя устройства
    /// урезается с многоточием так, чтобы всё уложилось в лимит.
    fn build_tooltip_text(device_name: &str, position: usize, total: usize) -> String {
        const MAX_CHARS: usize = 63; // 64-й символ занят нулевым терминатором

        let first = i18n::lang().device_position(position, total);
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
            let cycle_label = i18n::lang().switch_item(cycle_index + 1, fav_devices.len());
            let cycle = MenuItem::with_id("cycle", cycle_label, true, None::<Accelerator>);
            let _ = menu.append(&cycle);
        } else {
            let _ = menu.append(&PredefinedMenuItem::separator());
            // Пункт-подсказка: отключён, чтобы не «кликать в пустоту».
            let note = MenuItem::with_id(
                "empty",
                i18n::lang().empty_hint(),
                false,
                None::<Accelerator>,
            );
            let _ = menu.append(&note);
        }

        // Пункт «Hotkey» — клик запускает захват комбинации.
        let hotkey_label = match &favorites.hotkey {
            Some(hk) => i18n::lang().hotkey_item(&hk.display()),
            None => i18n::lang().hotkey_unset(),
        };
        let hotkey_item = MenuItem::with_id("hotkey", hotkey_label, true, None::<Accelerator>);
        let _ = menu.append(&hotkey_item);

        // Разделитель.
        let _ = menu.append(&PredefinedMenuItem::separator());

        // Подменю «Избранное»: все устройства списком, галочка = в избранном.
        // Клик по пункту добавляет/убирает устройство (переключение — через
        // верхний пункт, иконку трея или хоткей).
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

        // Разделитель.
        let _ = menu.append(&PredefinedMenuItem::separator());

        // Версия приложения: неактивная строка над «Выход».
        let version_item = MenuItem::with_id(
            "version",
            i18n::lang().version_label(&crate::version::full()),
            false,
            None::<Accelerator>,
        );
        let _ = menu.append(&version_item);

        // «Выход»: завершает приложение (обрабатывается в event loop по id "exit").
        let exit = MenuItem::with_id("exit", i18n::lang().exit_label(), true, None::<Accelerator>);
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
