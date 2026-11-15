# Windows API для SwitcherProgramm

Справочник по API Windows, которые используются в проекте. Разбито по задачам: перечисление устройств, смена устройства по умолчанию, трей, single instance, автозапуск.

> Формат: название интерфейса/функции → зачем нужно → ключевые моменты. GUID-ы приведены, чтобы их не выдумывать и не копировать из сомнительных источников.

---

## 1. Перечисление и мониторинг устройств вывода — Core Audio (MMDevice API)

Публичный, документированный API (`<audiopolicy.h>`, `<mmdeviceapi.h>`). Использовать через крейт `windows` с фичей `Win32_Media_Audio`.

### CLSID_MMDeviceEnumerator / IID_IMMDeviceEnumerator

- GUIDы:
  - `CLSID_MMDeviceEnumerator` = `{BCDE0395-E52F-467C-8E3D-C4579291692E}`
  - `IID_IMMDeviceEnumerator` = `{A95664D2-9614-4F35-A746-DE8DB63617E6}`
- Получение: `CoCreateInstance` → `IMMDeviceEnumerator`.
- Используемые методы:
  - `EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE, &collection)` — список **активных** устройств вывода (для меню). `eRender = 0`. Для отображения отключённых в меню можно дополнительно запросить `DEVICE_STATE_UNPLUGGED` (= 2), но менять default можно только на active-устройство.
  - `GetDefaultAudioEndpoint(eRender, eConsole, &device)` — текущее устройство по умолчанию (роль `eConsole = 0` — «общий» default, он и отображается в системе).
  - `RegisterNotificationCallback` — подписка на `IMMNotificationClient`.

### IMMDevice / IMMEndpoint / IPropertyStore

- `IMMDevice::GetId(&strId)` — **стабильный endpoint ID**. Именно он хранится в `config.json` как идентификатор избранного, а не имя устройства.
- `IMMDevice::OpenPropertyStore(STGM_READ)` → `IPropertyStore::GetValue(PKEY_Device_FriendlyName)` — дружественное имя для отображения.
  - `PKEY_Device_FriendlyName`: fmtid `{A45C254E-DF1C-4EFD-8020-67D146A850E0}`, pid = 14.
  - Значение — `VT_LPWSTR`, владение строкой уходит к вызывающему (`CoTaskMemFree` / `PropVariantClear`).
- `IMMEndpoint::GetDataFlow()` — чтобы отфильтровать только render-устройства, если работаем со смешанной коллекцией.

### IMMNotificationClient

- GUID `IID_IMMNotificationClient` = `{7991EEC9-7E89-4D85-8390-6C703CEC60C0}`.
- Callback-интерфейс: реализуется в Rust-объекте через `implement`-макрос `windows-core`.
- События:
  - `OnDeviceAdded` / `OnDeviceRemoved` — USB-наушники воткнули/вынули;
  - `OnDeviceStateChanged` — устройство стало active/unplugged/notpresent;
  - `OnDefaultDeviceChanged` — кто-то сменил default → обновить иконку и текущий индекс цикла.
- Все колбэки приходят на потоке, где был сделан `RegisterNotificationCallback`.
- **Обязательно** отпустить callback перед выгрузкой: иначе возможен crash при завершении процесса.

---

## 2. Смена устройства вывода по умолчанию — IPolicyConfig (недокументированный)

Windows не даёт публичного API. Единственный практичный путь — COM-класс `CPolicyConfigClient` с интерфейсом `IPolicyConfig`, как это делают SoundSwitch и EarTrumpet.

- GUIDы (сверены с SoundSwitch: `ComGuid.cs`):
  - `CLSID_CPolicyConfigClient` = `{870AF99C-171D-4F9E-AF0D-E63DF40C2BC9}`
  - `IID_IPolicyConfigX` = `{8F9FB2AA-1C0B-4D54-B6BB-B2F2A10CE03C}` (новые сборки Win10/11 — пробуется первым)
  - `IID_IPolicyConfig` = `{F8679F50-850A-41CF-9C72-430F290290C8}` (Windows 7+)
  - ⚠️ IID `{F8679F50-850A-41CF-9C26-220C23D0139C}` из старых проектов даёт **E_NOINTERFACE (0x80004002)** — не использовать.
  - `IID_IPolicyConfigVista` = `{568B9108-44BF-40B4-9006-86AFE5B5A620}` (другая раскладка vtable)
- `IPolicyConfig` и `IPolicyConfigX` имеют **одинаковую** раскладку vtable (13 методов после IUnknown, `SetDefaultEndpoint` = слот 13). Перебор IID через `QueryInterface` — как в SoundSwitch.
- Ключевой метод: `SetDefaultEndpoint(PCWSTR wszDeviceId, ERole role)`.
  - Вызывать **трижды** для ролей `eConsole (0)`, `eMultimedia (1)`, `eCommunication (2)`. Если вызвать только для одной роли, часть приложений продолжит играть через старое устройство.
  - `wszDeviceId` — тот же `strId`, что и из `IMMDevice::GetId`.
- Риски и правила:
  - Недокументировано Microsoft — изолировать в отдельном модуле `policy`.
  - Ошибка — логировать и деградировать, не паниковать.
  - Комментарий в коде со ссылкой на источник (SoundSwitch).
- Запрещено: эмуляция через UI (`mmsys.cpl` + `SendInput`).

---

## 3. Трей — Shell_NotifyIconW

