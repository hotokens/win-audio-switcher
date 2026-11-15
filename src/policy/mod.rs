//! Смена устройства вывода по умолчанию через IPolicyConfig.
//!
//! Windows не предоставляет публичного API для смены default-устройства.
//! Используем недокументированный COM-класс `CPolicyConfigClient` с интерфейсом
//! `IPolicyConfig`, как это делают SoundSwitch и EarTrumpet.
//!
//! Источник: SoundSwitch / `SoundSwitch/Audio/PolicyConfigClient.cs`
//! в исходниках SoundSwitch (https://github.com/Belphemur/SoundSwitch).
//!
//! Раскладка vtable (0-based, методы после IUnknown):
//! 3: GetMixFormat, 4: GetDeviceFormat, 5: ResetDeviceFormat, 6: SetDeviceFormat,
//! 7: GetProcessingPeriod, 8: SetProcessingPeriod, 9: GetShareMode, 10: SetShareMode,
//! 11: GetPropertyValue, 12: SetPropertyValue, 13: SetDefaultEndpoint, 14: SetEndpointVisibility.

use std::ffi::c_void;

use windows::core::{IUnknown, Interface, GUID, HRESULT, PCWSTR};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

/// CLSID_CPolicyConfigClient.
#[allow(non_upper_case_globals)]
const CLSID_CPolicyConfigClient: GUID = GUID::from_u128(0x870AF99C_171D_4F9E_AF0D_E63DF40C2BC9);

/// Кандидаты IID, которые пробуем по порядку (как в SoundSwitch,
/// `SoundSwitch.Audio.Manager/Interop/Interface/ComGuid.cs`).
///
/// ВАЖНО: у всех трёх интерфейсов РАЗНАЯ раскладка vtable, но первые два
/// (IPolicyConfigX и IPolicyConfig) идентичны той, что описана ниже.
/// IPolicyConfigVista имеет другую раскладку и в перебор не входит:
/// на Win10/11 срабатывает один из первых двух.
///
/// Проверено на живой системе: IID из старых проектов
/// (F8679F50-...-9C26-220C23D0139C) даёт E_NOINTERFACE.
const POLICY_CONFIG_IIDS: [GUID; 2] = [
    // IPolicyConfigX (новые сборки Windows 10/11).
    GUID::from_u128(0x8F9FB2AA_1C0B_4D54_B6BB_B2F2A10CE03C),
    // IPolicyConfig (Windows 7+).
    GUID::from_u128(0xF8679F50_850A_41CF_9C72_430F290290C8),
];

/// Роли аудиоустройств.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ERole {
    Console = 0,
    Multimedia = 1,
    Communications = 2,
}

/// Vtable IPolicyConfig. Не вызываемые методы объявлены `usize` (это всё равно
/// указатели — раскладка не меняется), чтобы не описывать их сигнатуры.
#[allow(non_snake_case)]
#[repr(C)]
struct IPolicyConfigVtable {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: usize,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    // Слоты 3–12: методы, которые мы не вызываем.
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
    // Слот 13: SetDefaultEndpoint(PCWSTR, ERole).
    set_default_endpoint: unsafe extern "system" fn(*mut c_void, PCWSTR, u32) -> HRESULT,
    // Слот 14: SetEndpointVisibility.
    set_endpoint_visibility: usize,
}

/// Обёртка над COM-объектом CPolicyConfigClient.
///
/// `com` — настоящий адрес COM-интерфейса (именно он передаётся как `this`
/// при вызовах методов через vtable).
struct PolicyConfig {
    com: *mut c_void,
}

impl PolicyConfig {
    /// Создать IPolicyConfig через CoCreateInstance.
    fn new() -> Result<Self, PolicyError> {
        unsafe {
            let unknown: IUnknown = CoCreateInstance(&CLSID_CPolicyConfigClient, None, CLSCTX_ALL)
                .map_err(PolicyError::CreateInstance)?;

            // Владение COM-ссылкой переходит в PolicyConfig:
            // освобождаем вручную через Release.
            let raw: *mut c_void = unknown.as_raw();
            std::mem::forget(unknown);

            // Перебираем IID-кандидаты (SoundSwitch: сначала X, потом классика).
            // Явный QueryInterface вместо «объект как есть» — устойчиво к смене
            // интерфейса по умолчанию у CPolicyConfigClient.
            let vtable = &**(raw as *const *const IPolicyConfigVtable);
            let mut policy_ptr: *mut c_void = std::ptr::null_mut();
            for iid in &POLICY_CONFIG_IIDS {
                let hr = (vtable.query_interface)(raw, iid, &mut policy_ptr);
                if hr.is_ok() && !policy_ptr.is_null() {
                    break;
                }
                policy_ptr = std::ptr::null_mut();
            }
            // Исходная ссылка больше не нужна: QI в случае успеха дал новую (+1 ref).
            (vtable.release)(raw);

            if policy_ptr.is_null() {
                return Err(PolicyError::QueryInterface(
                    windows::core::HRESULT(0x80004002_u32 as i32), // E_NOINTERFACE
                ));
            }

            Ok(Self { com: policy_ptr })
        }
    }

    /// Установить устройство по умолчанию для всех трёх ролей.
    fn set_default_endpoint(&self, device_id: &str) -> Result<(), PolicyError> {
        unsafe {
            // Строка в UTF-16 с нулевым терминатором; живёт дольше всех вызовов.
            let wide: Vec<u16> = device_id.encode_utf16().chain(Some(0)).collect();
            let wide_ptr = PCWSTR::from_raw(wide.as_ptr());

            // COM-объект: указатель на структуру, чьё первое поле — указатель на vtable.
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

/// Установить устройство по умолчанию.
pub fn set_default_endpoint(device_id: &str) -> Result<(), PolicyError> {
    let policy = PolicyConfig::new()?;
    policy.set_default_endpoint(device_id)
}

/// Ошибки policy-модуля.
#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("COM instance creation failed: {0}")]
    CreateInstance(windows::core::Error),
    #[error("QueryInterface failed for all known IPolicyConfig IIDs (E_NOINTERFACE)")]
    QueryInterface(HRESULT),
    #[error("SetDefaultEndpoint failed")]
    SetDefault,
}
