// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The source's rules as plain functions: which files count, when each was
//! taken, how far back a span reaches, the stamp the journal reads, and what
//! the journal's answer means. Nothing here touches the disk or the clock.

use serde::{Deserialize, Serialize};

/// The journal keeps an upload's file up to this size.
pub const MAX_BYTES: u64 = 64 * 1024 * 1024;
/// The journal keeps an image up to this many pixels, by its own header.
pub const MAX_PIXELS: u64 = 100_000_000;
/// A file changed more recently than this may still be being written.
pub const STABLE_MS: i64 = 2_000;

const DAY_SECS: i64 = 86_400;

/// How far back the owner chose to start.
#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Span {
    #[default]
    ThirtyDays,
    SixMonths,
    Year,
    All,
    Nothing,
}

impl Span {
    pub const ALL: [Span; 5] = [
        Span::ThirtyDays,
        Span::SixMonths,
        Span::Year,
        Span::All,
        Span::Nothing,
    ];
}

/// Where the capture time came from, as the journal names it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TakenFrom {
    Platform,
    File,
}

impl TakenFrom {
    pub fn as_str(self) -> &'static str {
        match self {
            TakenFrom::Platform => "platform",
            TakenFrom::File => "file",
        }
    }
}

/// What kind of image a file is, by its extension.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageKind {
    /// Sent byte for byte.
    Accepted,
    /// Converted to PNG on this computer, then sent.
    Convertible,
}

/// The image kind of a lowercase extension, or `None` for anything that isn't
/// an image the source takes (videos included).
pub fn image_kind(ext: &str) -> Option<ImageKind> {
    match ext {
        "png" | "jpg" | "jpeg" | "webp" => Some(ImageKind::Accepted),
        "bmp" | "gif" | "tif" | "tiff" | "heic" | "heif" | "jxr" | "wdp" | "avif" => {
            Some(ImageKind::Convertible)
        }
        _ => None,
    }
}

/// The journal's content type for an accepted extension.
pub fn content_type(ext: &str) -> &'static str {
    match ext {
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        _ => "image/png",
    }
}

/// The lowercase extension and the stem of a file name.
pub fn split_name(name: &str) -> (String, &str) {
    match name.rfind('.') {
        Some(dot) if dot > 0 => (name[dot + 1..].to_ascii_lowercase(), &name[..dot]),
        _ => (String::new(), name),
    }
}

/// A name that is never a finished screenshot: the folder's settings file, a
/// temporary or partial download, an editor's lock file.
pub fn is_scratch_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower == "desktop.ini" || lower == "thumbs.db" {
        return true;
    }
    if lower.starts_with("~$") || lower.starts_with('.') {
        return true;
    }
    [
        ".tmp",
        ".temp",
        ".part",
        ".partial",
        ".crdownload",
        ".download",
        ".opdownload",
        ".!ut",
    ]
    .iter()
    .any(|suffix| lower.ends_with(suffix))
}

/// A local date and time read from a file name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalStamp {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

impl LocalStamp {
    /// Seconds since the epoch as if this local time were UTC.
    pub fn as_local_secs(self) -> i64 {
        days_from_civil(self.year, self.month, self.day) * DAY_SECS
            + i64::from(self.hour) * 3600
            + i64::from(self.minute) * 60
            + i64::from(self.second)
    }
}

/// Strip a trailing ` (N)` copy number.
fn without_copy_number(stem: &str) -> &str {
    if let Some(open) = stem.rfind(" (") {
        let inner = &stem[open + 2..];
        if let Some(number) = inner.strip_suffix(')') {
            if !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()) {
                return &stem[..open];
            }
        }
    }
    stem
}

