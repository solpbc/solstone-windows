// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Device metadata validation, sanitization, and request models.

use serde::{Deserialize, Serialize};

/// Maximum UTF-8 byte length for the name field.
pub const MAX_NAME_BYTES: usize = 80;

/// Maximum UTF-8 byte length for platform, device_type, app_id, app_version.
pub const MAX_FIELD_BYTES: usize = 64;

/// Raw, unsanitized device facts sampled from the environment and host.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawDeviceFacts {
    pub name: Option<String>,
    pub platform: Option<String>,
    pub device_type: Option<String>,
    pub app_id: Option<String>,
    pub app_version: Option<String>,
}

/// The sanitized 5-tuple of reported device metadata.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedMetadata {
    pub name: Option<String>,
    pub platform: Option<String>,
    pub device_type: Option<String>,
    pub app_id: Option<String>,
    pub app_version: Option<String>,
}

/// Sanitize a single string field according to length and character rules.
///
/// Rules:
/// - Trim leading/trailing whitespace.
/// - If empty after trim -> `None`.
/// - If any character is a control character -> `None`.
/// - If byte length > `max_bytes` -> `None`. Never slice/truncate mid-codepoint.
pub fn sanitize_field(val: Option<&str>, max_bytes: usize) -> Option<String> {
    let trimmed = val?.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.chars().any(|c| c.is_control()) {
        return None;
    }
    if trimmed.len() > max_bytes {
        return None;
    }
    Some(trimmed.to_string())
}

/// Sanitize raw device facts into the standard reported metadata structure.
pub fn sanitize_facts(raw: &RawDeviceFacts) -> ReportedMetadata {
    ReportedMetadata {
        name: sanitize_field(raw.name.as_deref(), MAX_NAME_BYTES),
        platform: sanitize_field(raw.platform.as_deref(), MAX_FIELD_BYTES),
        device_type: sanitize_field(raw.device_type.as_deref(), MAX_FIELD_BYTES),
        app_id: sanitize_field(raw.app_id.as_deref(), MAX_FIELD_BYTES),
        app_version: sanitize_field(raw.app_version.as_deref(), MAX_FIELD_BYTES),
    }
}

/// PUT request payload for `/app/network/api/clients/self`.
#[derive(Debug, Clone, Serialize)]
pub struct MetadataPutRequest<'a> {
    pub protocol_version: u32,
    pub expected_revision: u64,
    pub reported: &'a ReportedMetadata,
}

/// Journal details within the metadata GET response.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct JournalInfo {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

/// Response from `GET /app/network/api/clients/self`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MetadataGetResponse {
    pub protocol_version: u32,
    pub revision: u64,
    #[serde(default)]
    pub reported: Option<ReportedMetadata>,
    #[serde(default)]
    pub owner_label: Option<String>,
    #[serde(default)]
    pub display_label: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub journal: Option<JournalInfo>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_field_bounds_and_controls() {
        assert_eq!(sanitize_field(None, 64), None);
        assert_eq!(sanitize_field(Some("   "), 64), None);
        assert_eq!(sanitize_field(Some("hello\x00world"), 64), None);
        assert_eq!(sanitize_field(Some("hello\nworld"), 64), None);
        assert_eq!(sanitize_field(Some("hello\tworld"), 64), None);
        assert_eq!(
            sanitize_field(Some("  valid-name  "), 64),
            Some("valid-name".to_string())
        );

        // Oversized name > 80 bytes
        let long_name = "a".repeat(81);
        assert_eq!(sanitize_field(Some(&long_name), 80), None);
        let exact_80 = "a".repeat(80);
        assert_eq!(sanitize_field(Some(&exact_80), 80), Some(exact_80));

        // Multibyte 3-byte utf-8 characters: 27 chars * 3 = 81 bytes > 80 bytes -> None
        let multibyte = "日".repeat(27);
        assert_eq!(sanitize_field(Some(&multibyte), 80), None);

        // 26 chars * 3 = 78 bytes <= 80 bytes -> Valid
        let multibyte_ok = "日".repeat(26);
        assert_eq!(sanitize_field(Some(&multibyte_ok), 80), Some(multibyte_ok));
    }

    #[test]
    fn sanitize_facts_nulls_invalid_only() {
        let raw = RawDeviceFacts {
            name: Some("  My Machine  ".to_string()),
            platform: Some("windows\x07".to_string()), // control char
            device_type: None,
            app_id: Some("app.solstone.windows".to_string()),
            app_version: Some(" ".repeat(10)), // empty after trim
        };
        let sanitized = sanitize_facts(&raw);
        assert_eq!(sanitized.name.as_deref(), Some("My Machine"));
        assert_eq!(sanitized.platform, None);
        assert_eq!(sanitized.device_type, None);
        assert_eq!(sanitized.app_id.as_deref(), Some("app.solstone.windows"));
        assert_eq!(sanitized.app_version, None);
    }
}
