# Windows API reference

The Windows APIs this project relies on, grouped by task: device enumeration, switching the default
device, tray, single instance, autostart.

> Format: interface/function → why we need it → key points. GUIDs are listed here so they are never
> guessed or copied from unreliable sources.

---

## 1. Enumerating and monitoring output devices — Core Audio (MMDevice API)

Public, documented API (`<audiopolicy.h>`, `<mmdeviceapi.h>`). Used through the `windows` crate with
the `Win32_Media_Audio` feature.

### CLSID_MMDeviceEnumerator / IID_IMMDeviceEnumerator

- GUIDs:
  - `CLSID_MMDeviceEnumerator` = `{BCDE0395-E52F-467C-8E3D-C4579291692E}`
  - `IID_IMMDeviceEnumerator` = `{A95664D2-9614-4F35-A746-DE8DB63617E6}`
- Obtained via `CoCreateInstance` → `IMMDeviceEnumerator`.
- Methods we use:
  - `EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE, &collection)` — **active** render devices
    (for the menu). `eRender = 0`. `DEVICE_STATE_UNPLUGGED` (= 2) may be added for display, but the
    default device can only be set to an active one.
  - `GetDefaultAudioEndpoint(eRender, eConsole, &device)` — the current default
    (`eConsole = 0` is the “general” default shown in the system).
  - `RegisterNotificationCallback` — subscribe an `IMMNotificationClient`.

### IMMDevice / IMMEndpoint / IPropertyStore

- `IMMDevice::GetId(&strId)` — the **stable endpoint ID**. This is what `config.json` stores for
  favourites, not the device name.
- `IMMDevice::OpenPropertyStore(STGM_READ)` → `IPropertyStore::GetValue(PKEY_Device_FriendlyName)` —
  the display name.
  - `PKEY_Device_FriendlyName`: fmtid `{A45C254E-DF1C-4EFD-8020-67D146A850E0}`, pid = 14.
  - The value is `VT_LPWSTR`; ownership transfers to the caller (`CoTaskMemFree` / `PropVariantClear`).
- `IMMEndpoint::GetDataFlow()` — to filter render devices when working with a mixed collection.

### IMMNotificationClient

- `IID_IMMNotificationClient` = `{7991EEC9-7E89-4D85-8390-6C703CEC60C0}`.
- A callback interface implemented by a Rust COM object (`implement` macro from `windows-core`).
- Events:
  - `OnDeviceAdded` / `OnDeviceRemoved` — USB headphones plugged in/out;
  - `OnDeviceStateChanged` — active / unplugged / notpresent;
  - `OnDefaultDeviceChanged` — someone changed the default → refresh icon and cycle index.
- All callbacks arrive on the thread that called `RegisterNotificationCallback`.
- **Must** release the callback before unload, otherwise the process can crash on exit.

---

## 2. Switching the default output device — IPolicyConfig (undocumented)

Windows offers no public API. The only practical path is the `CPolicyConfigClient` COM class with the
`IPolicyConfig` interface, as used by SoundSwitch and EarTrumpet.

- GUIDs (verified against SoundSwitch `ComGuid.cs`):
  - `CLSID_CPolicyConfigClient` = `{870AF99C-171D-4F9E-AF0D-E63DF40C2BC9}`
  - `IID_IPolicyConfigX` = `{8F9FB2AA-1C0B-4D54-B6BB-B2F2A10CE03C}` (newer Win10/11 builds — tried first)
  - `IID_IPolicyConfig` = `{F8679F50-850A-41CF-9C72-430F290290C8}` (Windows 7+)
  - ⚠️ The old IID `{F8679F50-850A-41CF-9C26-220C23D0139C}` returns **E_NOINTERFACE (0x80004002)** — do not use.
  - `IID_IPolicyConfigVista` = `{568B9108-44BF-40B4-9006-86AFE5B5A620}` (different vtable layout)
- `IPolicyConfig` and `IPolicyConfigX` share the **same** vtable layout (13 methods after IUnknown,
  `SetDefaultEndpoint` = slot 13). Try the IIDs in order via `QueryInterface`, as SoundSwitch does.
- Key method: `SetDefaultEndpoint(PCWSTR wszDeviceId, ERole role)`.
  - Call it **three times** for `eConsole (0)`, `eMultimedia (1)`, `eCommunication (2)`. With only one
    role set, some applications keep playing through the old device.
  - `wszDeviceId` is the same `strId` returned by `IMMDevice::GetId`.
- Risks and rules:
  - Undocumented — keep it isolated in the `policy` module.
  - On error: log and degrade, never panic.
  - Leave a code comment pointing at the source (SoundSwitch).
- Forbidden: emulating the Control Panel through `SendInput`.

---

## 3. Tray — Shell_NotifyIconW

- You do **not** call it directly: the `tray-icon` crate wraps `Shell_NotifyIconW` +
  `NOTIFYICONDATAW` and the message loop. Low-level knowledge helps when debugging:
  - icon and tooltip are registered by one `Shell_NotifyIconW(NIM_ADD, ...)` call;
  - click events arrive as a custom window message (`WM_APP+n`) in a hidden window;
  - `Shell_NotifyIconGetRect` — the icon's screen rectangle;
  - menu: `CreatePopupMenu`/`TrackPopupMenuEx` plus `SetForegroundWindow` before showing (otherwise
    the menu does not close on outside clicks).