/// The local time in a Snipping Tool name: `<words> YYYY-MM-DD HHMMSS`, with an
/// optional ` (N)`. The words are any, so a localized name counts too.
pub fn snipping_tool_time(stem: &str) -> Option<LocalStamp> {
    let stem = without_copy_number(stem);
    // "YYYY-MM-DD HHMMSS" is 17 bytes, after a space and at least one word.
    let bytes = stem.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let tail = &bytes[bytes.len() - 17..];
    let before = &stem[..stem.len() - 17];
    let words = before.strip_suffix(' ')?;
    if words.trim().is_empty() {
        return None;
    }
    let digits = |range: std::ops::Range<usize>| -> Option<u32> {
        let part = &tail[range];
        if !part.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(part).ok()?.parse().ok()
    };
    if tail[4] != b'-' || tail[7] != b'-' || tail[10] != b' ' {
        return None;
    }
    let stamp = LocalStamp {
        year: i64::from(digits(0..4)?),
        month: digits(5..7)?,
        day: digits(8..10)?,
        hour: digits(11..13)?,
        minute: digits(13..15)?,
        second: digits(15..17)?,
    };
    let valid = (1..=12).contains(&stamp.month)
        && stamp.day >= 1
        && stamp.day <= days_in_month(stamp.year, stamp.month)
        && stamp.hour < 24
        && stamp.minute < 60
        && stamp.second < 60;
    valid.then_some(stamp)
}

/// Windows' older name for a Win+PrtScn capture: `Screenshot (N)`.
pub fn is_legacy_screenshot_name(stem: &str) -> bool {
    let Some(rest) = stem.strip_prefix("Screenshot (") else {
        return false;
    };
    let Some(number) = rest.strip_suffix(')') else {
        return false;
    };
    !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())
}

/// Whether Windows' own tools named this file as a screenshot.
pub fn has_name_stamp(stem: &str) -> bool {
    snipping_tool_time(stem).is_some() || is_legacy_screenshot_name(stem)
}

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const GAME_BAR_KEYWORD: &[u8] = b"Microsoft.GameDVR.";

/// Whether a PNG carries Game Bar's marker: an `iTXt` chunk whose keyword
/// begins `Microsoft.GameDVR.`.
pub fn has_game_bar_marker(bytes: &[u8]) -> bool {
    png_chunks(bytes).any(|(kind, data)| kind == *b"iTXt" && data.starts_with(GAME_BAR_KEYWORD))
}

/// The chunks of a PNG, in order, up to `IEND` or the first malformed one.
fn png_chunks(bytes: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    let mut at = if bytes.starts_with(PNG_SIGNATURE) {
        8
    } else {
        bytes.len()
    };
    std::iter::from_fn(move || {
        let header = bytes.get(at..at + 8)?;
        let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let kind = [header[4], header[5], header[6], header[7]];
        let data = bytes.get(at + 8..at.checked_add(8 + len)?)?;
        at = at.checked_add(12 + len)?;
        if kind == *b"IEND" {
            at = bytes.len();
        }
        Some((kind, data))
    })
}

/// The pixel count by the image's own header (PNG, JPEG, WebP), when it reads.
pub fn header_pixels(bytes: &[u8]) -> Option<u64> {
    let (width, height) = png_size(bytes)
        .or_else(|| jpeg_size(bytes))
        .or_else(|| webp_size(bytes))?;
    Some(u64::from(width) * u64::from(height))
}

fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let (kind, data) = png_chunks(bytes).next()?;
    if kind != *b"IHDR" || data.len() < 8 {
        return None;
    }
    let width = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    let height = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    Some((width, height))
}

fn jpeg_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if !bytes.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut at = 2;
    while at + 4 <= bytes.len() {
        if bytes[at] != 0xFF {
            return None;
        }
        let marker = bytes[at + 1];
        if marker == 0xFF {
            at += 1;
            continue;
        }
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            at += 2;
            continue;
        }
        let len = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
        let is_frame = (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if is_frame {
            let frame = bytes.get(at + 4..at + 2 + len)?;
            if frame.len() < 5 {
                return None;
            }
            let height = u32::from(u16::from_be_bytes([frame[1], frame[2]]));
            let width = u32::from(u16::from_be_bytes([frame[3], frame[4]]));
            return Some((width, height));
        }
        at += 2 + len;
    }
    None
}

