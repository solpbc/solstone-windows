// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! RFC3339 UTC timestamp formatting.

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;

/// Format a SystemTime as `YYYY-MM-DDTHH:MM:SS.mmmZ` in UTC.
pub fn format_rfc3339_utc(now: SystemTime) -> String {
    let (seconds, millis) = epoch_parts(now);
    let days = div_floor(seconds, 86_400);
    let seconds_of_day = seconds - days * 86_400;
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Parse an RFC3339 UTC timestamp formatted as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub(crate) fn parse_rfc3339_utc(text: &str) -> Option<SystemTime> {
    let bytes = text.as_bytes();
    if bytes.len() != 24 {
        return None;
    }
    if bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'.'
        || bytes[23] != b'Z'
    {
        return None;
    }

    let parse_digits = |slice: &[u8]| -> Option<u32> {
        let mut val = 0u32;
        for &b in slice {
            if !b.is_ascii_digit() {
                return None;
            }
            val = val.checked_mul(10)?.checked_add((b - b'0') as u32)?;
        }
        Some(val)
    };

    let year = parse_digits(&bytes[0..4])? as i64;
    let month = parse_digits(&bytes[5..7])?;
    let day = parse_digits(&bytes[8..10])?;
    let hour = parse_digits(&bytes[11..13])?;
    let minute = parse_digits(&bytes[14..16])?;
    let second = parse_digits(&bytes[17..19])?;
    let millis = parse_digits(&bytes[20..23])?;

    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
        || millis > 999
    {
        return None;
    }

    let days = days_from_civil(year, month as i64, day as i64);
    let total_secs = days
        .checked_mul(86_400)?
        .checked_add((hour as i64).checked_mul(3_600)?)?
        .checked_add((minute as i64).checked_mul(60)?)?
        .checked_add(second as i64)?;

    if total_secs >= 0 {
        let millis_total = (total_secs as u64)
            .checked_mul(1000)?
            .checked_add(millis as u64)?;
        UNIX_EPOCH.checked_add(Duration::from_millis(millis_total))
    } else {
        let abs_secs = total_secs.unsigned_abs();
        let dur = Duration::from_secs(abs_secs);
        let base = UNIX_EPOCH.checked_sub(dur)?;
        base.checked_add(Duration::from_millis(millis as u64))
    }
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn epoch_parts(now: SystemTime) -> (i64, u32) {
    match now.duration_since(UNIX_EPOCH) {
        Ok(duration) => (duration.as_secs() as i64, duration.subsec_millis()),
        Err(error) => {
            let duration = error.duration();
            let seconds = duration.as_secs() as i64;
            let nanos = duration.subsec_nanos();
            if nanos == 0 {
                (-seconds, 0)
            } else {
                (-seconds - 1, 1_000 - (nanos / 1_000_000))
            }
        }
    }
}

fn div_floor(a: i64, b: i64) -> i64 {
    let q = a / b;
    let r = a % b;
    if r != 0 && ((r > 0) != (b > 0)) {
        q - 1
    } else {
        q
    }
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let y = y + if m <= 2 { 1 } else { 0 };
    (y, m, d)
}

/// tracing-subscriber timer adapter over the shared formatter.
#[derive(Debug, Clone, Copy, Default)]
pub struct Rfc3339Utc;

impl Rfc3339Utc {
    #[cfg(test)]
    pub(crate) fn format_for_test(self, now: SystemTime) -> String {
        format_rfc3339_utc(now)
    }
}

impl FormatTime for Rfc3339Utc {
    fn format_time(&self, writer: &mut Writer<'_>) -> fmt::Result {
        write!(writer, "{}", format_rfc3339_utc(SystemTime::now()))
    }
}
