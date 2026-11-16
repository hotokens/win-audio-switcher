//! Перечисление и мониторинг аудиоустройств вывода (Core Audio / MMDevice API).
//!
//! Использует `IMMDeviceEnumerator` для получения списка устройств и
//! `IMMNotificationClient` для подписки на события добавления/удаления/смены состояния.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use windows::core::{implement, PCWSTR, PWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::Media::Audio::{
    eConsole, eRender, EDataFlow, ERole, IMMDevice, IMMDeviceEnumerator, IMMNotificationClient,
    IMMNotificationClient_Impl, MMDeviceEnumerator, DEVICE_STATE, DEVICE_STATE_ACTIVE,
    DEVICE_STATE_UNPLUGGED,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL, STGM_READ};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, PROPERTYKEY};
use windows::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_APP};

use crate::config::Favorites;

/// Состояние устройства.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    Active,
    Unplugged,
    Inactive,
}

/// Информация об одном устройстве.
#[derive(Debug, Clone)]
pub struct AudioDevice {
    /// Стабильный endpoint ID (используется как идентификатор).
    pub id: String,
    /// Дружелюбное имя для отображения.
    pub name: String,
    /// Текущее состояние.
    pub state: DeviceState,
}

/// Менеджер аудиоустройств.
///
/// Инкапсулирует `IMMDeviceEnumerator` и `IMMNotificationClient`.
pub struct AudioManager {
    enumerator: IMMDeviceEnumerator,
    /// COM-объект callback'а. Держим, чтобы ссылка, отданная в MMDevice API,
    /// оставалась валидной; отпускаем в Drop после Unregister.
    notification_client: Option<IMMNotificationClient>,
}

impl AudioManager {
    /// Создать новый AudioManager.
    ///
    /// Допустимые `expect` — инициализация до входа в event loop (см. AGENTS.md).
    pub fn new() -> Self {
        // Запоминаем поток event loop — колбэки будут будить его сообщением.
        UI_THREAD_ID.store(unsafe { GetCurrentThreadId() }, Ordering::Release);

        unsafe {
            // Создаём IMMDeviceEnumerator через CoCreateInstance.
            // IID берётся из типа T (= IID_IMMDeviceEnumerator).
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .expect("Failed to create IMMDeviceEnumerator");

            // Создаём COM-объект IMMNotificationClient и регистрируем callback.
            // События выставляют флаг, который забирает event loop и
            // перестраивает меню (см. `take_refresh_request`).
            let client: IMMNotificationClient = NotificationClient.into();
            if let Err(e) = enumerator.RegisterEndpointNotificationCallback(&client) {
                log::error!("Failed to register endpoint notification callback: {}", e);
            } else {
                log::info!("Endpoint notification callback registered");
            }

            Self {
                enumerator,
                notification_client: Some(client),
            }
        }
    }

    /// Получить список всех устройств вывода (active + unplugged).
    pub fn list_devices(&self) -> Vec<AudioDevice> {
        unsafe {
            let state_mask = DEVICE_STATE(DEVICE_STATE_ACTIVE.0 | DEVICE_STATE_UNPLUGGED.0);
            let collection = match self.enumerator.EnumAudioEndpoints(eRender, state_mask) {
                Ok(c) => c,
                Err(e) => {
                    log::error!("Failed to enumerate audio endpoints: {}", e);
                    return Vec::new();
                }
            };

            let count = match collection.GetCount() {
                Ok(c) => c,
                Err(e) => {
                    log::error!("Failed to get device count: {}", e);
                    return Vec::new();
                }
            };

            let mut devices = Vec::with_capacity(count as usize);

            for i in 0..count {
                let device = match collection.Item(i) {
                    Ok(d) => d,
                    Err(e) => {
                        log::error!("Failed to get device {}: {}", i, e);
                        continue;
                    }
                };

                // Получаем endpoint ID (стабильный идентификатор).
                let id = match get_endpoint_id(&device) {
                    Some(id) => id,
                    None => continue,
                };

                // Получаем FriendlyName; если пусто — показываем endpoint ID.
                let name = get_friendly_name(&device).unwrap_or_else(|| id.clone());

                // Определяем состояние.
                let state = get_state(&device);

                devices.push(AudioDevice { id, name, state });
            }

            devices
        }
    }

    /// Получить текущее устройство по умолчанию (endpoint ID).
    pub fn get_default_endpoint(&self) -> Result<String, windows::core::Error> {
        unsafe {
            let device = self
                .enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .map_err(|e| {
                    log::error!("Failed to get default endpoint: {}", e);
                    e
                })?;

            get_endpoint_id(&device).ok_or_else(|| {
                log::error!("Failed to get default endpoint ID");
                windows::core::Error::from(windows::core::HRESULT(0x80004005_u32 as i32))
                // E_FAIL
            })
        }
    }

    /// Получить список избранных устройств с их состоянием (в порядке конфига).
    pub fn get_favorites(&self, favorites: &Favorites) -> Vec<AudioDevice> {
        let all_devices = self.list_devices();
        let device_map: HashMap<String, AudioDevice> =
            all_devices.into_iter().map(|d| (d.id.clone(), d)).collect();

        favorites
            .devices
            .iter()
            .filter_map(|id| device_map.get(id).cloned())
            .collect()
    }
}

