// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Journal export save host contract (version 2 of `window.solstoneJournalHost`).
//!
//! The journal's export page requests a native save destination and streams
//! progress, pause/resume/cancel, reveal, and device-space checks through the
//! same reserved host channel as import handoff and OAuth brokerage.

use serde_json::{json, Map, Value};

use crate::HostContractError;

pub const EXPORT_CONTRACT_SHA256: &str =
    "7c4f637d55dc879ba980cde392f4ab5f4e4fe1ab14960f6cae20f0f91b4809fc";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportContract {
    pub version: u64,
    pub capability: String,
    pub start: String,
    pub status: String,
    pub pause: String,
    pub resume: String,
    pub cancel: String,
    pub reveal: String,
    pub device_space: String,
    pub event: String,
    pub unsupported: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportStartRequest {
    pub id: String,
    pub export_id: String,
    pub download_path: String,
    pub filename: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub etag: String,
    pub mode: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportMessage {
    Start(ExportStartRequest),
    Status {
        id: String,
        export_id: Option<String>,
    },
    Pause {
        id: String,
        export_id: Option<String>,
    },
    Resume {
        id: String,
        export_id: Option<String>,
    },
    Cancel {
        id: String,
        export_id: Option<String>,
    },
    Reveal {
        id: String,
        export_id: Option<String>,
    },
    DeviceSpace {
        id: String,
    },
}

impl ExportMessage {
    pub fn id(&self) -> &str {
        match self {
            Self::Start(req) => &req.id,
            Self::Status { id, .. }
            | Self::Pause { id, .. }
            | Self::Resume { id, .. }
            | Self::Cancel { id, .. }
            | Self::Reveal { id, .. }
            | Self::DeviceSpace { id } => id,
        }
    }
}

pub fn bundled_export_contract() -> &'static [u8] {
    include_bytes!("../../../contracts/journal-web-host/export-host-contract.json")
}

pub fn export_contract(bytes: &[u8]) -> Result<ExportContract, HostContractError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| HostContractError::Malformed)?;
    let text = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .ok_or(HostContractError::Malformed)
    };
    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .ok_or(HostContractError::Malformed)?;
    if version != 2 {
        return Err(HostContractError::Unsupported);
    }
    Ok(ExportContract {
        version,
        capability: text("capability")?,
        start: text("start")?,
        status: text("status")?,
        pause: text("pause")?,
        resume: text("resume")?,
        cancel: text("cancel")?,
        reveal: text("reveal")?,
        device_space: text("deviceSpace")?,
        event: text("event")?,
        unsupported: text("unsupported")?,
    })
}

pub fn valid_export_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub fn valid_sha256_hex(hex: &str) -> bool {
    hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub fn parse_message(
    contract: &ExportContract,
    kind: &str,
    id: String,
    message: &Map<String, Value>,
) -> Option<Result<ExportMessage, String>> {
    let optional_export_id = || -> Result<Option<String>, ()> {
        match message.get("export_id") {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) if valid_export_id(s) => Ok(Some(s.clone())),
            _ => Err(()),
        }
    };

    if kind == contract.start {
        let Some(export_id) = message
            .get("export_id")
            .and_then(Value::as_str)
            .filter(|s| valid_export_id(s))
            .map(str::to_owned)
        else {
            return Some(Err(id));
        };
        let expected_path = format!("/app/export/api/ready/{export_id}/download");
        let Some(download_path) = message
            .get("download_path")
            .and_then(Value::as_str)
            .filter(|p| *p == expected_path)
            .map(str::to_owned)
        else {
            return Some(Err(id));
        };
        let Some(sha256) = message
            .get("sha256")
            .and_then(Value::as_str)
            .filter(|h| valid_sha256_hex(h))
            .map(|h| h.to_ascii_lowercase())
        else {
            return Some(Err(id));
        };
        let Some(size_bytes) = message
            .get("size_bytes")
            .and_then(Value::as_u64)
            .filter(|n| *n > 0)
        else {
            return Some(Err(id));
        };
        let Some(etag) = message
            .get("etag")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|e| !e.is_empty() && e.len() <= 256)
            .map(str::to_owned)
        else {
            return Some(Err(id));
        };
        let filename = message
            .get("filename")
            .and_then(Value::as_str)
            .map(sanitize_suggested_filename)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "solstone-journal.zip".to_owned());
        let mode = message
            .get("mode")
            .and_then(Value::as_str)
            .filter(|m| matches!(*m, "full" | "without_processed_media"))
            .unwrap_or("full")
            .to_owned();
        return Some(Ok(ExportMessage::Start(ExportStartRequest {
            id,
            export_id,
            download_path,
            filename,
            size_bytes,
            sha256,
            etag,
            mode,
        })));
    }
    if kind == contract.status {
        return Some(match optional_export_id() {
            Ok(export_id) => Ok(ExportMessage::Status { id, export_id }),
            Err(()) => Err(id),
        });
    }
    if kind == contract.pause {
        return Some(match optional_export_id() {
            Ok(export_id) => Ok(ExportMessage::Pause { id, export_id }),
            Err(()) => Err(id),
        });
    }
    if kind == contract.resume {
        return Some(match optional_export_id() {
            Ok(export_id) => Ok(ExportMessage::Resume { id, export_id }),
            Err(()) => Err(id),
        });
    }
    if kind == contract.cancel {
        return Some(match optional_export_id() {
            Ok(export_id) => Ok(ExportMessage::Cancel { id, export_id }),
            Err(()) => Err(id),
        });
    }
    if kind == contract.reveal {
        return Some(match optional_export_id() {
            Ok(export_id) => Ok(ExportMessage::Reveal { id, export_id }),
            Err(()) => Err(id),
        });
    }
    if kind == contract.device_space {
        return Some(Ok(ExportMessage::DeviceSpace { id }));
    }
    None
}

