//! Встраивание иконки приложения.
//!
//! Мастер-файл — `assets/app-icon.png` (512×512 RGBA). Из него собирается:
//! - `.ico` для ресурса exe (размеры 16…256, PNG-записи);
//! - RGBA-кадр 32×32 для иконки трея (генерируется в `OUT_DIR` и
//!   подключается через `include_bytes!`).
//!
//! Уменьшение — усреднением по площади (box filter) с предумножением альфы,
//! чтобы не было тёмной каймы по краям.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Размеры, которые попадут в `.ico`.
const ICON_SIZES: [u32; 9] = [16, 20, 24, 32, 40, 48, 64, 128, 256];
/// Размер иконки трея (Windows сам уменьшит до 16/20/24 при другом DPI).
const TRAY_SIZE: u32 = 32;

fn main() {
    println!("cargo:rerun-if-changed=assets/app-icon.png");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=SWITCHER_BUILD_NUMBER");

    // Номер сборки: env → build-number.txt (инкремент только в release).
    let number = build_number();
    println!("cargo:rustc-env=SWITCHER_BUILD_NUMBER={}", number);

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo"));

    let source = match load_source() {
        Ok(source) => source,
        Err(e) => {
            println!("cargo:warning=cannot read assets/app-icon.png: {}", e);
            write_fallback_tray_icon(&out_dir);
            return;
        }
    };
    let (width, height, rgba) = source;

    // Кадр для трея — всегда, независимо от наличия rc.exe.
    let tray = resize_rgba(&rgba, width, height, TRAY_SIZE, TRAY_SIZE);
    if let Err(e) = std::fs::write(out_dir.join("tray-32.rgba"), &tray) {
        println!("cargo:warning=cannot write tray icon: {}", e);
    }

    let ico_path = out_dir.join("app.ico");
    if let Err(e) = write_ico(&ico_path, width, height, &rgba) {
        println!("cargo:warning=cannot generate app icon: {}", e);
        return;
    }

    embed_as_resource(&out_dir, &ico_path);
}

/// Прочитать PNG и вернуть RGBA-пиксели.
fn load_source() -> Result<(u32, u32, Vec<u8>), Box<dyn std::error::Error>> {
    let bytes = std::fs::read("assets/app-icon.png")?;
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(
        png::Transformations::EXPAND | png::Transformations::ALPHA | png::Transformations::STRIP_16,
    );

    let mut reader = decoder.read_info()?;
    let mut buffer = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer)?;
    buffer.truncate(info.buffer_size());

    let (color_type, _) = reader.output_color_type();
    let rgba = match color_type {
        png::ColorType::Rgba => buffer,
        png::ColorType::Rgb => buffer
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|rgb| [rgb[0], rgb[1], rgb[2], 255])
            .collect(),
        other => return Err(format!("unsupported PNG color type: {:?}", other).into()),
    };

    Ok((info.width, info.height, rgba))
}

