# Architecture

How the project is put together: modules, event flow, and the key design decisions.

## Stack

- **Rust**, edition 2021, stable channel.
- `tray-icon 0.8` (+ bundled `muda 0.8`) — tray icon and context menu.
- `windows 0.58` / `windows-core` — Win32 and COM bindings.
- `serde` + `serde_json` — config; `dirs` — `%APPDATA%` path.
- `log` + `env_logger` (debug only) — logging; `thiserror` — error types.
- Icons are generated in memory; no external files next to the executable.

## Layout

```
Cargo.toml
build.rs            // generates the .ico, the manifest and the build number
build-number.txt    // build counter, incremented on release builds
assets/app-icon.png // icon master (512×512 RGBA)
AGENTS.md
README.md
docs/
  windows-apis.md   // Win32/COM reference (GUIDs, pitfalls)
  architecture.md   // this file
src/
  main.rs           // entry point, single instance, COM, event loop, hotkey capture
  audio/mod.rs      // Core Audio: device enumeration, default endpoint
  policy/mod.rs     // IPolicyConfig: SetDefaultEndpoint
  config/mod.rs     // favourites + hotkey: load/save config.json
  tray/mod.rs       // icon, menu, tooltip
  hotkey/mod.rs     // global hotkey: RegisterHotKey, capture
  i18n/mod.rs       // UI localization: English / Russian by system language
  version.rs        // "0.1.0+build.N" from Cargo.toml + build.rs
  toast/mod.rs      // switch notification (tracking tooltip, auto-hide)
  icon/mod.rs       // tray icon: embedded 32×32 RGBA frame from build.rs
```

## Executable manifest

`build.rs` writes an application manifest and embeds it as `RT_MANIFEST` (resource id `1`) together
with the icon: common controls **v6** (modern tooltips and menus instead of comctl32 v5.82),
**PerMonitorV2** DPI awareness (crisp rendering on HiDPI and across monitors) and Windows 10/11
`supportedOS`. Without `rc.exe` both resources are skipped with a warning.

## Module responsibilities

### `audio` — output devices

Thin layer over the MMDevice API: `IMMDeviceEnumerator::EnumAudioEndpoints` for render devices,
`GetDefaultAudioEndpoint` for the current default, `PKEY_Device_FriendlyName` for names.
`AudioDevice { id, name, state }`, where `id` is the stable endpoint ID (`IMMDevice::GetId`).

`IMMNotificationClient` is implemented: every event (device added/removed/state changed, default changed)
marks the state dirty and wakes the event loop with a thread message. The loop coalesces bursts and
after 150 ms of quiet re-reads the default endpoint, re-syncs the cycle position and rebuilds the menu.

### `policy` — changing the default device

`CoCreateInstance(CPolicyConfigClient)` → try IID candidates via `QueryInterface`
(`IPolicyConfigX` first, then `IPolicyConfig`), then `SetDefaultEndpoint` for all three roles.
This is the only working path: Windows has no public API for it.

### `config` — settings

`Favorites { devices: Vec<String>, hotkey: Option<Hotkey> }`, stored at
`%APPDATA%\SwitcherProgramm\config.json`, written atomically (tmp + rename).
Corrupt/empty JSON or missing fields fall back to defaults instead of panicking.

### `tray` — icon and menu

`TrayManager::build_ui` builds both the tooltip and the menu from a single device enumeration:
“Переключить (N из M)”, “Hot-key переключатель: …”, a “Избранное” submenu and “Выход”. The menu is
rebuilt from scratch whenever state changes.

The “Избранное” submenu is the favourites editor: every render device is a **check menu item**
(checked = in favourites, the current device is marked ▶) and clicking it adds or removes the device
without switching. Each entry starts with a device-type symbol derived from
`PKEY_AudioEndpoint_FormFactor` (speakers, headphones, HDMI/DisplayPort, digital, line level) — `muda`
cannot render real icons in menu items. Switching happens via the top item, the tray icon or the hotkey.

The hover tooltip is capped at 64 characters (Shell `szTip` limit), so the text is compact:
`Устройство: N из M` plus the device name, truncated when needed.

### `hotkey` — global shortcut

`RegisterHotKey(hwnd = NULL)` → `WM_HOTKEY` is delivered to the thread queue; the event loop
handles it before dispatching. Capture works by polling `GetAsyncKeyState` over a candidate list
(A-Z, 0-9, F1-F24, named keys) and detecting newly pressed keys.
`Hotkey { modifiers, vk }` is serialised into the config.

