//! Enumeration and monitoring of audio output devices (Core Audio / MMDevice API).
//!
//! Uses `IMMDeviceEnumerator` to get the device list and
//! `IMMNotificationClient` to subscribe to add/remove/state-change events.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use windows::core::{implement, PCWSTR, PWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::Media::Audio::{
    eConsole, eRender, EDataFlow, ERole, IMMDevice, IMMDeviceEnumerator, IMMNotificationClient,
    IMMNotificationClient_Impl, MMDeviceEnumerator, PKEY_AudioEndpoint_FormFactor, DEVICE_STATE,
    DEVICE_STATE_ACTIVE, DEVICE_STATE_UNPLUGGED,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL, STGM_READ};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::System::Variant::{VT_LPWSTR, VT_UI4};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, PROPERTYKEY};
use windows::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_APP};

use crate::config::Favorites;

/// Device state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    Active,
    Unplugged,
    Inactive,
}

/// Output device form factor — this is how Windows classifies endpoints
/// (used for the icon in the menu, as in the sound settings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormFactor {
    Speakers,
    Headphones,
    /// HDMI / DisplayPort (DigitalAudioDisplayDevice).
    DisplayPort,
    /// S/PDIF and digital passthrough.
    Digital,
    LineLevel,
    Remote,
    Other,
}

impl FormFactor {
    fn from_win32(value: u32) -> Self {
        match value {
            1 => Self::Speakers,
            3 | 5 => Self::Headphones, // Headphones, Headset
            9 => Self::DisplayPort,
            7 | 8 => Self::Digital, // UnknownDigitalPassthrough, SPDIF
            2 => Self::LineLevel,
            0 => Self::Remote,
            _ => Self::Other,
        }
    }
}

/// Information about a single device.
#[derive(Debug, Clone)]
pub struct AudioDevice {
    /// Stable endpoint ID (used as the identifier).
    pub id: String,
    /// Friendly name for display.
    pub name: String,
    /// Current state.
    pub state: DeviceState,
    /// Device type (for the icon in the menu).
    pub form_factor: FormFactor,
}

/// Audio device manager.
///
/// Encapsulates `IMMDeviceEnumerator` and `IMMNotificationClient`.
pub struct AudioManager {
    enumerator: IMMDeviceEnumerator,
    /// COM callback object. Kept so that the reference handed to the MMDevice API
    /// stays valid; released in Drop after Unregister.
    notification_client: Option<IMMNotificationClient>,
}

impl AudioManager {
    /// Create a new AudioManager.
    ///
    /// The `expect`s are acceptable — initialization happens before entering the event loop (see AGENTS.md).
    pub fn new() -> Self {
        // Remember the event loop thread — callbacks will wake it with a message.
        UI_THREAD_ID.store(unsafe { GetCurrentThreadId() }, Ordering::Release);

        unsafe {
            // Create IMMDeviceEnumerator via CoCreateInstance.
            // The IID is taken from type T (= IID_IMMDeviceEnumerator).
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .expect("Failed to create IMMDeviceEnumerator");

            // Create the IMMNotificationClient COM object and register the callback.
            // Events set a flag that the event loop picks up and
            // rebuilds the menu with (see `take_refresh_request`).
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

    /// Get the list of all output devices (active + unplugged).
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

                // Get the endpoint ID (stable identifier).
                let id = match get_endpoint_id(&device) {
                    Some(id) => id,
                    None => continue,
                };

                // Get the FriendlyName; if empty — show the endpoint ID.
                let name = get_friendly_name(&device).unwrap_or_else(|| id.clone());

                // Determine the state.
                let state = get_state(&device);

                // Form factor — for the icon in the menu.
                let form_factor = get_form_factor(&device);

                devices.push(AudioDevice {
                    id,
                    name,
                    state,
                    form_factor,
                });
            }

            devices
        }
    }

    /// Get the current default device (endpoint ID).
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

    /// Get the list of favorite devices with their state (in config order).
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
        // Unsubscribe from notifications BEFORE releasing the COM callback object:
        // otherwise MMDevice may invoke an already destroyed object.
        if let Some(client) = self.notification_client.take() {
            unsafe {
                if let Err(e) = self
                    .enumerator
                    .UnregisterEndpointNotificationCallback(&client)
                {
                    log::error!("Failed to unregister endpoint notification callback: {}", e);
                }
            }
            // `client` is dropped here — the COM refcount is released.
        }
    }
}