impl Drop for AudioManager {
    fn drop(&mut self) {
        // Отписываемся от уведомлений ДО отпускания COM-объекта callback'а:
        // иначе MMDevice может вызвать уже уничтоженный объект.
        if let Some(client) = self.notification_client.take() {
            unsafe {
                if let Err(e) = self
                    .enumerator
                    .UnregisterEndpointNotificationCallback(&client)
                {
                    log::error!("Failed to unregister endpoint notification callback: {}", e);
                }
            }
            // `client` дропается здесь — COM-рефкаунт освобождается.
        }
    }
}

/// Получить endpoint ID устройства.
///
/// Строка выделена COM-аллокатором и обязана быть освобождена через CoTaskMemFree.
fn get_endpoint_id(device: &IMMDevice) -> Option<String> {
    unsafe {
        let id = match device.GetId() {
            Ok(id) => id,
            Err(e) => {
                log::error!("Failed to get device ID: {}", e);
                return None;
            }
        };
        let result = id.to_string().ok();
        // Освобождаем строку, выделенную CoTaskMemAlloc внутри MMDevice API.
        CoTaskMemFree(Some(id.as_ptr() as *const core::ffi::c_void));
        result
    }
}

/// Получить FriendlyName устройства из IPropertyStore.
///
/// `PROPVARIANT` освобождается автоматически (Drop вызывает PropVariantClear).
fn get_friendly_name(device: &IMMDevice) -> Option<String> {
    unsafe {
        let store: IPropertyStore = device.OpenPropertyStore(STGM_READ).ok()?;
        let propvar = store.GetValue(&PKEY_Device_FriendlyName).ok()?;

        let raw = propvar.as_raw();
        if raw.Anonymous.Anonymous.vt == VT_LPWSTR.0 {
            let ptr = raw.Anonymous.Anonymous.Anonymous.pwszVal;
            if !ptr.is_null() {
                return PWSTR(ptr).to_string().ok();
            }
        }
        None
    }
}

/// Получить состояние устройства.
fn get_state(device: &IMMDevice) -> DeviceState {
    match unsafe { device.GetState() } {
        Ok(state) if state.0 == DEVICE_STATE_ACTIVE.0 => DeviceState::Active,
        Ok(state) if state.0 == DEVICE_STATE_UNPLUGGED.0 => DeviceState::Unplugged,
        _ => DeviceState::Inactive,
    }
}

/// Флаг «состояние устройств изменилось».
///
/// Колбэки `IMMNotificationClient` выставляют его, а event loop забирает
/// (`take_refresh_request`) и перестраивает меню.
static DEVICES_DIRTY: AtomicBool = AtomicBool::new(false);

/// ID потока, в котором крутится event loop (0 — ещё не известен).
static UI_THREAD_ID: AtomicU32 = AtomicU32::new(0);

/// Сообщение потоку: список устройств изменился.
pub const WM_DEVICES_CHANGED: u32 = WM_APP + 1;

/// Запросить обновление списка устройств (из колбэка).
///
/// Колбэки могут прийти не на потоке event loop, поэтому мало выставить флаг —
/// нужно ещё разбудить его сообщением, иначе он спит в `MsgWaitForMultipleObjects`.
fn request_refresh() {
    DEVICES_DIRTY.store(true, Ordering::Release);
    let thread_id = UI_THREAD_ID.load(Ordering::Acquire);
    if thread_id != 0 {
        unsafe {
            let _ = PostThreadMessageW(thread_id, WM_DEVICES_CHANGED, WPARAM(0), LPARAM(0));
        }
    }
}

/// Забрать запрос на обновление: `true` — устройства изменились.
pub fn take_refresh_request() -> bool {
    DEVICES_DIRTY.swap(false, Ordering::Acquire)
}

/// `IMMNotificationClient` проекта: любое событие означает, что кэш меню
/// и состояние цикла могли устареть.
///
/// ВАЖНО (windows 0.58): `*_Impl`-трейт реализуется для СГЕНЕРИРОВАННОЙ
/// обёртки `NotificationClient_Impl`, а не для исходной структуры —
/// именно её адрес попадает в vtable. Доступ к данным исходной
/// структуры — через трейт `windows::core::IUnknownImpl` (`get_impl`).
#[implement(IMMNotificationClient)]
struct NotificationClient;

impl IMMNotificationClient_Impl for NotificationClient_Impl {
    fn OnDeviceStateChanged(
        &self,
        _deviceid: &PCWSTR,
        _newstate: DEVICE_STATE,
    ) -> windows::core::Result<()> {
        log::debug!("Notification: device state changed");
        request_refresh();
        Ok(())
    }

    fn OnDeviceAdded(&self, _deviceid: &PCWSTR) -> windows::core::Result<()> {
        log::debug!("Notification: device added");
        request_refresh();
        Ok(())
    }

    fn OnDeviceRemoved(&self, _deviceid: &PCWSTR) -> windows::core::Result<()> {
        log::debug!("Notification: device removed");
        request_refresh();
        Ok(())
    }

    fn OnDefaultDeviceChanged(
        &self,
        _flow: EDataFlow,
        _role: ERole,
        _deviceid: &PCWSTR,
    ) -> windows::core::Result<()> {
        log::debug!("Notification: default device changed");
        request_refresh();
        Ok(())
    }

    fn OnPropertyValueChanged(
        &self,
        _deviceid: &PCWSTR,
        _key: &PROPERTYKEY,
    ) -> windows::core::Result<()> {
        log::debug!("Notification: property value changed");
        request_refresh();
        Ok(())
    }
}