- Напрямую использовать **не нужно**: крейт `tray-icon` оборачивает `Shell_NotifyIconW` + `NOTIFYICONDATAW` и message loop. Знание нижнего уровня нужно только для отладки:
  - иконка и tooltip регистрируются одним вызовом `Shell_NotifyIconW(NIM_ADD, ...)`;
  - события кликов приходят кастомным оконным сообщением (`WM_APP+n`) в скрытое окно;
  - `Shell_NotifyIconGetRect` — прямоугольник иконки на экране;
  - меню: `CreatePopupMenu`/`TrackPopupMenuEx` + `SetForegroundWindow` перед показом (иначе меню не закрывается по клику мимо).
- При изменении списка устройств — пересоздавать меню целиком.
- ⚠️ **Лимит текста тултипа**: Shell показывает `szTip` максимум в **64 UTF-16 символа** и молча обрезает более длинный текст. Держать текст ≤63 символов.

### 3a. Всплывающая подсказка-уведомление (tracking tooltip)

- Окно класса `tooltips_class32` (`InitCommonControls` обязателен), стили `WS_POPUP | TTS_ALWAYSTIP | TTS_NOPREFIX`, расширенные — `WS_EX_TOPMOST | WS_EX_TOOLWINDOW`.
- Флаги `TTF_TRACK | TTF_ABSOLUTE` + `TTM_TRACKPOSITION` (координаты в `LPARAM` через `MAKELONG`: младшее слово — x, старшее — y) + `TTM_TRACKACTIVATE(TRUE)`; деактивация — `TTM_TRACKACTIVATE(FALSE)`.
- ⚠️ **`cbSize` в `TTTOOLINFOW`**: полный размер (V3, 72 байта на x64) встроенный comctl32 (v5.82) **отвергает** — `TTM_ADDTOOLW` возвращает FALSE. Нужен **V2-размер**: `size_of::<TTTOOLINFOW>() - size_of::<*mut c_void>()` (64 на x64).
- ⚠️ **Позиционирование у кромки экрана**: `TTM_TRACKPOSITION` ставит окно ровно в точку и не подправляет его. Активировать → `GetWindowRect` → позиция (под якорем, иначе над ним) → зажать в `rcWork` из `MonitorFromPoint`/`GetMonitorInfoW` → ещё раз `TTM_TRACKPOSITION`.
- Текст `lpszText` должен оставаться валидным, пока зарегистрирован инструмент — в Rust `Box::leak`.
- Авто-скрытие: `SetTimer(hwnd, id, ms, NULL)` + `WM_TIMER` в `WndProc` окна-владельца (может быть message-only).

---

## 3b. Глобальная горячая клавиша — RegisterHotKey

- `RegisterHotKey(NULL, id, modifiers | MOD_NOREPEAT, vk)` — `WM_HOTKEY` приходит в очередь потока, комбинация срабатывает из любого приложения.
- `MOD_NOREPEAT` (0x4000) обязателен: без него удержание клавиши генерирует поток `WM_HOTKEY`.
- Проверка занятости: попытаться зарегистрировать. Занятая комбинация → `ERROR_HOTKEY_ALREADY_REGISTERED` (1409 → `0x80070581`).
- Только с модификатором: без Ctrl/Alt/Shift/Win комбинация перехватила бы обычное нажатие. Модификаторы — `GetAsyncKeyState(VK_CONTROL/VK_SHIFT/VK_MENU/VK_LWIN/VK_RWIN)`.
- Захват: опрос `GetAsyncKeyState` по списку кандидатов + сравнение с предыдущим опросом. Работает независимо от фокуса, в отличие от `WM_KEYDOWN`.
- Перед настройкой снять текущую регистрацию (`UnregisterHotKey`).

---

## 4. Одиночный запуск — именованный мьютекс

- `CreateMutexW(NULL, FALSE, "Local\\SwitcherProgramm.Instance")` → если `GetLastError() == ERROR_ALREADY_EXISTS`, второй экземпляр тихо выходит.
- Префикс `Local\` обязателен — мьютекс в сессии текущего пользователя.
- Хендл держать до конца работы `main`.

---

## 5. Автозапуск (опциональная настройка) — реестр

- Ключ: `HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run`, значение типа `REG_SZ` с именем `SwitcherProgramm`.
- Данные: полный путь к exe (в кавычках).
- Только HKCU — не HKLM.
- API: `RegOpenKeyExW` / `RegSetValueExW` / `RegDeleteValueW` (фича `Win32_System_Registry`).

---

## 6. Что НЕ использовать

| API | Почему нет |
|---|---|
| `SendInput` + `mmsys.cpl` для смены устройства | Хрупко, медленно, ломается от локали/масштаба UI. |
| `IAudioClient` / WASAPI render | Мы не воспроизводим звук, только управляем маршрутом. |
| `IAudioEndpointVolume` | Регулировка громкости — вне скоупа проекта. |
| Устаревший `waveOut*`/`mixer*` API | Legacy API, не умеет работать с modern endpoint-моделью. |
| Хранение данных в HKLM или ProgramData | Нужны лишние права; всё пользовательское — в HKCU и `%APPDATA%`. |

---

## 7. Фичи крейта `windows`

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

## 8. Ссылки

- MMDevice API: <https://learn.microsoft.com/windows/win32/coreaudio/mmdevice-api>
- IMMNotificationClient: <https://learn.microsoft.com/windows/win32/api/mmdeviceapi/nn-mmdeviceapi-immnotificationclient>
- SoundSwitch (эталон смены default-устройства): <https://github.com/Belphemur/SoundSwitch>
- EarTrumpet: <https://github.com/File-New-Project/EarTrumpet>
- tray-icon crate: <https://crates.io/crates/tray-icon>
