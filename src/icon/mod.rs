//! Иконка приложения.
//!
//! Источник один — `assets/app-icon.png` (512×512 RGBA). `build.rs` уменьшает
//! её до 32×32 для трея (файл `tray-32.rgba` в `OUT_DIR`) и собирает `.ico`
//! для ресурса exe из тех же пикселей.

use tray_icon::Icon;

/// RGBA-кадр 32×32, сгенерированный сборкой из `assets/app-icon.png`.
const TRAY_ICON_RGBA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tray-32.rgba"));
/// Сторона кадра трея.
const TRAY_ICON_SIZE: u32 = 32;

/// Иконка приложения для трея.
pub fn get_icon() -> Icon {
    Icon::from_rgba(TRAY_ICON_RGBA.to_vec(), TRAY_ICON_SIZE, TRAY_ICON_SIZE).unwrap_or_else(|_| {
        // Резерв на случай повреждённого кадра: 1×1 белый пиксель.
        Icon::from_rgba(vec![255; 4], 1, 1).unwrap()
    })
}