### `icon` + `build.rs` — one icon for everything

The single source is `assets/app-icon.png` (512×512 RGBA). `build.rs` decodes it, downscales with an
area-average (box) filter using premultiplied alpha — so no dark fringe on the edges — and produces:
- a multi-size `.ico` (16/20/24/32/40/48/64/128/256, PNG entries) compiled by `rc.exe` and linked in;
- a 32×32 RGBA frame (`tray-32.rgba` in `OUT_DIR`) that `icon::get_icon()` embeds with `include_bytes!`
  and hands to the tray. Windows downscales it to 16/20/24 for lower DPI.

No icon is drawn in code. If `rc.exe` is missing the build only warns and the executable keeps the
default icon; if the PNG cannot be read, a transparent tray frame is used so the build still succeeds.

### `i18n` — localization

All user-visible strings live in `i18n::lang()`: a `Lang` enum (English / Russian) with one method
per message. The language is detected once from `GetUserDefaultUILanguage` (primary language ID
`0x19` = Russian), overridable with `SWITCHER_LANG=en|ru` for testing. English is the fallback for
every other locale. No UI text is hardcoded outside this module.

### `toast` — switch notification
A `tooltips_class32` window in `TTF_TRACK | TTF_ABSOLUTE` mode:
`TTM_ADDTOOLW` → `TTM_TRACKACTIVATE` → measure with `GetWindowRect` → place precisely
(below the icon, or above it when there is no room) clamped to the monitor work area →
send `TTM_TRACKPOSITION` again. Auto-hide is a 1-second `SetTimer`, handled as `WM_TIMER`
in the owner window's `WndProc`.

Look and feel matches Shell tooltips: the v6 manifest enables themed rendering, and before every show
the window gets `SetWindowTheme(hwnd, "DarkMode_Explorer")` when dark mode is on
(`ShouldAppsUseDarkMode` from `uxtheme.dll`, ordinal 132 — undocumented, so any failure falls back to
the light theme). `WM_SETTINGCHANGE`/`WM_THEMECHANGED` re-apply the theme, and because message-only
windows miss broadcasts, it is also re-applied on each show.

## Event flow

A single main thread with its own message loop:

```
loop {
    while PeekMessageW(...) {
        WM_QUIT           -> exit
        WM_HOTKEY         -> cycle_next()
        WM_DEVICES_CHANGED-> schedule a debounced refresh
        otherwise         -> TranslateMessage + DispatchMessage
    }
    TrayIconEvent::receiver().try_recv()  -> cache icon rect; Left click -> cycle_next()
    MenuEvent::receiver().try_recv()      -> dispatch_menu_event()
    flush_due_refresh()                   -> re-read devices after the quiet period
    tick_capture()                        -> hotkey capture (when active)
    MsgWaitForMultipleObjectsEx(timeout)  -> INFINITE, or 15 ms while capturing
}
```

The loop sleeps in `MsgWaitForMultipleObjectsEx` instead of polling, so it costs no CPU while idle and
still wakes instantly on clicks, `WM_HOTKEY` and COM notifications. Notification callbacks may run on a
different thread, so they post a thread message rather than relying on shared state alone.

Double clicks on the tray icon are debounced: Windows delivers `Double` plus two `Left` events, and only
the first click within 350 ms triggers a switch.

COM is initialised (`CoInitializeEx`, STA) before any object is created; all COM objects and UI
live on this same thread — nothing is passed between threads.

## Config

```json
{
  "devices": ["{0.0.0.00000000}.{...}"],
  "hotkey": { "modifiers": 6, "vk": 85 }
}
```

`modifiers` is the Win32 bitmask (Alt = 1, Ctrl = 2, Shift = 4, Win = 8; here Ctrl + Shift),
`vk` is the virtual-key code (85 = `U`).

## Release profile

`panic = "abort"`, `strip = true`, `lto = "fat"`, `opt-level = "z"`, `codegen-units = 1` —
the goal is one small (~300 KB) tray executable.

## Diagnostic modes

`--list`, `--switch <N>`, `--toast-test`, `--hotkey-test`, `--lang` run without the tray and the
mutex, with their own COM initialisation; they exist to verify audio/UI behaviour on a live system.
