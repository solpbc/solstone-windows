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

/// Days since 1970-01-01 for a civil date — the exact inverse of the
/// civil-from-days step in [`utc_parts`] (Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if month > 2 { month - 3 } else { month + 9 } as i64; // [0, 11]
    let doy = (153 * mp + 2) / 5 + day as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// The epoch second whose device-local wall clock under `offset_secs` is exactly
/// the supplied civil date and time — the inverse of [`day_string_local`] +
/// [`segment_key_string_local`].
///
/// Returns `None` for a date/time that is not a real calendar instant or that
/// falls outside the representable epoch range, so a caller-named identity can
/// never be fabricated from nonsense. Round-tripping the result back through
/// [`utc_parts`] is what earns the name.
pub fn epoch_from_local_parts(
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    offset_secs: i64,
) -> Option<u64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    // Reject a day that does not exist in this month (e.g. 20260231): the
    // round-trip through civil-from-days lands on a different date.
    let (ry, rm, rd, _, _, _) = utc_parts(u64::try_from(days.checked_mul(86_400)?).ok()?);
    if (ry, rm, rd) != (year, month, day) {
        return None;
    }
    let local = days
        .checked_mul(86_400)?
        .checked_add(i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second))?;
    u64::try_from(local.checked_sub(offset_secs)?).ok()
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
    fn epoch_from_local_parts_inverts_utc_parts() {
        for secs in [0u64, 1_700_000_000, 1_781_706_600, 1_709_164_800] {
            let (y, m, d, h, mi, s) = utc_parts(secs);
            assert_eq!(
                epoch_from_local_parts(y, m, d, h, mi, s, 0),
                Some(secs),
                "round trip for {secs}"
            );
        }
    }

    #[test]
    fn epoch_from_local_parts_applies_the_offset() {
        // 2026-06-17 07:30:00 local at UTC-7 is 2026-06-17T14:30:00Z.
        let epoch = epoch_from_local_parts(2026, 6, 17, 7, 30, 0, -7 * 3600).unwrap();
        assert_eq!(epoch, 1_781_706_600);
        assert_eq!(day_string_local(epoch, -7 * 3600), "20260617");
        assert_eq!(
            segment_key_string_local(epoch, -7 * 3600, 300),
            "073000_300"
        );
    }

    #[test]
    fn epoch_from_local_parts_rejects_impossible_civil_values() {
        assert_eq!(epoch_from_local_parts(2026, 2, 31, 0, 0, 0, 0), None);
        assert_eq!(epoch_from_local_parts(2026, 13, 1, 0, 0, 0, 0), None);
        assert_eq!(epoch_from_local_parts(2026, 0, 1, 0, 0, 0, 0), None);
        assert_eq!(epoch_from_local_parts(2026, 1, 0, 0, 0, 0, 0), None);
        assert_eq!(epoch_from_local_parts(2026, 1, 1, 24, 0, 0, 0), None);
        assert_eq!(epoch_from_local_parts(2026, 1, 1, 0, 60, 0, 0), None);
        assert_eq!(epoch_from_local_parts(2026, 1, 1, 0, 0, 60, 0), None);
        // Pre-epoch instants are not representable as a segment boundary.
        assert_eq!(epoch_from_local_parts(1969, 12, 31, 23, 59, 59, 0), None);
    }

    #[test]
    fn epoch_from_local_parts_accepts_a_real_leap_day() {
        let epoch = epoch_from_local_parts(2024, 2, 29, 0, 0, 0, 0).unwrap();
        assert_eq!(epoch, 1_709_164_800);
        assert_eq!(day_string_local(epoch, 0), "20240229");
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