fn webp_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.get(0..4)? != b"RIFF" || bytes.get(8..12)? != b"WEBP" {
        return None;
    }
    let kind = bytes.get(12..16)?;
    let data = bytes.get(20..)?;
    match kind {
        b"VP8X" => {
            let w = u32::from_le_bytes([*data.get(4)?, data[5], data[6], 0]) + 1;
            let h = u32::from_le_bytes([*data.get(7)?, data[8], data[9], 0]) + 1;
            Some((w, h))
        }
        b"VP8 " => {
            if data.get(3..6)? != [0x9d, 0x01, 0x2a] {
                return None;
            }
            let w = u32::from(u16::from_le_bytes([*data.get(6)?, data[7]]) & 0x3fff);
            let h = u32::from(u16::from_le_bytes([*data.get(8)?, data[9]]) & 0x3fff);
            Some((w, h))
        }
        b"VP8L" => {
            if *data.first()? != 0x2f {
                return None;
            }
            let bits = u32::from_le_bytes([*data.get(1)?, data[2], data[3], data[4]]);
            let w = (bits & 0x3fff) + 1;
            let h = ((bits >> 14) & 0x3fff) + 1;
            Some((w, h))
        }
        _ => None,
    }
}

/// Whether the journal keeps a file of this size and pixel count. An unknown
/// pixel count is left to the journal.
pub fn within_limits(bytes: u64, pixels: Option<u64>) -> bool {
    bytes <= MAX_BYTES && pixels.is_none_or(|count| count <= MAX_PIXELS)
}

// ── calendar ────────────────────────────────────────────────────────────────

/// Days since 1970-01-01 for a proleptic Gregorian date.
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date of a day number from [`days_from_civil`].
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        _ => 28,
    }
}

/// The instant of a local time, given the zone's offset at any instant.
pub fn local_to_instant(local_secs: i64, offset_at: &dyn Fn(i64) -> i32) -> i64 {
    let first = local_secs - i64::from(offset_at(local_secs));
    local_secs - i64::from(offset_at(first))
}

/// Where a span starts, as an instant: local midnight today, moved back by the
/// span in the calendar.
pub fn span_start(span: Span, now_secs: i64, offset_at: &dyn Fn(i64) -> i32) -> i64 {
    let local_now = now_secs + i64::from(offset_at(now_secs));
    let today = local_now.div_euclid(DAY_SECS);
    let (year, month, day) = civil_from_days(today);
    let start_day = match span {
        Span::Nothing => today,
        Span::ThirtyDays => today - 30,
        Span::SixMonths => {
            let months = year * 12 + i64::from(month) - 1 - 6;
            let (y, m) = (months.div_euclid(12), months.rem_euclid(12) as u32 + 1);
            days_from_civil(y, m, day.min(days_in_month(y, m)))
        }
        Span::Year => days_from_civil(year - 1, month, day.min(days_in_month(year - 1, month))),
        Span::All => days_from_civil(1900, 1, 1),
    };
    local_to_instant(start_day * DAY_SECS, offset_at)
}

// ── the stamp ───────────────────────────────────────────────────────────────

/// What the journal reads to place a screenshot at its capture second.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Stamp {
    pub captured_at: String,
    pub day: String,
    pub segment: String,
}

/// The stamp for a capture second in the zone's offset at that instant. An
/// offset that isn't a whole minute is sent as UTC.
pub fn stamp(captured_secs: i64, offset_secs: i32) -> Stamp {
    let offset = if offset_secs % 60 == 0 {
        i64::from(offset_secs)
    } else {
        0
    };
    let local = captured_secs + offset;
    let (year, month, day) = civil_from_days(local.div_euclid(DAY_SECS));
    let secs = local.rem_euclid(DAY_SECS);
    let (hour, minute, second) = (secs / 3600, secs % 3600 / 60, secs % 60);
    let zone = if offset == 0 {
        "Z".to_string()
    } else {
        let sign = if offset < 0 { '-' } else { '+' };
        let abs = offset.abs();
        format!("{sign}{:02}:{:02}", abs / 3600, abs % 3600 / 60)
    };
    Stamp {
        captured_at: format!(
            "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}{zone}"
        ),
        day: format!("{year:04}{month:02}{day:02}"),
        segment: format!("{hour:02}{minute:02}{second:02}_1"),
    }
}

/// The one file name the journal takes for this source.
pub fn upload_name(ext: &str) -> String {
    format!("screenshot_screen.{ext}")
}

// ── the journal's answer ────────────────────────────────────────────────────