- When the device list changes, rebuild the menu from scratch.
- ⚠️ **Tooltip text limit**: the Shell displays at most **64 UTF-16 characters** of `szTip` and
  silently cuts anything longer, mid-string. Keep the text at ≤63 characters and truncate names yourself.

### 3a. Notification tooltip (tracking tooltip)

- Window class `tooltips_class32` (`InitCommonControls` required), styles
  `WS_POPUP | TTS_ALWAYSTIP | TTS_NOPREFIX`, extended `WS_EX_TOPMOST | WS_EX_TOOLWINDOW`.
- Tool flags `TTF_TRACK | TTF_ABSOLUTE` + `TTM_TRACKPOSITION` (coordinates packed into `LPARAM` with
  `MAKELONG`: low word = x, high word = y) + `TTM_TRACKACTIVATE(TRUE)`; deactivate with
  `TTM_TRACKACTIVATE(FALSE)`.
- ⚠️ **`cbSize` in `TTTOOLINFOW`**: the full size (V3, 72 bytes on x64) is **rejected** by the
  built-in comctl32 (v5.82) — `TTM_ADDTOOLW` returns FALSE and the tooltip never shows. Use the
  **V2 size**: `size_of::<TTTOOLINFOW>() - size_of::<*mut c_void>()` (64 on x64).
- ⚠️ **Positioning near the screen edge**: `TTM_TRACKPOSITION` places the window exactly at the given
  point and does not adjust it. Near the bottom/right edge (a tray icon), the tooltip ends up
  off-screen. Correct order: activate → `GetWindowRect` → choose position (below the anchor, else
  above) → clamp to the `rcWork` from `MonitorFromPoint`/`GetMonitorInfoW` → send `TTM_TRACKPOSITION` again.
- `lpszText` must stay valid while the tool is registered (Win32 keeps a pointer, not a copy) — in
  Rust, `Box::leak`.
- Auto-hide: `SetTimer(hwnd, id, ms, NULL)` + `WM_TIMER` in the owner window's `WndProc` (the owner
  may be a message-only window).

---

## 3b. Global hotkey — RegisterHotKey

- `RegisterHotKey(NULL, id, modifiers | MOD_NOREPEAT, vk)` — `WM_HOTKEY` is posted to the thread
  queue and the combination fires from any application.
- `MOD_NOREPEAT` (0x4000) is required; otherwise holding the keys floods `WM_HOTKEY`.
- Conflict check: simply try to register. A taken combination fails with
  `ERROR_HOTKEY_ALREADY_REGISTERED` (1409 → `0x80070581`).
- Modifier required: without Ctrl/Alt/Shift/Win a hotkey would swallow a plain keypress everywhere.
  Modifier state comes from `GetAsyncKeyState(VK_CONTROL/VK_SHIFT/VK_MENU/VK_LWIN/VK_RWIN)`.
- Capture: poll `GetAsyncKeyState` over a candidate list and compare with the previous poll. Works
  regardless of focus, unlike `WM_KEYDOWN`.
- Unregister the current combination before starting capture (`UnregisterHotKey`).

---

## 4. Single instance — named mutex

- `CreateMutexW(NULL, FALSE, "Local\\SwitcherProgramm.Instance")` → if
  `GetLastError() == ERROR_ALREADY_EXISTS`, the second instance exits quietly.
- The `Local\` prefix is required: the mutex lives in the current user session.
- Keep the handle alive until `main` returns.

---

## 5. Autostart (optional) — registry

- Key: `HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run`, `REG_SZ` value named
  `SwitcherProgramm`.
- Data: the full path to the executable, quoted.
- HKCU only — never HKLM.
- API: `RegOpenKeyExW` / `RegSetValueExW` / `RegDeleteValueW` (`Win32_System_Registry` feature).

---

## 6. What not to use

| API | Why not |
|---|---|
| `SendInput` + `mmsys.cpl` to switch devices | Fragile, slow, breaks with locale and UI scaling. |
| `IAudioClient` / WASAPI render | We do not play audio, only route it. |
| `IAudioEndpointVolume` | Volume control is out of scope. |
| Legacy `waveOut*`/`mixer*` API | Cannot work with the modern endpoint model. |
| Storing data in HKLM or ProgramData | Requires extra privileges; user data belongs in HKCU and `%APPDATA%`. |

---

## 7. `windows` crate features

```toml
features = [
  "Win32_System_Com",
  "Win32_Media_Audio",
  "Win32_UI_Shell",
  "Win32_UI_WindowsAndMessaging",
  "Win32_UI_Controls",
  "Win32_UI_Input_KeyboardAndMouse",
  "Win32_System_Registry",
]
```

---

## 8. Links

- MMDevice API: <https://learn.microsoft.com/windows/win32/coreaudio/mmdevice-api>
- IMMNotificationClient: <https://learn.microsoft.com/windows/win32/api/mmdeviceapi/nn-mmdeviceapi-immnotificationclient>
- SoundSwitch (reference implementation): <https://github.com/Belphemur/SoundSwitch>
- EarTrumpet: <https://github.com/File-New-Project/EarTrumpet>
- tray-icon crate: <https://crates.io/crates/tray-icon>
