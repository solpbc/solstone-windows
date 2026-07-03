// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Epoch -> calendar conversion for the ingest `day` / `segment` keys.
//!
//! The journal validates `day` as `YYYYMMDD` and `segment` as `HHMMSS_LEN`. A
//! sealed segment's clock-aligned boundary is `index * period_secs` epoch
//! seconds (see `observer-segment`), so the uploader derives both keys from the
//! same boundary plus the injected device-local UTC offset for that instant. We
//! compute calendar parts with pure integer arithmetic (Howard Hinnant's
//! civil-from-days algorithm) after applying the offset — no `chrono`, no
//! timezone database, fully deterministic and host-testable.

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

/// `YYYYMMDD` for the boundary in device-local wall clock (`offset_secs` = local-UTC).
pub fn day_string_local(boundary_epoch_secs: u64, offset_secs: i64) -> String {
    let shifted = boundary_epoch_secs as i64 + offset_secs;
    debug_assert!(shifted >= 0, "shifted instant underflow");
    let (y, m, d, _, _, _) = utc_parts(shifted as u64);
    format!("{y:04}{m:02}{d:02}")
}

/// `HHMMSS_LEN` for the boundary in device-local wall clock.
pub fn segment_key_string_local(
    boundary_epoch_secs: u64,
    offset_secs: i64,
    len_secs: u64,
) -> String {
    let shifted = boundary_epoch_secs as i64 + offset_secs;
    debug_assert!(shifted >= 0, "shifted instant underflow");
    let (_, _, _, h, mi, s) = utc_parts(shifted as u64);
    format!("{h:02}{mi:02}{s:02}_{len_secs}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_zero_is_unix_birth() {
        assert_eq!(utc_parts(0), (1970, 1, 1, 0, 0, 0));
    }

    #[test]
    fn known_timestamp_decodes() {
        // 2026-06-17T14:30:00Z = 1781706600
        let secs = 1_781_706_600;
        assert_eq!(utc_parts(secs), (2026, 6, 17, 14, 30, 0));
    }

    #[test]
    fn leap_day_2024_decodes() {
        // 2024-02-29T00:00:00Z = 1709164800
        assert_eq!(utc_parts(1_709_164_800), (2024, 2, 29, 0, 0, 0));
    }

    #[test]
    fn local_keys_with_zero_offset_match_utc_parts() {
        assert_eq!(day_string_local(0, 0), "19700101");
        assert_eq!(segment_key_string_local(0, 0, 300), "000000_300");

        let secs = 1_781_706_600;
        assert_eq!(day_string_local(secs, 0), "20260617");
        assert_eq!(segment_key_string_local(secs, 0, 300), "143000_300");
    }

    #[test]
    fn negative_offset_can_cross_to_previous_local_day() {
        // 2026-06-18T02:30:00Z at UTC-7 is 2026-06-17 19:30:00 local.
        let boundary = 1_781_749_800;
        let offset = -7 * 3600;
        assert_eq!(utc_parts(boundary), (2026, 6, 18, 2, 30, 0));
        assert_eq!(day_string_local(boundary, offset), "20260617");
        assert_eq!(
            segment_key_string_local(boundary, offset, 300),
            "193000_300"
        );
    }

    #[test]
    fn dst_spring_forward_uses_supplied_post_transition_offset() {
        // 2026-03-08T07:00:00Z at UTC-4 is 2026-03-08 03:00:00 local.
        let boundary = 1_772_953_200;
        let offset = -4 * 3600;
        assert_eq!(utc_parts(boundary), (2026, 3, 8, 7, 0, 0));
        assert_eq!(day_string_local(boundary, offset), "20260308");
        assert_eq!(
            segment_key_string_local(boundary, offset, 300),
            "030000_300"
        );
    }

    #[test]
    fn dst_fall_back_fold_can_collide_by_design() {
        // The journal remaps collisions: these two UTC boundaries are one hour
        // apart, but the DST fold maps both to 01:30 local with different offsets.
        let before_fold = 1_793_511_000; // 2026-11-01T05:30:00Z at UTC-4.
        let after_fold = before_fold + 3600; // 2026-11-01T06:30:00Z at UTC-5.
        let first = segment_key_string_local(before_fold, -4 * 3600, 300);
        let second = segment_key_string_local(after_fold, -5 * 3600, 300);
        assert_eq!(first, "013000_300");
        assert_eq!(first, second);
    }

    #[test]
    fn local_keys_match_format_regex_shape() {
        for secs in [1_700_000_000u64, 1_781_015_400, 253_402_300_000] {
            let day = day_string_local(secs, 0);
            assert_eq!(day.len(), 8);
            assert!(day.chars().all(|c| c.is_ascii_digit()));
            let seg = segment_key_string_local(secs, 0, 300);
            let (hhmmss, len) = seg.split_once('_').unwrap();
            assert_eq!(hhmmss.len(), 6);
            assert!(hhmmss.chars().all(|c| c.is_ascii_digit()));
            assert_eq!(len, "300");
        }
    }
}