/// What one answer means for the screenshot that was sent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Answer {
    /// In the journal, or already there. Never sent again.
    Delivered,
    /// The journal won't keep this one. Final.
    Refused,
    /// Not now: sent again on a later look.
    Held,
}

/// Route the journal's answer by its HTTP status, status word and reason code,
/// as the contract's answers table says.
pub fn route_answer(http_status: u16, status: Option<&str>, reason_code: Option<&str>) -> Answer {
    let ok = (200..300).contains(&http_status);
    if ok && matches!(status, Some("ok" | "duplicate" | "collision")) {
        return Answer::Delivered;
    }
    if matches!(
        reason_code,
        Some("screenshot_invalid" | "multipart_part_too_large")
    ) || http_status == 413
    {
        return Answer::Refused;
    }
    Answer::Held
}

// ── folders ─────────────────────────────────────────────────────────────────

fn parts(path: &str) -> Vec<&str> {
    path.split(['\\', '/']).filter(|p| !p.is_empty()).collect()
}

/// The folder as the tile says it: under the owner's home its path from there,
/// and the last three parts of that, joined ` › `.
pub fn folder_line(path: &str, home: Option<&str>) -> String {
    let all = parts(path);
    let relative = home
        .map(parts)
        .filter(|home| {
            !home.is_empty()
                && home.len() < all.len()
                && home
                    .iter()
                    .zip(&all)
                    .all(|(a, b)| a.eq_ignore_ascii_case(b))
        })
        .map_or(&all[..], |home| &all[home.len()..]);
    let from = relative.len().saturating_sub(3);
    relative[from..].join(" \u{203A} ")
}

/// The folder's own name, the last part of its path.
pub fn folder_name(path: &str) -> String {
    parts(path)
        .last()
        .map_or_else(String::new, |p| (*p).to_string())
}

