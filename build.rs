//! Встраивание иконки приложения в `.exe`.
//!
//! Иконка трея и иконка файла должны совпадать, поэтому источник один —
//! `src/icon/grid.rs`. Здесь карта разворачивается в несколько размеров
//! (16…256), упаковывается в `.ico`, компилируется `rc.exe` в `.res` и
//! передаётся линкеру.

use std::path::{Path, PathBuf};
use std::process::Command;

// Пиксельная карта без внешних зависимостей — можно включить как есть.
include!("src/icon/grid.rs");

/// Размеры, которые попадут в `.ico` (Explorer выберет подходящий).
const ICON_SIZES: [u32; 6] = [16, 24, 32, 48, 64, 256];

fn main() {
    println!("cargo:rerun-if-changed=src/icon/grid.rs");
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo"));
    let ico_path = out_dir.join("app.ico");

    if let Err(e) = write_ico(&ico_path) {
        println!("cargo:warning=cannot generate app icon: {}", e);
        return;
    }

    let Some(rc) = find_rc() else {
        println!("cargo:warning=rc.exe not found; the .exe will use the default icon");
        return;
    };

    let rc_file = out_dir.join("app.rc");
    // В .rc обратные слэши экранируются, путь — в кавычках.
    let ico_escaped = ico_path.to_string_lossy().replace('\\', "\\\\");
    if let Err(e) = std::fs::write(&rc_file, format!("1 ICON \"{}\"\n", ico_escaped)) {
        println!("cargo:warning=cannot write app.rc: {}", e);
        return;
    }

    let res_file = out_dir.join("app.res");
    let status = Command::new(&rc)
        .arg("/nologo")
        .arg(format!("/fo{}", res_file.display()))
        .arg(&rc_file)
        .status();

    match status {
        Ok(status) if status.success() => {
            println!("cargo:rustc-link-arg={}", res_file.display());
        }
        Ok(status) => println!("cargo:warning=rc.exe failed with {}; icon skipped", status),
        Err(e) => println!("cargo:warning=cannot run rc.exe: {}; icon skipped", e),
    }
}

/// Собрать `.ico` с PNG-изображениями всех размеров.
fn write_ico(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut images: Vec<(u32, Vec<u8>)> = Vec::with_capacity(ICON_SIZES.len());
    for size in ICON_SIZES {
        let pixels = rgba(size as usize);
        images.push((size, encode_png(size, &pixels)?));
    }

    let mut out = Vec::new();
    // ICONDIR
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type: 1 = icon
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());

    // ICONDIRENTRY для каждого изображения
    let mut offset = 6 + 16 * images.len() as u32;
    for (size, data) in &images {
        let dimension = if *size >= 256 { 0 } else { *size as u8 };
        out.push(dimension); // width (0 = 256)
        out.push(dimension); // height
        out.push(0); // palette size
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // color planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += data.len() as u32;
    }

    for (_, data) in &images {
        out.extend_from_slice(data);
    }

    std::fs::write(path, out)?;
    Ok(())
}

/// Закодировать RGBA-пиксели в PNG с индексированной палитрой.
///
/// В иконке всего три цвета (прозрачный, фон, полоса), поэтому один байт
/// на пиксель вместо четырёх сжимается заметно лучше — особенно на 256×256.
fn encode_png(size: u32, rgba: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    // Палитра: 0 — прозрачный, 1 — фон, 2 — полоса.
    const TRANSPARENT: u8 = 0;
    const BACKGROUND: u8 = 1;
    const BAR_INDEX: u8 = 2;

    let mut indexed = Vec::with_capacity(rgba.len() / 4);
    for pixel in rgba.chunks_exact(4) {
        let index = if pixel[3] == 0 {
            TRANSPARENT
        } else if pixel[..3] == BAR[..3] {
            BAR_INDEX
        } else {
            BACKGROUND
        };
        indexed.push(index);
    }

    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, size, size);
        encoder.set_color(png::ColorType::Indexed);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_palette(&[
            0,
            0,
            0, // 0: прозрачный (цвет не важен)
            BG[0],
            BG[1],
            BG[2], // 1: фон
            BAR[0],
            BAR[1],
            BAR[2], // 2: полоса
        ]);
        // Альфа для палитровых записей: прозрачный только индекс 0.
        encoder.set_trns(vec![0u8, 255, 255]);
        encoder.set_compression(png::Compression::Best);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&indexed)?;
    }
    Ok(png)
}

/// Найти `rc.exe`: переменная `RC`, переменные окружения Visual Studio,
/// затем стандартные каталоги Windows SDK (берётся самая новая версия).
fn find_rc() -> Option<PathBuf> {
    if let Ok(value) = std::env::var("RC") {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Some(path);
        }
    }

    if let Ok(dir) = std::env::var("WindowsSdkVerBinPath") {
        let path = PathBuf::from(dir).join("x64").join("rc.exe");
        if path.is_file() {
            return Some(path);
        }
    }

    let roots = [
        PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\bin"),
        PathBuf::from(r"C:\Program Files\Windows Kits\10\bin"),
    ];

    let mut best: Option<(u64, PathBuf)> = None;
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let candidate = entry.path().join("x64").join("rc.exe");
            if !candidate.is_file() {
                continue;
            }
            let version = entry
                .file_name()
                .to_string_lossy()
                .split('.')
                .filter_map(|part| part.parse::<u64>().ok())
                .fold(0u64, |acc, number| acc * 1000 + number.min(999));
            if best
                .as_ref()
                .is_none_or(|(best_version, _)| version > *best_version)
            {
                best = Some((version, candidate));
            }
        }
    }

    best.map(|(_, path)| path)
}