/// Get the device endpoint ID.
///
/// The string is allocated by the COM allocator and must be freed via CoTaskMemFree.
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
        // Free the string allocated by CoTaskMemAlloc inside the MMDevice API.
        CoTaskMemFree(Some(id.as_ptr() as *const core::ffi::c_void));
        result
    }
}

/// Get the device FriendlyName from IPropertyStore.
///
/// `PROPVARIANT` is freed automatically (Drop calls PropVariantClear).
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

/// Get the device state.
fn get_state(device: &IMMDevice) -> DeviceState {
    match unsafe { device.GetState() } {
        Ok(state) if state.0 == DEVICE_STATE_ACTIVE.0 => DeviceState::Active,
        Ok(state) if state.0 == DEVICE_STATE_UNPLUGGED.0 => DeviceState::Unplugged,
        _ => DeviceState::Inactive,
    }
}

/// Get the device form factor (`PKEY_AudioEndpoint_FormFactor`, VT_UI4).
fn get_form_factor(device: &IMMDevice) -> FormFactor {
    unsafe {
        let Some(store) = device.OpenPropertyStore(STGM_READ).ok() else {
            return FormFactor::Other;
        };
        let Ok(propvar) = store.GetValue(&PKEY_AudioEndpoint_FormFactor) else {
            return FormFactor::Other;
        };
        let raw = propvar.as_raw();
        if raw.Anonymous.Anonymous.vt != VT_UI4.0 {
            return FormFactor::Other;
        }
        FormFactor::from_win32(raw.Anonymous.Anonymous.Anonymous.ulVal)
    }
}

/// Flag "device state has changed".
///
/// `IMMNotificationClient` callbacks set it, and the event loop picks it up
/// (`take_refresh_request`) and rebuilds the menu.
static DEVICES_DIRTY: AtomicBool = AtomicBool::new(false);

/// ID of the thread running the event loop (0 — not known yet).
static UI_THREAD_ID: AtomicU32 = AtomicU32::new(0);

/// Message to the thread: the device list has changed.
pub const WM_DEVICES_CHANGED: u32 = WM_APP + 1;

/// Request a device list refresh (from a callback).
///
/// Callbacks may arrive not on the event loop thread, so setting the flag alone is not enough —
/// the loop also needs to be woken with a message, otherwise it sleeps in `MsgWaitForMultipleObjects`.
fn request_refresh() {
    DEVICES_DIRTY.store(true, Ordering::Release);
    let thread_id = UI_THREAD_ID.load(Ordering::Acquire);
    if thread_id != 0 {
        unsafe {
            let _ = PostThreadMessageW(thread_id, WM_DEVICES_CHANGED, WPARAM(0), LPARAM(0));
        }
    }
}

/// Take the refresh request: `true` — devices have changed.
pub fn take_refresh_request() -> bool {
    DEVICES_DIRTY.swap(false, Ordering::Acquire)
}

/// The project's `IMMNotificationClient`: any event means the menu cache
/// and cycle state may be stale.
///
/// IMPORTANT (windows 0.58): the `*_Impl` trait is implemented for the GENERATED
/// wrapper `NotificationClient_Impl`, not for the original struct —
/// it is its address that ends up in the vtable. Access to the original
/// struct's data is through the `windows::core::IUnknownImpl` trait (`get_impl`).
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
