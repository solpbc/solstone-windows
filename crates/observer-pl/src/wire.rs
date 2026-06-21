// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The observer wire types — request/response bodies, serde-shaped to match the
//! journal (`solstone` convey) exactly.
//!
//! Endpoint shapes verified against `apps/link/routes.py` (`/pair`) and
//! `apps/observer/routes.py` (`/register`, `/ingest`, `/ingest/event`,
//! `/ingest/segments/<day>`). Field names are the journal's JSON keys; anything
//! the client doesn't consume (e.g. `local_endpoints`, `home_attestation`) is
//! optional so a server adding fields never breaks the client.

use serde::{Deserialize, Serialize};

// ── /app/network/pair ────────────────────────────────────────────────────────

/// POST body for `/app/network/pair?token=<nonce>`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairRequest {
    pub csr: String,
    pub device_label: String,
}

/// Success response from `/app/network/pair`. The journal signs our CSR and returns
/// the client cert plus the CA chain to trust.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairResponse {
    pub client_cert: String,
    pub ca_chain: Vec<String>,
    pub instance_id: String,
    pub home_label: String,
    /// `"sha256:<hex>"` of the signed client cert DER — we verify it matches.
    pub fingerprint: String,
    #[serde(default)]
    pub home_attestation: Option<String>,
    /// The journal's own LAN endpoints; unused by the client (we already have
    /// the pair-link candidates) but captured so deserialization never fails.
    #[serde(default)]
    pub local_endpoints: Option<serde_json::Value>,
}

// ── /app/observer/register ───────────────────────────────────────────────────

/// POST body for `/app/observer/register`. `stream_type` is `"desktop"` for the
/// Windows observer; `platform` is `"windows"`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegisterRequest {
    pub platform: String,
    pub hostname: String,
    pub stream_type: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub label: Option<String>,
}

/// Response from `/app/observer/register`. `key` is the observer handle used in
/// the `X-Solstone-Observer` header on every subsequent request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegisterResponse {
    pub key: String,
    pub name: String,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub ingest_url: Option<String>,
    #[serde(default)]
    pub protocol_version: Option<u32>,
}

// ── /app/observer/ingest ─────────────────────────────────────────────────────

/// Response from `/app/observer/ingest`. `status` is `ok` / `duplicate` /
/// `collision`; on collision `segment` carries the adjusted key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IngestResponse {
    pub status: String,
    #[serde(default)]
    pub segment: Option<String>,
    #[serde(default)]
    pub existing_segment: Option<String>,
    #[serde(default)]
    pub files: Option<Vec<String>>,
    #[serde(default)]
    pub bytes: Option<u64>,
}

impl IngestResponse {
    /// The segment landed (newly written or already present): a confirmed
    /// upload either way.
    pub fn is_accepted(&self) -> bool {
        matches!(self.status.as_str(), "ok" | "duplicate" | "collision")
    }
}

// ── /app/observer/ingest/event (heartbeat) ───────────────────────────────────

/// The heartbeat event body. The journal updates `last_seen` when it sees an
/// `observe.status` event; `paused` carries the observer's current pause state,
/// matching the macOS `HeartbeatService` POST.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HeartbeatEvent {
    pub tract: String,
    pub event: String,
    pub paused: bool,
}

impl HeartbeatEvent {
    /// Build the canonical `observe.status` heartbeat.
    pub fn status(paused: bool) -> Self {
        Self {
            tract: "observe".to_string(),
            event: "status".to_string(),
            paused,
        }
    }
}

// ── /app/observer/ingest/segments/<day> (reconcile) ──────────────────────────

/// One file recorded on the journal for a segment, used to reconcile by sha256.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerFile {
    pub name: String,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
}

/// One segment the journal has on record for the day.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerSegment {
    pub key: String,
    #[serde(default)]
    pub files: Vec<ServerFile>,
}

/// The protocol-v2 reconcile envelope (`{items,total,protocol_version}`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SegmentsResponse {
    #[serde(default)]
    pub items: Vec<ServerSegment>,
    #[serde(default)]
    pub total: Option<u64>,
    #[serde(default)]
    pub protocol_version: Option<u32>,
}

impl SegmentsResponse {
    /// True if the journal records `sha` for any file under `segment`.
    pub fn has_segment_sha(&self, segment: &str, sha: &str) -> bool {
        self.items.iter().any(|item| {
            item.key == segment && item.files.iter().any(|f| f.sha256.as_deref() == Some(sha))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_request_serializes_stream_type_key() {
        let req = RegisterRequest {
            platform: "windows".into(),
            hostname: "winbox".into(),
            stream_type: "desktop".into(),
            version: "0.1.0".into(),
            label: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"stream_type\":\"desktop\""));
        assert!(json.contains("\"platform\":\"windows\""));
        // None label is omitted.
        assert!(!json.contains("label"));
    }

    #[test]
    fn register_response_parses_journal_shape() {
        let raw = r#"{"key":"abc123key","prefix":"abc123ke","name":"winbox","ingest_url":"/app/observer/ingest","protocol_version":2}"#;
        let resp: RegisterResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(resp.key, "abc123key");
        assert_eq!(resp.name, "winbox");
        assert_eq!(resp.protocol_version, Some(2));
    }

    #[test]
    fn ingest_response_accepts_ok_duplicate_collision() {
        for status in ["ok", "duplicate", "collision"] {
            let raw = format!("{{\"status\":\"{status}\",\"segment\":\"143000_300\"}}");
            let resp: IngestResponse = serde_json::from_str(&raw).unwrap();
            assert!(resp.is_accepted(), "status {status}");
        }
        let rejected: IngestResponse = serde_json::from_str(r#"{"status":"error"}"#).unwrap();
        assert!(!rejected.is_accepted());
    }

    #[test]
    fn pair_response_tolerates_extra_and_missing_fields() {
        let raw = r#"{"client_cert":"PEM","ca_chain":["CA"],"instance_id":"id","home_label":"Home","fingerprint":"sha256:deadbeef"}"#;
        let resp: PairResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(resp.fingerprint, "sha256:deadbeef");
        assert_eq!(resp.ca_chain, vec!["CA".to_string()]);
        assert!(resp.home_attestation.is_none());
    }

    #[test]
    fn heartbeat_status_is_observe_status() {
        let json = serde_json::to_string(&HeartbeatEvent::status(true)).unwrap();
        assert!(json.contains("\"tract\":\"observe\""));
        assert!(json.contains("\"event\":\"status\""));
        assert!(json.contains("\"paused\":true"));
    }

    #[test]
    fn segments_response_reconciles_by_sha() {
        let raw = r#"{"items":[{"key":"143000_300","files":[{"name":"display_1_screen.mp4","sha256":"abcd","size":10}]}],"total":1,"protocol_version":2}"#;
        let resp: SegmentsResponse = serde_json::from_str(raw).unwrap();
        assert!(resp.has_segment_sha("143000_300", "abcd"));
        assert!(!resp.has_segment_sha("143000_300", "ffff"));
        assert!(!resp.has_segment_sha("000000_300", "abcd"));
    }
}
