// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Device-local UTC offset provider.

use observer_model::{LocalOffset, LocalOffsetError};

#[cfg(windows)]
const WINDOWS_EPOCH_OFFSET_SECS: u64 = 11_644_473_600;
#[cfg(windows)]
const FILETIME_TICKS_PER_SEC: u64 = 10_000_000;

/// Platform local-offset lookup. Off Windows this is an honest unsupported seam,
/// never a UTC fallback.
#[derive(Debug, Default)]
pub struct WindowsLocalOffset;

#[cfg(windows)]
impl LocalOffset for WindowsLocalOffset {
    fn local_offset_secs(&self, epoch_secs: u64) -> Result<i64, LocalOffsetError> {
        use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
        use windows::Win32::System::Time::{
            FileTimeToSystemTime, SystemTimeToFileTime, SystemTimeToTzSpecificLocalTime,
        };

        let utc_ticks = epoch_secs
            .checked_add(WINDOWS_EPOCH_OFFSET_SECS)
            .and_then(|secs| secs.checked_mul(FILETIME_TICKS_PER_SEC))
            .ok_or(LocalOffsetError::Lookup)?;
        let utc_ft = filetime_from_ticks(utc_ticks);
        let mut utc_st = SYSTEMTIME::default();
        let mut local_st = SYSTEMTIME::default();
        let mut local_ft = FILETIME::default();

        unsafe {
            FileTimeToSystemTime(&utc_ft, &mut utc_st).map_err(|_| LocalOffsetError::Lookup)?;
            SystemTimeToTzSpecificLocalTime(None, &utc_st, &mut local_st)
                .map_err(|_| LocalOffsetError::Lookup)?;
            SystemTimeToFileTime(&local_st, &mut local_ft).map_err(|_| LocalOffsetError::Lookup)?;
        }

        let local_ticks = ticks_from_filetime(local_ft);
        Ok(((local_ticks as i64) - (utc_ticks as i64)) / FILETIME_TICKS_PER_SEC as i64)
    }
}

#[cfg(not(windows))]
impl LocalOffset for WindowsLocalOffset {
    fn local_offset_secs(&self, _epoch_secs: u64) -> Result<i64, LocalOffsetError> {
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
