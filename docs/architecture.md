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
  toast/mod.rs      // switch notification (tracking tooltip, auto-hide)
  icon/mod.rs       // 16×16 pixel icon
```

## Module responsibilities

### `audio` — output devices

Thin layer over the MMDevice API: `IMMDeviceEnumerator::EnumAudioEndpoints` for render devices,
`GetDefaultAudioEndpoint` for the current default, `PKEY_Device_FriendlyName` for names.
`AudioDevice { id, name, state }`, where `id` is the stable endpoint ID (`IMMDevice::GetId`).

`IMMNotificationClient` is a stub: add/remove/state-change events are only logged
(reacting to them is a future step).

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
“Переключить (N из M)”, “Hot-key переключатель: …”, a “Избранное” submenu (current device marked ▶,
all other devices as `+ name`) and “Выход”. The menu is rebuilt from scratch whenever state changes.

The hover tooltip is capped at 64 characters (Shell `szTip` limit), so the text is compact:
`Устройство: N из M` plus the device name, truncated when needed.

### `hotkey` — global shortcut

`RegisterHotKey(hwnd = NULL)` → `WM_HOTKEY` is delivered to the thread queue; the event loop
handles it before dispatching. Capture works by polling `GetAsyncKeyState` over a candidate list
(A-Z, 0-9, F1-F24, named keys) and detecting newly pressed keys.
`Hotkey { modifiers, vk }` is serialised into the config.

### `toast` — switch notification

A `tooltips_class32` window in `TTF_TRACK | TTF_ABSOLUTE` mode:
`TTM_ADDTOOLW` → `TTM_TRACKACTIVATE` → measure with `GetWindowRect` → place precisely
(below the icon, or above it when there is no room) clamped to the monitor work area →
send `TTM_TRACKPOSITION` again. Auto-hide is a 1-second `SetTimer`, handled as `WM_TIMER`
in the owner window's `WndProc`.

## Event flow

A single main thread with its own message loop:

```
loop {
    while PeekMessageW(...) {
        WM_QUIT   -> exit
        WM_HOTKEY -> cycle_next()
        otherwise -> TranslateMessage + DispatchMessage
    }
    TrayIconEvent::receiver().try_recv()  -> cache icon rect; Left click -> cycle_next()
    MenuEvent::receiver().try_recv()      -> dispatch_menu_event()
    tick_capture()                        -> hotkey capture (when active)
    sleep(15 ms while capturing, otherwise 50 ms)
}
```

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

`--list`, `--switch <N>`, `--toast-test`, `--hotkey-test` run without the tray and the mutex,
with their own COM initialisation; they exist to verify audio/UI behaviour on a live system.
