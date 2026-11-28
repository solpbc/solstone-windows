// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Pure parsing for Windows NTFS `Zone.Identifier` alternate data streams, plus
//! extension, MIME, timestamp, and origin-domain helpers.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Parsed fields from a Windows `<file>:Zone.Identifier` NTFS stream.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ZoneIdentifier {
    pub zone_id: Option<u64>,
    pub origin_url: Option<String>,
    pub referrer_url: Option<String>,
}

impl ZoneIdentifier {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.zone_id.is_none() && self.origin_url.is_none() && self.referrer_url.is_none()
    }
}

/// Parse raw bytes from `<path>:Zone.Identifier`.
///
/// Windows browsers and SmartScreen write an INI-shaped `[ZoneTransfer]` section
/// encoded as UTF-8 (with or without BOM) or UTF-16LE/BE, with CRLF or LF line
/// endings. Placeholder URLs such as `about:internet` or empty/control strings
/// are omitted while keeping any valid `ZoneId`.
#[must_use]
pub fn parse_zone_identifier(bytes: &[u8]) -> ZoneIdentifier {
    let Some(text) = decode_text(bytes) else {
        return ZoneIdentifier::default();
    };
    let mut out = ZoneIdentifier::default();
    let mut in_section = true;

    for raw_line in text.lines() {
        let line = raw_line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let section = line[1..line.len() - 1].trim();
            in_section = section.eq_ignore_ascii_case("ZoneTransfer");
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let val = val.trim();
        if key.eq_ignore_ascii_case("ZoneId") {
            if let Ok(id) = val.parse::<u64>() {
                out.zone_id = Some(id);
            }
        } else if key.eq_ignore_ascii_case("HostUrl") {
            out.origin_url = clean_url(val);
        } else if key.eq_ignore_ascii_case("ReferrerUrl") {
            out.referrer_url = clean_url(val);
        }
    }
    out
}

fn decode_text(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        if rest.len() % 2 != 0 {
            return None;
        }
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16(&units).ok();
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        if rest.len() % 2 != 0 {
            return None;
        }
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16(&units).ok();
    }
    let rest = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    Some(String::from_utf8_lossy(rest).into_owned())
}

fn clean_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_control) {
        return None;
    }
    if trimmed.eq_ignore_ascii_case("about:internet") || trimmed.eq_ignore_ascii_case("about:blank")
    {
        return None;
    }
    if trimmed.len() <= 2048 {
        return Some(trimmed.to_owned());
    }
    let mut end = 2048;
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    Some(trimmed[..end].to_owned())
}

/// Extract a clean host domain (dropping a leading `www.`) from an origin URL.
#[must_use]
pub fn origin_domain(origin_url: &str) -> Option<String> {
    let trimmed = origin_url.trim();
    let (scheme, rest) = trimmed.split_once("://")?;
    if scheme.is_empty()
        || !scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
    {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host_raw = if host_port.starts_with('[') {
        let end = host_port.find(']')?;
        &host_port[..=end]
    } else {
        host_port.split(':').next()?
    };
    let host = host_raw.trim().to_ascii_lowercase();
    if host.is_empty() || host.bytes().any(|b| b.is_ascii_whitespace()) {
        return None;
    }
    let stripped = host.strip_prefix("www.").unwrap_or(&host);
    (!stripped.is_empty()).then(|| stripped.to_owned())
}

/// Lowercase file extension without the leading dot.
#[must_use]
pub fn extension_lower(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|ext| !ext.is_empty())
}

/// Best-effort MIME type from file extension for deterministic activity lines.
#[must_use]
pub fn guess_mime(extension: Option<&str>) -> Option<String> {
    let mime = match extension? {
        "md" | "markdown" => "text/markdown",
        "txt" | "log" => "text/plain",
        "pdf" => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "doc" => "application/msword",
        "odt" => "application/vnd.oasis.opendocument.text",
        "rtf" => "application/rtf",
        "epub" => "application/epub+zip",
        "html" | "htm" => "text/html",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "csv" => "text/csv",
        "tsv" => "text/tab-separated-values",
        "vtt" => "text/vtt",
        "srt" => "application/x-subrip",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "avif" => "image/avif",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "jxl" => "image/jxl",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        "ogg" => "audio/ogg",
        "mp4" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "tar" => "application/x-tar",
        "iso" => "application/x-iso9660-image",
        "msi" => "application/x-msi",
        "exe" => "application/vnd.microsoft.portable-executable",
        "json" => "application/json",
        _ => return None,
    };
    Some(mime.to_owned())
}

#[must_use]
pub fn system_time_ms(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
        Err(err) => -i64::try_from(err.duration().as_millis()).unwrap_or(i64::MAX),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_edge_and_chrome_zone_identifier() {
        let raw = b"[ZoneTransfer]\r\nZoneId=3\r\nReferrerUrl=https://www.example.org/releases\r\nHostUrl=https://cdn.example.org/files/report.pdf?dl=1\r\n";
        let parsed = parse_zone_identifier(raw);
        assert_eq!(parsed.zone_id, Some(3));
        assert_eq!(
            parsed.origin_url.as_deref(),
            Some("https://cdn.example.org/files/report.pdf?dl=1")
        );
        assert_eq!(
            parsed.referrer_url.as_deref(),
            Some("https://www.example.org/releases")
        );
        assert_eq!(
            origin_domain(parsed.origin_url.as_deref().unwrap()).as_deref(),
            Some("cdn.example.org")
        );
    }

    #[test]
    fn omits_about_internet_placeholder_but_keeps_zone_id() {
        let raw = b"[ZoneTransfer]\r\nZoneId=3\r\nHostUrl=about:internet\r\n";
        let parsed = parse_zone_identifier(raw);
        assert_eq!(parsed.zone_id, Some(3));
        assert_eq!(parsed.origin_url, None);
        assert_eq!(parsed.referrer_url, None);
    }

    #[test]
    fn decodes_utf16le_bom_and_ignores_unrelated_sections() {
        let text = "[OtherSection]\r\nHostUrl=https://ignored.example/\r\n[ZoneTransfer]\r\nZoneId=3\r\nHostUrl=https://docs.example.com/a.pdf\r\n";
        let mut bytes = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let parsed = parse_zone_identifier(&bytes);
        assert_eq!(parsed.zone_id, Some(3));
        assert_eq!(
            parsed.origin_url.as_deref(),
            Some("https://docs.example.com/a.pdf")
        );
    }

    #[test]
    fn origin_domain_strips_www_and_rejects_malformed_urls() {
        assert_eq!(
            origin_domain("https://www.mozilla.org/en-US/firefox/download/"),
            Some("mozilla.org".into())
        );
        assert_eq!(
            origin_domain("https://app.docusign.net/documents/123?token=abc"),
            Some("app.docusign.net".into())
        );
        assert_eq!(origin_domain("not a url"), None);
    }
}