/// Whether two paths name the same folder, as Windows compares them.
pub fn same_folder(a: &str, b: &str) -> bool {
    let norm = |p: &str| {
        parts(p)
            .iter()
            .map(|p| p.to_lowercase())
            .collect::<Vec<_>>()
    };
    norm(a) == norm(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mountain(_: i64) -> i32 {
        -7 * 3600
    }

    #[test]
    fn the_stamp_matches_the_golden_moments() {
        // 2026-11-03T14:32:07-07:00
        let at = days_from_civil(2026, 11, 3) * DAY_SECS + 21 * 3600 + 32 * 60 + 7;
        let s = stamp(at, -7 * 3600);
        assert_eq!(s.captured_at, "2026-11-03T14:32:07-07:00");
        assert_eq!(s.day, "20261103");
        assert_eq!(s.segment, "143207_1");
        let at = days_from_civil(2026, 11, 4) * DAY_SECS + 8 * 3600 + 15 * 60 + 2;
        let s = stamp(at, 3600);
        assert_eq!(s.captured_at, "2026-11-04T09:15:02+01:00");
        assert_eq!(
            (s.day.as_str(), s.segment.as_str()),
            ("20261104", "091502_1")
        );
        let at = days_from_civil(2026, 11, 5) * DAY_SECS;
        let s = stamp(at, 0);
        assert_eq!(s.captured_at, "2026-11-05T00:00:00Z");
        assert_eq!(s.segment, "000000_1");
    }

    #[test]
    fn the_stamp_uses_utc_for_an_offset_that_is_not_a_whole_minute() {
        let at = days_from_civil(2026, 1, 1) * DAY_SECS;
        assert_eq!(stamp(at, 30).captured_at, "2026-01-01T00:00:00Z");
        assert_eq!(stamp(at, 19_800).captured_at, "2026-01-01T05:30:00+05:30");
    }

    #[test]
    fn span_starts_match_the_calendar() {
        // 2026-11-13 10:00 local, Mountain.
        let now = days_from_civil(2026, 11, 13) * DAY_SECS + 17 * 3600;
        let local_midnight = |y, m, d| days_from_civil(y, m, d) * DAY_SECS + 7 * 3600;
        assert_eq!(
            span_start(Span::Nothing, now, &mountain),
            local_midnight(2026, 11, 13)
        );
        assert_eq!(
            span_start(Span::ThirtyDays, now, &mountain),
            local_midnight(2026, 10, 14)
        );
        assert_eq!(
            span_start(Span::SixMonths, now, &mountain),
            local_midnight(2026, 5, 13)
        );
        assert_eq!(
            span_start(Span::Year, now, &mountain),
            local_midnight(2025, 11, 13)
        );
        assert_eq!(
            span_start(Span::All, now, &mountain),
            local_midnight(1900, 1, 1)
        );
        // Six months back from the 31st lands on the last day of that month.
        let aug31 = days_from_civil(2026, 8, 31) * DAY_SECS + 17 * 3600;
        assert_eq!(
            span_start(Span::SixMonths, aug31, &mountain),
            local_midnight(2026, 2, 28)
        );
    }

    #[test]
    fn accepted_bytes_are_png_jpeg_and_webp_only() {
        for ext in ["png", "jpg", "jpeg", "webp"] {
            assert_eq!(image_kind(ext), Some(ImageKind::Accepted), "{ext}");
        }
        for ext in [
            "bmp", "gif", "tif", "tiff", "heic", "heif", "jxr", "wdp", "avif",
        ] {
            assert_eq!(image_kind(ext), Some(ImageKind::Convertible), "{ext}");
        }
        for ext in ["mp4", "mkv", "mov", "txt", "ini", ""] {
            assert_eq!(image_kind(ext), None, "{ext}");
        }
        assert_eq!(split_name("Shot.JPEG"), ("jpeg".to_string(), "Shot"));
        assert_eq!(upload_name("jpeg"), "screenshot_screen.jpeg");
    }

    #[test]
    fn windows_names_its_own_screenshots() {
        let t = snipping_tool_time("Screenshot 2026-11-13 014542").unwrap();
        assert_eq!(
            (t.year, t.month, t.day, t.hour, t.minute, t.second),
            (2026, 11, 13, 1, 45, 42)
        );
        assert!(snipping_tool_time("Screenshot 2026-11-13 014542 (2)").is_some());
        assert!(snipping_tool_time("Capture d'écran 2026-11-13 014542").is_some());
        assert!(
            snipping_tool_time("2026-11-13 014542").is_none(),
            "needs a word"
        );
        assert!(snipping_tool_time("Screenshot 2026-13-13 014542").is_none());
        assert!(snipping_tool_time("Screenshot 2026-02-30 014542").is_none());
        assert!(snipping_tool_time("Screenshot 2026-11-13 246000").is_none());
        assert!(snipping_tool_time("IMG_20261113_014542").is_none());
        assert!(is_legacy_screenshot_name("Screenshot (1)"));
        assert!(is_legacy_screenshot_name("Screenshot (128)"));
        assert!(!is_legacy_screenshot_name("Screenshot"));
        assert!(!is_legacy_screenshot_name("Screenshot (a)"));
        assert!(!is_legacy_screenshot_name("Holiday (1)"));
        assert!(has_name_stamp("Screenshot (3)"));
        assert!(!has_name_stamp("IMG_0042"));
    }

    fn png_with(chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut out = PNG_SIGNATURE.to_vec();
        for (kind, data) in chunks {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            out.extend_from_slice(*kind);
            out.extend_from_slice(data);
            out.extend_from_slice(&[0, 0, 0, 0]);
        }
        out
    }

    fn ihdr(w: u32, h: u32) -> Vec<u8> {
        let mut d = w.to_be_bytes().to_vec();
        d.extend_from_slice(&h.to_be_bytes());
        d.extend_from_slice(&[8, 6, 0, 0, 0]);
        d
    }

    #[test]
    fn game_bar_marks_its_captures() {
        let marked = png_with(&[
            (b"IHDR", &ihdr(4, 2)),
            (b"iTXt", b"Microsoft.GameDVR.Title\0\0\0\0\0Desktop"),
            (b"IEND", b""),
        ]);
        assert!(has_game_bar_marker(&marked));
        let plain = png_with(&[
            (b"IHDR", &ihdr(4, 2)),
            (b"tEXt", b"Microsoft.GameDVR.Title\0x"),
            (b"IEND", b""),
        ]);
        assert!(!has_game_bar_marker(&plain), "only iTXt");
        assert!(!has_game_bar_marker(b"not a png"));
        assert_eq!(header_pixels(&marked), Some(8));
    }

    #[test]
    fn pixel_counts_come_from_each_header() {
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0, 0];
        jpeg.extend_from_slice(&[
            0xFF, 0xC0, 0x00, 0x0B, 8, 0x00, 0x03, 0x00, 0x05, 1, 1, 0x11, 0,
        ]);
        assert_eq!(header_pixels(&jpeg), Some(15));
        let mut webp = b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0".to_vec();
        webp.extend_from_slice(&[0, 0, 0, 0, 9, 0, 0, 1, 0, 0]);
        assert_eq!(header_pixels(&webp), Some(20));
        assert_eq!(header_pixels(b"garbage"), None);
        assert!(within_limits(MAX_BYTES, Some(MAX_PIXELS)));
        assert!(!within_limits(MAX_BYTES + 1, None));
        assert!(!within_limits(10, Some(MAX_PIXELS + 1)));
        assert!(within_limits(10, None));
    }

    #[test]
    fn answers_follow_the_contract() {
        assert_eq!(route_answer(200, Some("ok"), None), Answer::Delivered);
        assert_eq!(
            route_answer(200, Some("duplicate"), None),
            Answer::Delivered
        );
        assert_eq!(
            route_answer(200, Some("collision"), None),
            Answer::Delivered
        );
        assert_eq!(
            route_answer(400, None, Some("screenshot_invalid")),
            Answer::Refused
        );
        assert_eq!(
            route_answer(400, None, Some("multipart_part_too_large")),
            Answer::Refused
        );
        assert_eq!(route_answer(413, None, None), Answer::Refused);
        assert_eq!(
            route_answer(409, None, Some("screenshot_ahead")),
            Answer::Held
        );
        assert_eq!(route_answer(503, Some("retryable"), None), Answer::Held);
        assert_eq!(route_answer(500, Some("failed"), None), Answer::Held);
        assert_eq!(route_answer(403, None, None), Answer::Held);
        assert_eq!(route_answer(426, None, None), Answer::Held);
        assert_eq!(
            route_answer(200, None, None),
            Answer::Held,
            "an undecodable 2xx waits"
        );
    }

    #[test]
    fn the_folder_line_keeps_the_last_three_parts() {
        let home = Some(r"C:\Users\ana");
        assert_eq!(
            folder_line(r"C:\Users\ana\Pictures\Screenshots", home),
            "Pictures \u{203A} Screenshots"
        );
        assert_eq!(
            folder_line(r"C:\Users\ana\OneDrive\Pictures\Screenshots", home),
            "OneDrive \u{203A} Pictures \u{203A} Screenshots"
        );
        assert_eq!(
            folder_line(r"C:\Users\ana\a\b\c\d", home),
            "b \u{203A} c \u{203A} d"
        );
        assert_eq!(folder_line(r"D:\Shots", home), "D: \u{203A} Shots");
        assert_eq!(
            folder_name(r"C:\Users\ana\Pictures\Screenshots\"),
            "Screenshots"
        );
        assert!(same_folder(
            r"C:\Users\Ana\Pictures\Screenshots\",
            "c:/users/ana/pictures/screenshots"
        ));
        assert!(!same_folder(
            r"C:\Users\ana\Pictures",
            r"C:\Users\ana\Pictures\Screenshots"
        ));
    }

    #[test]
    fn scratch_names_never_count() {
        for name in [
            "desktop.ini",
            "Desktop.INI",
            "~$shot.png",
            ".hidden.png",
            "a.png.crdownload",
            "a.tmp",
            "b.png.part",
        ] {
            assert!(is_scratch_name(name), "{name}");
        }
        assert!(!is_scratch_name("Screenshot (1).png"));
    }

    #[test]
    fn a_local_name_time_becomes_its_instant() {
        let stamp = snipping_tool_time("Screenshot 2026-11-13 014542").unwrap();
        let at = local_to_instant(stamp.as_local_secs(), &mountain);
        assert_eq!(
            at,
            days_from_civil(2026, 11, 13) * DAY_SECS + 8 * 3600 + 45 * 60 + 42
        );
    }
}
