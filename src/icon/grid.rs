// Пиксельная карта иконки — единый источник для иконки трея и ресурса exe.
//
// Файл подключается и в `src/icon/mod.rs` (как обычный модуль), и в
// `build.rs` (`include!`), поэтому здесь не должно быть `use` и inner-документации.

/// Цвет фона (синий), RGBA.
pub const BG: [u8; 4] = [43, 127, 255, 255];
/// Цвет полос (белый), RGBA.
pub const BAR: [u8; 4] = [255, 255, 255, 255];
/// Прозрачный пиксель.
pub const EMPTY: [u8; 4] = [0, 0, 0, 0];

/// Сторона пиксельной карты.
pub const GRID_SIZE: usize = 16;

/// Пиксельная карта 16×16: '.' — прозрачный, 'b' — фон, 'w' — полоса.
/// Полосы эквалайзера разной высоты (столбцы 4, 7, 10, 13).
pub const GRID: [&str; GRID_SIZE] = [
    "................",
    "..bbbbbbbbbbbb..",
    ".bbbbbbbbbbbbbb.",
    ".bbbbbbbbbbbbbb.",
    ".bbbbbbbbbbbbbb.",
    ".bbbbbbbbbbbbbb.",
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

/// Развернуть карту в RGBA-буфер размера `size × size` (метод ближайшего соседа).
///
/// Трей использует 16×16, а ресурс exe — набор размеров 16…256. Координаты
/// масштабируются пропорционально, поэтому подходят и размеры, не кратные 16.
pub fn rgba(size: usize) -> Vec<u8> {
    debug_assert!(size >= GRID_SIZE);
    let mut out = Vec::with_capacity(size * size * 4);
    for y in 0..size {
        let row: Vec<char> = GRID[y * GRID_SIZE / size].chars().collect();
        for x in 0..size {
            let ch = row[x * GRID_SIZE / size];
            let color = match ch {
                'b' => BG,
                'w' => BAR,
                _ => EMPTY,
            };
            out.extend_from_slice(&color);
        }
    }
    out
}
