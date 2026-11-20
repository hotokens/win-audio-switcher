//! Switching the default output device via IPolicyConfig.
//!
//! Windows does not provide a public API for changing the default device.
//! We use the undocumented COM class `CPolicyConfigClient` with the
//! `IPolicyConfig` interface, the way SoundSwitch and EarTrumpet do.
//!
//! Source: SoundSwitch / `SoundSwitch/Audio/PolicyConfigClient.cs`
//! in the SoundSwitch sources (https://github.com/Belphemur/SoundSwitch).
//!
//! Vtable layout (0-based, methods after IUnknown):
//! 3: GetMixFormat, 4: GetDeviceFormat, 5: ResetDeviceFormat, 6: SetDeviceFormat,
//! 7: GetProcessingPeriod, 8: SetProcessingPeriod, 9: GetShareMode, 10: SetShareMode,
//! 11: GetPropertyValue, 12: SetPropertyValue, 13: SetDefaultEndpoint, 14: SetEndpointVisibility.

use std::ffi::c_void;

use windows::core::{IUnknown, Interface, GUID, HRESULT, PCWSTR};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

/// CLSID_CPolicyConfigClient.
#[allow(non_upper_case_globals)]
const CLSID_CPolicyConfigClient: GUID = GUID::from_u128(0x870AF99C_171D_4F9E_AF0D_E63DF40C2BC9);

/// IID candidates that we try in order (as in SoundSwitch,
/// `SoundSwitch.Audio.Manager/Interop/Interface/ComGuid.cs`).
///
/// IMPORTANT: all three interfaces have a DIFFERENT vtable layout, but the first two
/// (IPolicyConfigX and IPolicyConfig) are identical to the one described below.
/// IPolicyConfigVista has a different layout and is not among the candidates:
/// on Win10/11 one of the first two works.
///
/// Verified on a live system: the IID from older projects
/// (F8679F50-...-9C26-220C23D0139C) returns E_NOINTERFACE.
const POLICY_CONFIG_IIDS: [GUID; 2] = [
    // IPolicyConfigX (new Windows 10/11 builds).
    GUID::from_u128(0x8F9FB2AA_1C0B_4D54_B6BB_B2F2A10CE03C),
    // IPolicyConfig (Windows 7+).
    GUID::from_u128(0xF8679F50_850A_41CF_9C72_430F290290C8),
];

/// Audio device roles.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ERole {
    Console = 0,
    Multimedia = 1,
    Communications = 2,
}

/// IPolicyConfig vtable. Methods we do not call are declared as `usize` (they are
/// pointers anyway — the layout does not change) so their signatures need not be described.
#[allow(non_snake_case)]
#[repr(C)]
struct IPolicyConfigVtable {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: usize,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    // Slots 3–12: methods we do not call.
    get_mix_format: usize,
    get_device_format: usize,
    reset_device_format: usize,
    set_device_format: usize,
    get_processing_period: usize,
    set_processing_period: usize,
    get_share_mode: usize,
    set_share_mode: usize,
    get_property_value: usize,
    set_property_value: usize,
    // Slot 13: SetDefaultEndpoint(PCWSTR, ERole).
    set_default_endpoint: unsafe extern "system" fn(*mut c_void, PCWSTR, u32) -> HRESULT,
    // Slot 14: SetEndpointVisibility.
    set_endpoint_visibility: usize,
}

/// Wrapper around the CPolicyConfigClient COM object.
///
/// `com` — the actual address of the COM interface (it is what gets passed as `this`
/// when calling methods through the vtable).
struct PolicyConfig {
    com: *mut c_void,
}

impl PolicyConfig {
    /// Create IPolicyConfig via CoCreateInstance.
    fn new() -> Result<Self, PolicyError> {
        unsafe {
            let unknown: IUnknown = CoCreateInstance(&CLSID_CPolicyConfigClient, None, CLSCTX_ALL)
                .map_err(PolicyError::CreateInstance)?;

            // Ownership of the COM reference passes to PolicyConfig:
            // it is released manually via Release.
            let raw: *mut c_void = unknown.as_raw();
            std::mem::forget(unknown);

            // Try the IID candidates (SoundSwitch: first X, then the classic one).
            // An explicit QueryInterface instead of "the object as is" — resilient to
            // CPolicyConfigClient changing its default interface.
            let vtable = &**(raw as *const *const IPolicyConfigVtable);
            let mut policy_ptr: *mut c_void = std::ptr::null_mut();
            for iid in &POLICY_CONFIG_IIDS {
                let hr = (vtable.query_interface)(raw, iid, &mut policy_ptr);
                if hr.is_ok() && !policy_ptr.is_null() {
                    break;
                }
                policy_ptr = std::ptr::null_mut();
            }
            // The original reference is no longer needed: a successful QI returned a new one (+1 ref).
            (vtable.release)(raw);

            if policy_ptr.is_null() {
                return Err(PolicyError::QueryInterface(
                    windows::core::HRESULT(0x80004002_u32 as i32), // E_NOINTERFACE
                ));
            }

            Ok(Self { com: policy_ptr })
        }
    }

    /// Set the default device for all three roles.
    fn set_default_endpoint(&self, device_id: &str) -> Result<(), PolicyError> {
        unsafe {
            // UTF-16 string with a null terminator; outlives all the calls.
            let wide: Vec<u16> = device_id.encode_utf16().chain(Some(0)).collect();
            let wide_ptr = PCWSTR::from_raw(wide.as_ptr());

            // COM object: a pointer to a struct whose first field is a pointer to the vtable.
            let vtable = &**(self.com as *const *const IPolicyConfigVtable);

            let roles = [
                ERole::Console as u32,
                ERole::Multimedia as u32,
                ERole::Communications as u32,
            ];

            let mut failed = false;
            for role in roles {
                let hr = (vtable.set_default_endpoint)(self.com, wide_ptr, role);
                if hr.is_ok() {
                    log::debug!("SetDefaultEndpoint ok for role {:?}", role);
                } else {
                    log::warn!(
                        "SetDefaultEndpoint failed for role {:?}: 0x{:08x}",
                        role,
                        hr.0
                    );
                    failed = true;
                }
            }

            if failed {
                Err(PolicyError::SetDefault)
            } else {
                Ok(())
            }
        }
    }
}

impl Drop for PolicyConfig {
    fn drop(&mut self) {
        unsafe {
            let vtable = &**(self.com as *const *const IPolicyConfigVtable);
            (vtable.release)(self.com);
        }
    }
}

/// Set the default device.
pub fn set_default_endpoint(device_id: &str) -> Result<(), PolicyError> {
    let policy = PolicyConfig::new()?;
    policy.set_default_endpoint(device_id)
}

/// Errors of the policy module.
#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("COM instance creation failed: {0}")]
    CreateInstance(windows::core::Error),
    #[error("QueryInterface failed for all known IPolicyConfig IIDs (E_NOINTERFACE)")]
    QueryInterface(HRESULT),
    #[error("SetDefaultEndpoint failed")]
    SetDefault,
}
