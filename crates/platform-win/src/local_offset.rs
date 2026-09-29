// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Device-local zone provider.
//!
//! One lookup reads the current Windows zone and applies that snapshot to the
//! instant. The IANA id, when System ICU maps that same Windows zone id, is
//! returned with the offset. A missing mapping omits the id and still returns
//! the offset. Nothing is cached: the next upload reads the zone again.

#[cfg(windows)]
use observer_model::accept_iana_tz;
use observer_model::{LocalOffset, LocalOffsetError, LocalZone};

#[cfg(windows)]
const WINDOWS_EPOCH_OFFSET_SECS: u64 = 11_644_473_600;
#[cfg(windows)]
const FILETIME_TICKS_PER_SEC: u64 = 10_000_000;
#[cfg(windows)]
const ICU_DLL: [u16; 8] = [
    b'i' as u16,
    b'c' as u16,
    b'u' as u16,
    b'.' as u16,
    b'd' as u16,
    b'l' as u16,
    b'l' as u16,
    0,
];
#[cfg(windows)]
const UCAL_WINDOWS_ID: &[u8] = b"ucal_getTimeZoneIDForWindowsID\0";

/// Platform zone lookup. Off Windows this is an honest unsupported seam,
/// never a UTC fallback.
#[derive(Debug, Default)]
pub struct WindowsLocalOffset;

#[cfg(windows)]
impl LocalOffset for WindowsLocalOffset {
    fn local_zone(&self, epoch_secs: u64) -> Result<LocalZone, LocalOffsetError> {
        use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
        use windows::Win32::System::Time::{
            FileTimeToSystemTime, GetDynamicTimeZoneInformation, SystemTimeToFileTime,
            SystemTimeToTzSpecificLocalTimeEx, DYNAMIC_TIME_ZONE_INFORMATION, TIME_ZONE_ID_INVALID,
        };

        let utc_ticks = epoch_secs
            .checked_add(WINDOWS_EPOCH_OFFSET_SECS)
            .and_then(|secs| secs.checked_mul(FILETIME_TICKS_PER_SEC))
            .ok_or(LocalOffsetError::Lookup)?;
        let utc_ft = filetime_from_ticks(utc_ticks);
        let mut utc_st = SYSTEMTIME::default();
        let mut local_st = SYSTEMTIME::default();
        let mut local_ft = FILETIME::default();
        let mut zone = DYNAMIC_TIME_ZONE_INFORMATION::default();

        unsafe {
            FileTimeToSystemTime(&utc_ft, &mut utc_st).map_err(|_| LocalOffsetError::Lookup)?;
            if GetDynamicTimeZoneInformation(&mut zone) == TIME_ZONE_ID_INVALID {
                return Err(LocalOffsetError::Lookup);
            }
            // Apply this snapshot. A second read of the current zone could
            // name a different zone than the offset.
            SystemTimeToTzSpecificLocalTimeEx(
                Some(std::ptr::from_ref(&zone)),
                &utc_st,
                &mut local_st,
            )
            .map_err(|_| LocalOffsetError::Lookup)?;
            SystemTimeToFileTime(&local_st, &mut local_ft).map_err(|_| LocalOffsetError::Lookup)?;
        }

        let local_ticks = ticks_from_filetime(local_ft);
        let utc_offset_seconds =
            ((local_ticks as i64) - (utc_ticks as i64)) / FILETIME_TICKS_PER_SEC as i64;
        let tz = windows_zone_name(&zone.TimeZoneKeyName).and_then(iana_id_for_windows_zone);
        Ok(LocalZone {
            utc_offset_seconds,
            tz,
        })
    }
}

#[cfg(not(windows))]
impl LocalOffset for WindowsLocalOffset {
    fn local_zone(&self, _epoch_secs: u64) -> Result<LocalZone, LocalOffsetError> {
        Err(LocalOffsetError::Unsupported)
    }
}

#[cfg(windows)]
fn filetime_from_ticks(ticks: u64) -> windows::Win32::Foundation::FILETIME {
    windows::Win32::Foundation::FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    }
}

#[cfg(windows)]
fn ticks_from_filetime(filetime: windows::Win32::Foundation::FILETIME) -> u64 {
    (u64::from(filetime.dwHighDateTime) << 32) | u64::from(filetime.dwLowDateTime)
}

#[cfg(windows)]
fn windows_zone_name(key: &[u16; 128]) -> Option<&[u16]> {
    let end = key.iter().position(|unit| *unit == 0)?;
    if end == 0 {
        None
    } else {
        Some(&key[..end])
    }
}

#[cfg(windows)]
type UcalGetTimeZoneIdForWindowsId = unsafe extern "system" fn(
    winid: *const u16,
    len: i32,
    region: *const std::ffi::c_char,
    id: *mut u16,
    id_capacity: i32,
    status: *mut i32,
) -> i32;

/// Map the Windows zone id through System ICU's default (001) mapping.
/// Missing DLL, missing symbol, or a rejected result omits the id.
#[cfg(windows)]
fn iana_id_for_windows_zone(name: &[u16]) -> Option<String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{FreeLibrary, HANDLE};
    use windows::Win32::System::LibraryLoader::{LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};

    if name.is_empty() {
        return None;
    }
    // System ICU only: a bare-name load would also search the app's own,
    // possibly user-writable, directory.
    let module = unsafe {
        LoadLibraryExW(
            PCWSTR(ICU_DLL.as_ptr()),
            HANDLE::default(),
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    }
    .ok()?;
    let mapped = unsafe { icu_windows_zone_id(module, name) };
    let _ = unsafe { FreeLibrary(module) };
    accept_iana_tz(&mapped?).map(str::to_owned)
}

#[cfg(windows)]
unsafe fn icu_windows_zone_id(
    module: windows::Win32::Foundation::HMODULE,
    name: &[u16],
) -> Option<String> {
    use windows::core::PCSTR;
    use windows::Win32::System::LibraryLoader::GetProcAddress;

    let symbol = GetProcAddress(module, PCSTR(UCAL_WINDOWS_ID.as_ptr()))?;
    let func: UcalGetTimeZoneIdForWindowsId = std::mem::transmute(symbol);
    let mut status = 0i32;
    let mut id = [0u16; 128];
    let len = func(
        name.as_ptr(),
        i32::try_from(name.len()).ok()?,
        std::ptr::null(),
        id.as_mut_ptr(),
        i32::try_from(id.len()).ok()?,
        &mut status,
    );
    if status > 0 || len <= 0 {
        return None;
    }
    let len = usize::try_from(len).ok()?;
    if len >= id.len() {
        return None;
    }
    let mapped = String::from_utf16(&id[..len]).ok()?;
    if mapped.is_empty() {
        None
    } else {
        Some(mapped)
    }
}
