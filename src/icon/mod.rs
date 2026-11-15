//! Простая пиксельная иконка трея (16×16 RGBA): синий квадрат с белыми
//! «полосами эквалайзера» — отсылка к аудиовыводу.

use tray_icon::Icon;

/// Цвет фона (синий), RGBA.
const BG: [u8; 4] = [43, 127, 255, 255];
/// Цвет полос (белый), RGBA.
const BAR: [u8; 4] = [255, 255, 255, 255];
/// Прозрачный пиксель.
const EMPTY: [u8; 4] = [0, 0, 0, 0];

/// Пиксельная карта 16×16: '.' — прозрачный, 'b' — фон, 'w' — полоса.
/// Полосы эквалайзера разной высоты (столбцы 4, 7, 10, 13).
const GRID: [&str; 16] = [
    "................",
    "..bbbbbbbbbbbb..",
    ".bbbbbbbbbbbbbb.",
    ".bbbbbbbbbbbbbb.",
    ".bbbbbbbbbbbbbb.",
    ".bbbbbbwbbbbbbb.",
    ".bbbbbbwbbbbbbb.",
    ".bbbbbbwbbwbbbb.",
    ".bbbbbbwbbwbbwb.",
    ".bbbwbbwbbwbbwb.",
    ".bbbwbbwbbwbbwb.",
    ".bbbwbbwbbwbbwb.",
    ".bbbwbbwbbwbbwb.",
    ".bbbbbbbbbbbbbb.",
    "..bbbbbbbbbbbb..",
    "................",
];

/// Собрать RGBA-буфер из пиксельной карты.
fn rgba_from_grid() -> Vec<u8> {
    let mut rgba = Vec::with_capacity(16 * 16 * 4);
    for row in GRID {
        debug_assert_eq!(
            row.chars().count(),
            16,
            "строка пиксельной карты != 16 пикселей"
        );
        for ch in row.chars() {
            let color = match ch {
                'b' => BG,
                'w' => BAR,
                _ => EMPTY,
            };
            rgba.extend_from_slice(&color);
        }
    }
    rgba
}

/// Иконка приложения (единая для всех состояний).
pub fn get_icon() -> Icon {
    Icon::from_rgba(rgba_from_grid(), 16, 16).unwrap_or_else(|_| {
        // Резерв на случай ошибки в карте: 1×1 непрозрачный белый пиксель.
        Icon::from_rgba(vec![255; 4], 1, 1).unwrap()
    })
}