pub fn sanitize_suggested_filename(raw: &str) -> String {
    let trimmed = raw.trim();
    let leaf = trimmed
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim_matches(['.', ' ']);
    let mut cleaned = String::with_capacity(leaf.len());
    for ch in leaf.chars() {
        if ch.is_control() || matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
            cleaned.push('_');
        } else {
            cleaned.push(ch);
        }
    }
    if cleaned.len() > 200 {
        cleaned.truncate(200);
    }
    cleaned
}

pub fn idle_reply(id: &str) -> Value {
    json!({
        "id": id,
        "export_id": Value::Null,
        "phase": "idle",
        "bytes_written": 0,
        "total_bytes": 0,
        "destination_display": Value::Null,
        "pause_reason": Value::Null,
        "error_code": Value::Null,
        "can_reveal": false,
        "verified": false,
        "platform": "windows",
    })
}

pub fn failed_reply(id: &str, export_id: Option<&str>, error_code: &str) -> Value {
    json!({
        "id": id,
        "export_id": export_id,
        "phase": "error",
        "bytes_written": 0,
        "total_bytes": 0,
        "destination_display": Value::Null,
        "pause_reason": Value::Null,
        "error_code": error_code,
        "can_reveal": false,
        "verified": false,
        "platform": "windows",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn bundled_export_contract_matches_pinned_sha256() {
        let bytes = bundled_export_contract();
        let digest = Sha256::digest(bytes);
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, EXPORT_CONTRACT_SHA256);

        let parsed = export_contract(bytes).expect("valid export contract");
        assert_eq!(parsed.version, 2);
        assert_eq!(parsed.capability, "solstoneJournalHost");
        assert_eq!(parsed.start, "export.start");
        assert_eq!(parsed.device_space, "export.device_space");
        assert_eq!(parsed.event, "solstone-journal-export-status");
    }

    #[test]
    fn parses_valid_export_messages_and_rejects_path_mismatch() {
        let contract = export_contract(bundled_export_contract()).unwrap();
        let sha = "a".repeat(64);
        let valid = json!({
            "type": "export.start",
            "id": "e-1",
            "export_id": "exp_123",
            "download_path": "/app/export/api/ready/exp_123/download",
            "filename": "../evil/solstone-journal-2026-11-26.zip",
            "size_bytes": 1024,
            "sha256": sha,
            "etag": "\"sha256-test\"",
            "mode": "complete"
        });
        let parsed = parse_message(
            &contract,
            "export.start",
            "e-1".into(),
            valid.as_object().unwrap(),
        );
        let Some(Ok(ExportMessage::Start(req))) = parsed else {
            panic!("expected valid start message, got {parsed:?}");
        };
        assert_eq!(req.filename, "solstone-journal-2026-11-26.zip");

        let mismatched = json!({
            "type": "export.start",
            "id": "e-2",
            "export_id": "exp_123",
            "download_path": "/app/export/api/ready/other/download",
            "size_bytes": 1024,
            "sha256": sha,
            "etag": "\"sha256-test\""
        });
        assert_eq!(
            parse_message(
                &contract,
                "export.start",
                "e-2".into(),
                mismatched.as_object().unwrap()
            ),
            Some(Err("e-2".to_owned()))
        );
    }
}
