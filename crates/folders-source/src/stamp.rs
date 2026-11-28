// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Pure local timestamp and five-minute window math built on `observer_pl::civil`.
//! Keeps `folders-source` free of platform timezone crates while matching the
//! journal's `YYYYMMDD` / `HHMMSS_300` grid and RFC 3339 wire formatting.

use observer_pl::civil::utc_parts;
use serde::{Deserialize, Serialize};

/// Length of one folder-source window in seconds.
pub const WINDOW_SECS: i64 = 300;
const WINDOW_MS: i64 = WINDOW_SECS * 1_000;
const DAY_SECS: i64 = 86_400;

/// One five-minute window key in the owner's local calendar.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Window {
    pub day: String,
    pub segment: String,
}

/// An instant paired with the device-local UTC offset (seconds east of UTC)
/// that applied at that instant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Stamp {
    pub ms: i64,
    pub offset_s: i32,
}

impl Stamp {
    #[must_use]
    pub const fn new(ms: i64, offset_s: i32) -> Self {
        Self { ms, offset_s }
    }

    /// The five-minute window `(day, segment)` containing this stamp in local wall time.
    #[must_use]
    pub fn window(self) -> Window {
        let local_secs = self
            .ms
            .div_euclid(1_000)
            .saturating_add(i64::from(self.offset_s));
        let floored_local = local_secs.saturating_sub(local_secs.rem_euclid(WINDOW_SECS));
        let epoch = floored_local.max(0) as u64;
        let (y, m, d, h, mi, s) = utc_parts(epoch);
        Window {
            day: format!("{y:04}{m:02}{d:02}"),
            segment: format!("{h:02}{mi:02}{s:02}_{WINDOW_SECS}"),
        }
    }

    /// The UTC epoch millisecond at which this stamp's five-minute local window closes.
    #[must_use]
    pub fn window_end_ms(self) -> i64 {
        let offset_ms = i64::from(self.offset_s).saturating_mul(1_000);
        let local_ms = self.ms.saturating_add(offset_ms);
        let floored_local = local_ms.saturating_sub(local_ms.rem_euclid(WINDOW_MS));
        floored_local
            .saturating_add(WINDOW_MS)
            .saturating_sub(offset_ms)
    }

    /// RFC 3339 timestamp with explicit numeric offset (e.g. `2026-11-27T14:06:10-07:00`
    /// or `2026-11-27T21:06:10Z` when `offset_s == 0`).
    #[must_use]
    pub fn rfc3339(self) -> String {
        let local_secs = self
            .ms
            .div_euclid(1_000)
            .saturating_add(i64::from(self.offset_s));
        let epoch = local_secs.max(0) as u64;
        let (y, m, d, h, mi, s) = utc_parts(epoch);
        if self.offset_s == 0 {
            format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
        } else {
            let sign = if self.offset_s < 0 { '-' } else { '+' };
            let abs = i64::from(self.offset_s).abs();
            let oh = abs / 3600;
            let om = (abs % 3600) / 60;
            format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}{sign}{oh:02}:{om:02}")
        }
    }

    /// UTC epoch milliseconds of local midnight at the start of `self`'s local day.
    #[must_use]
    pub fn local_midnight_ms(self) -> i64 {
        let offset_ms = i64::from(self.offset_s).saturating_mul(1_000);
        let day_ms = DAY_SECS * 1_000;
        let local_ms = self.ms.saturating_add(offset_ms);
        local_ms
            .saturating_sub(local_ms.rem_euclid(day_ms))
            .saturating_sub(offset_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_and_rfc3339_match_local_offset_grid() {
        // 2026-11-27T21:06:10Z == 2026-11-27T14:06:10-07:00
        let ms = 1_795_813_570_000i64;
        let offset_s = -7 * 3600;
        let stamp = Stamp::new(ms, offset_s);

        assert_eq!(stamp.rfc3339(), "2026-11-27T14:06:10-07:00");
        assert_eq!(
            stamp.window(),
            Window {
                day: "20261127".into(),
                segment: "140500_300".into(),
            }
        );
        // Window is [14:05:00, 14:10:00) local => ends 230s after 14:06:10.
        assert_eq!(stamp.window_end_ms(), ms + 230_000);
        assert_eq!(
            Stamp::new(stamp.window_end_ms(), offset_s).rfc3339(),
            "2026-11-27T14:10:00-07:00"
        );
    }

    #[test]
    fn utc_stamp_formats_with_z_and_non_whole_hour_offsets_work() {
        let ms = 1_795_813_570_000i64;
        assert_eq!(Stamp::new(ms, 0).rfc3339(), "2026-11-27T21:06:10Z");
        // India Standard Time (+05:30)
        let ist = Stamp::new(ms, 5 * 3600 + 30 * 60);
        assert_eq!(ist.rfc3339(), "2026-11-28T02:36:10+05:30");
        assert_eq!(
            ist.window(),
            Window {
                day: "20261128".into(),
                segment: "023500_300".into(),
            }
        );
    }
}
