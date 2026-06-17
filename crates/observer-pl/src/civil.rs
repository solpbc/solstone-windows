// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Epoch -> calendar conversion for the ingest `day` / `segment` keys.
//!
//! The journal validates `day` as `YYYYMMDD` and `segment` as `HHMMSS_LEN`. A
//! sealed segment's clock-aligned boundary is `index * period_secs` epoch
//! seconds (see `observer-segment`), so the uploader derives both keys from that
//! boundary. We compute UTC with pure integer arithmetic (Howard Hinnant's
//! civil-from-days algorithm) — no `chrono`, no timezone database, fully
//! deterministic and host-testable.

/// (year, month 1-12, day 1-31, hour, minute, second) in UTC for `epoch_secs`.
pub fn utc_parts(epoch_secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (epoch_secs / 86_400) as i64;
    let rem = (epoch_secs % 86_400) as u32;
    let hour = rem / 3600;
    let minute = (rem % 3600) / 60;
    let second = rem % 60;

    // civil_from_days (Hinnant): days are since 1970-01-01.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let year = if month <= 2 { y + 1 } else { y };

    (year, month, day, hour, minute, second)
}

/// `YYYYMMDD` for the segment's boundary instant (UTC).
pub fn day_string(epoch_secs: u64) -> String {
    let (y, m, d, _, _, _) = utc_parts(epoch_secs);
    format!("{y:04}{m:02}{d:02}")
}

/// `HHMMSS_LEN` segment key for a boundary at `epoch_secs` lasting `len_secs`.
pub fn segment_key_string(epoch_secs: u64, len_secs: u64) -> String {
    let (_, _, _, h, mi, s) = utc_parts(epoch_secs);
    format!("{h:02}{mi:02}{s:02}_{len_secs}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_zero_is_unix_birth() {
        assert_eq!(utc_parts(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(day_string(0), "19700101");
        assert_eq!(segment_key_string(0, 300), "000000_300");
    }

    #[test]
    fn known_timestamp_decodes() {
        // 2026-06-17T14:30:00Z = 1781706600
        let secs = 1_781_706_600;
        assert_eq!(utc_parts(secs), (2026, 6, 17, 14, 30, 0));
        assert_eq!(day_string(secs), "20260617");
        assert_eq!(segment_key_string(secs, 300), "143000_300");
    }

    #[test]
    fn leap_day_2024_decodes() {
        // 2024-02-29T00:00:00Z = 1709164800
        assert_eq!(utc_parts(1_709_164_800), (2024, 2, 29, 0, 0, 0));
    }

    #[test]
    fn day_keys_match_format_regex_shape() {
        for secs in [1_700_000_000u64, 1_781_015_400, 253_402_300_000] {
            let day = day_string(secs);
            assert_eq!(day.len(), 8);
            assert!(day.chars().all(|c| c.is_ascii_digit()));
            let seg = segment_key_string(secs, 300);
            let (hhmmss, len) = seg.split_once('_').unwrap();
            assert_eq!(hhmmss.len(), 6);
            assert!(hhmmss.chars().all(|c| c.is_ascii_digit()));
            assert_eq!(len, "300");
        }
    }
}
