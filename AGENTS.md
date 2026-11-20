# AGENTS.md

Instructions for AI agents working with this repository.

## Project

Windows tray application (Rust) that switches the default audio output device.
There is no window: it runs in the background, and the UI is the tray icon and its menu.

Win32/COM reference: [docs/windows-apis.md](docs/windows-apis.md) — consult it first for any COM/Win32 work.

## Required behaviour

- Left click the tray icon — cycle through **favourite** devices; right click — the menu.
- Non-favourite devices never take part in the cycle; unplugged favourites are skipped but stay in the config.
- Favourites are edited in the “Favorites” submenu: every entry is a checkbox (checked = favourite),
  clicking adds or removes it and saves the config; the entry symbol is the device type
  (`PKEY_AudioEndpoint_FormFactor`). Switching is done with a left click, the top menu item or the hotkey.
- A single favourite → a click does nothing; empty favourites → the cycle is unavailable.
- After every switch — a short toast near the icon (1 s) with the name and position in the cycle.
- Single instance: a second launch does not create another instance.
- The user can assign a global hotkey; it works from any application.
- The UI is localized: Russian for Russian Windows, English otherwise;
  override with the `SWITCHER_LANG=en|ru` environment variable.
- System changes are picked up automatically: a device plugged in or removed, the default changed
  externally — menu, tooltip and cycle position refresh themselves.

## Hard constraints

- Switching the default device — **only** through the undocumented `IPolicyConfig`
  (`SetDefaultEndpoint` for `eConsole`, `eMultimedia`, `eCommunication`).
  UI emulation is forbidden. On error: log it and do not switch, never panic.
- Config: `%APPDATA%\SwitcherProgramm\config.json`, atomic write (temporary file + rename).
  Favourites are identified by a stable endpoint ID, not by name.
- Hotkey: modifiers are mandatory; combinations taken by the system or other apps must be rejected
  (verified by actually calling `RegisterHotKey`).
- The current cycle index is runtime state — never write it to the config.
- Win32/COM errors: handle `HRESULT`, log, degrade; no panics in production code.
- COM — main thread only; never pass pointers between threads.
- No heavy GUI frameworks; the registry is used only for optional autostart.
- User-facing strings (menu, toasts, hints) — only via `src/i18n` (`i18n::lang()`);
  never hardcode UI text in other modules. New languages are added there.
- Code comments and log messages are in English.
- The icon has a single source: `assets/app-icon.png` (512×512 RGBA). `build.rs` generates the
  `.ico` for the exe (16…256) and a 32×32 frame for the tray; nothing is drawn in code.
  Do not add binary assets other than this PNG. Without `rc.exe` the build does not fail — only a warning.

## Before committing

```powershell
cargo fmt
cargo clippy -- -D warnings
cargo test
cargo build --release
```

- The build must be warning-free (except the known false `unused dependency env_logger`).
- Quick live checks without the tray: `--list`, `--switch <N>`, `--toast-test`, `--hotkey-test`, `--lang`, `--version`.
- Build number: `build.rs` increments `build-number.txt` on release builds; commit the updated file
  with the release. `SWITCHER_BUILD_NUMBER` overrides the number.
- Manual scenarios: 1 / 2 / 5+ devices, unplugging the active device, sleep/wake,
  audio from an already playing app after switching.

## Commits

- Conventional commits, messages in English: `feat:`, `fix:`, `docs:`, `chore:`.

## Environment

- Windows 10 1809+ / 11, PowerShell. Rust — stable channel.