/// Уменьшить RGBA-изображение усреднением по площади (premultiplied alpha).
fn resize_rgba(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let (sw, sh, dw, dh) = (sw as usize, sh as usize, dw as usize, dh as usize);
    let mut out = vec![0u8; dw * dh * 4];
    let scale_x = sw as f64 / dw as f64;
    let scale_y = sh as f64 / dh as f64;

    for dy in 0..dh {
        let y0 = dy as f64 * scale_y;
        let y1 = (dy + 1) as f64 * scale_y;
        for dx in 0..dw {
            let x0 = dx as f64 * scale_x;
            let x1 = (dx + 1) as f64 * scale_x;

            let mut acc = [0f64; 3];
            let mut alpha_sum = 0f64;
            let mut coverage = 0f64;

            for sy in y0.floor() as usize..(y1.ceil() as usize).min(sh) {
                let wy = overlap(y0, y1, sy as f64, (sy + 1) as f64);
                for sx in x0.floor() as usize..(x1.ceil() as usize).min(sw) {
                    let wx = overlap(x0, x1, sx as f64, (sx + 1) as f64);
                    let weight = wx * wy;
                    if weight <= 0.0 {
                        continue;
                    }
                    let index = (sy * sw + sx) * 4;
                    let alpha = src[index + 3] as f64 / 255.0;
                    acc[0] += src[index] as f64 * alpha * weight;
                    acc[1] += src[index + 1] as f64 * alpha * weight;
                    acc[2] += src[index + 2] as f64 * alpha * weight;
                    alpha_sum += alpha * weight;
                    coverage += weight;
                }
            }

            let out_index = (dy * dw + dx) * 4;
            if coverage > 0.0 && alpha_sum > 0.0 {
                out[out_index] = (acc[0] / alpha_sum).round().clamp(0.0, 255.0) as u8;
                out[out_index + 1] = (acc[1] / alpha_sum).round().clamp(0.0, 255.0) as u8;
                out[out_index + 2] = (acc[2] / alpha_sum).round().clamp(0.0, 255.0) as u8;
                out[out_index + 3] = (alpha_sum / coverage * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
    }

    out
}

/// Длина пересечения отрезков [a0, a1] и [b0, b1].
fn overlap(a0: f64, a1: f64, b0: f64, b1: f64) -> f64 {
    (a1.min(b1) - a0.max(b0)).max(0.0)
}

/// Собрать `.ico` из PNG-изображений всех размеров.
fn write_ico(
    path: &Path,
    source_width: u32,
    source_height: u32,
    rgba: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut images: Vec<(u32, Vec<u8>)> = Vec::with_capacity(ICON_SIZES.len());
    for size in ICON_SIZES {
        let pixels = resize_rgba(rgba, source_width, source_height, size, size);
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

/// Закодировать RGBA-пиксели в PNG (максимальное сжатие).
fn encode_png(size: u32, rgba: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, size, size);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Best);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(rgba)?;
    }
    Ok(png)
}

/// Скомпилировать `.ico` и манифест в `.res` и передать линкеру.
fn embed_as_resource(out_dir: &Path, ico_path: &Path) {
    let Some(rc) = find_rc() else {
        println!(
            "cargo:warning=rc.exe not found; the .exe will use the default icon and no manifest"
        );
        return;
    };

    let manifest_path = out_dir.join("app.manifest");
    if let Err(e) = std::fs::write(&manifest_path, APP_MANIFEST) {
        println!("cargo:warning=cannot write app.manifest: {}", e);
    }

    let rc_file = out_dir.join("app.rc");
    // В .rc обратные слэши экранируются, пути — в кавычках.
    // 1 = CREATEPROCESS_MANIFEST_RESOURCE_ID, 24 = RT_MANIFEST.
    let ico_escaped = ico_path.to_string_lossy().replace('\\', "\\\\");
    let manifest_escaped = manifest_path.to_string_lossy().replace('\\', "\\\\");
    let rc_text = format!(
        "1 ICON \"{}\"\n1 24 \"{}\"\n",
        ico_escaped, manifest_escaped
    );
    if let Err(e) = std::fs::write(&rc_file, rc_text) {
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

/// Манифест приложения: comctl32 v6 (современные тултипы и меню),
/// PerMonitorV2 (чёткость на HiDPI) и поддержка Windows 10/11.
const APP_MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="SwitcherProgramm" version="1.0.0.0" processorArchitecture="*"/>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
    </windowsSettings>
  </application>
</assembly>
"#;

/// Номер сборки.
///
/// Приоритет: переменная `SWITCHER_BUILD_NUMBER` (CI/ручной релиз) → счётчик
/// в `build-number.txt` (коммитится). Счётчик увеличивается только при
/// release-сборке и пишется атомарно (tmp + rename). Файл намеренно не в
/// `rerun-if-changed`, иначе сборка зациклится на собственном изменении.
fn build_number() -> u64 {
    if let Ok(value) = std::env::var("SWITCHER_BUILD_NUMBER") {
        match value.trim().parse::<u64>() {
            Ok(number) => return number,
            Err(_) => println!(
                "cargo:warning=SWITCHER_BUILD_NUMBER is not a number: {:?}",
                value
            ),
        }
    }

    let path = Path::new("build-number.txt");
    let current = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok())
        .unwrap_or(0);

    let is_release = std::env::var("PROFILE").as_deref() == Ok("release");
    if !is_release {
        return current;
    }

    let next = current + 1;
    let temp = path.with_extension("txt.tmp");
    let written =
        std::fs::write(&temp, format!("{}\n", next)).and_then(|_| std::fs::rename(&temp, path));
    if let Err(e) = written {
        println!(
            "cargo:warning=cannot update build-number.txt ({}); keeping {}",
            e, current
        );
        return current;
    }
    next
}

/// Резервный кадр трея (прозрачный), если PNG не прочитался.
fn write_fallback_tray_icon(out_dir: &Path) {
    let size = TRAY_SIZE as usize;
    let _ = std::fs::write(out_dir.join("tray-32.rgba"), vec![0u8; size * size * 4]);
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
