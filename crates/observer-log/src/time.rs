// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! RFC3339 UTC timestamp formatting.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

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
