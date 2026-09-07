// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Device metadata validation, sanitization, and request models.

use serde::{Deserialize, Deserializer, Serialize};

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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ReportedMetadata {
    pub name: Option<String>,
    pub platform: Option<String>,
    pub device_type: Option<String>,
    pub app_id: Option<String>,
    pub app_version: Option<String>,
}

fn required_nullable<T: serde::de::DeserializeOwned>(
    object: &mut serde_json::Map<String, serde_json::Value>,
    field: &'static str,
) -> Result<Option<T>, String> {
    let value = object
        .remove(field)
        .ok_or_else(|| format!("missing {field}"))?;
    serde_json::from_value(value).map_err(|_| format!("invalid {field}"))
}

fn valid_value(value: &Option<String>, max: usize) -> bool {
    value
        .as_deref()
        .map(|value| sanitize_field(Some(value), max).as_deref() == Some(value))
        .unwrap_or(true)
}

impl<'de> Deserialize<'de> for ReportedMetadata {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let mut object = value
            .as_object()
            .cloned()
            .ok_or_else(|| serde::de::Error::custom("reported must be an object"))?;
        let reported = Self {
            name: required_nullable(&mut object, "name").map_err(serde::de::Error::custom)?,
            platform: required_nullable(&mut object, "platform")
                .map_err(serde::de::Error::custom)?,
            device_type: required_nullable(&mut object, "device_type")
                .map_err(serde::de::Error::custom)?,
            app_id: required_nullable(&mut object, "app_id").map_err(serde::de::Error::custom)?,
            app_version: required_nullable(&mut object, "app_version")
                .map_err(serde::de::Error::custom)?,
        };
        if !object.is_empty()
            || !valid_value(&reported.name, MAX_NAME_BYTES)
            || !valid_value(&reported.platform, MAX_FIELD_BYTES)
            || !valid_value(&reported.device_type, MAX_FIELD_BYTES)
            || !valid_value(&reported.app_id, MAX_FIELD_BYTES)
            || !valid_value(&reported.app_version, MAX_FIELD_BYTES)
        {
            return Err(serde::de::Error::custom("invalid reported metadata"));
        }
        Ok(reported)
    }
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct JournalInfo {
    pub name: Option<String>,
    pub version: Option<String>,
}

fn valid_journal_value(value: &Option<String>, max: usize) -> bool {
    value
        .as_deref()
        .map(|value| {
            !value.trim().is_empty()
                && value.len() <= max
                && !value.chars().any(|character| {
                    character.is_control() || character == '\u{2028}' || character == '\u{2029}'
                })
        })
        .unwrap_or(true)
}

impl<'de> Deserialize<'de> for JournalInfo {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let mut object = value
            .as_object()
            .cloned()
            .ok_or_else(|| serde::de::Error::custom("journal must be an object"))?;
        let journal = Self {
            name: required_nullable(&mut object, "name").map_err(serde::de::Error::custom)?,
            version: required_nullable(&mut object, "version").map_err(serde::de::Error::custom)?,
        };
        if !object.is_empty()
            || !valid_journal_value(&journal.name, MAX_NAME_BYTES)
            || !valid_journal_value(&journal.version, 128)
        {
            return Err(serde::de::Error::custom("invalid journal"));
        }
        Ok(journal)
    }
}

/// Response from `GET /app/network/api/clients/self`.
#[derive(Debug, Clone, Serialize)]
pub struct MetadataGetResponse {
    pub protocol_version: u32,
    pub revision: u64,
    pub reported: Option<ReportedMetadata>,
    pub owner_label: Option<String>,
    pub display_label: Option<String>,
    pub updated_at: Option<String>,
    pub journal: Option<JournalInfo>,
}

impl<'de> Deserialize<'de> for MetadataGetResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let mut object = value
            .as_object()
            .cloned()
            .ok_or_else(|| serde::de::Error::custom("metadata response must be an object"))?;
        let response = Self {
            protocol_version: object
                .remove("protocol_version")
                .ok_or_else(|| serde::de::Error::custom("missing protocol_version"))
                .and_then(|value| {
                    serde_json::from_value(value).map_err(serde::de::Error::custom)
                })?,
            revision: object
                .remove("revision")
                .ok_or_else(|| serde::de::Error::custom("missing revision"))
                .and_then(|value| {
                    serde_json::from_value(value).map_err(serde::de::Error::custom)
                })?,
            reported: required_nullable(&mut object, "reported")
                .map_err(serde::de::Error::custom)?,
            owner_label: required_nullable(&mut object, "owner_label")
                .map_err(serde::de::Error::custom)?,
            display_label: required_nullable(&mut object, "display_label")
                .map_err(serde::de::Error::custom)?,
            updated_at: required_nullable(&mut object, "updated_at")
                .map_err(serde::de::Error::custom)?,
            journal: required_nullable(&mut object, "journal").map_err(serde::de::Error::custom)?,
        };
        if !object.is_empty() {
            return Err(serde::de::Error::custom("unknown metadata response field"));
        }
        Ok(response)
    }
}

/// Successful `PUT /clients/self` response. The journal returns the same
/// complete protocol-1 resource as `GET`; accepting only a revision would
/// silently trust a partial response.
#[derive(Debug, Clone)]
pub struct MetadataPutResponse(MetadataGetResponse);

impl MetadataPutResponse {
    pub fn resource(&self) -> &MetadataGetResponse {
        &self.0
    }
}

impl<'de> Deserialize<'de> for MetadataPutResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let resource = MetadataGetResponse::deserialize(deserializer)?;
        if resource.protocol_version != 1 {
            return Err(serde::de::Error::custom("unsupported metadata protocol"));
        }
        Ok(Self(resource))
    }
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

    #[test]
    fn metadata_resource_rejects_omitted_nullable_fields_and_accepts_full_null_snapshot() {
        let omitted = r#"{
            "protocol_version": 1,
            "revision": 0,
            "reported": null,
            "display_label": null,
            "updated_at": null,
            "journal": null
        }"#;
        assert!(serde_json::from_str::<MetadataGetResponse>(omitted).is_err());

        let full_null = r#"{
            "protocol_version": 1,
            "revision": 0,
            "reported": null,
            "owner_label": null,
            "display_label": null,
            "updated_at": null,
            "journal": null
        }"#;
        let parsed: MetadataGetResponse = serde_json::from_str(full_null).unwrap();
        assert_eq!(parsed.reported, None);
        assert_eq!(parsed.journal, None);
        assert!(serde_json::from_str::<MetadataPutResponse>(full_null).is_ok());
    }
}
