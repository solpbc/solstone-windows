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

/// Diagnostics-only health fields carried by `observe.status`. All fields are
/// optional and omitted when absent so the legacy heartbeat body stays unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HealthBeacon {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub stream_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub version: Option<String>,
    /// Monotonic process uptime in seconds.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub uptime: Option<u64>,
    /// Epoch milliseconds of the last successful sync tick.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub last_successful_sync: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub pending_queue_depth: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub recent_error_count: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub last_error_reason: Option<String>,
}

/// The heartbeat event body. The journal updates `last_seen` when it sees an
/// `observe.status` event; `paused` carries the observer's current pause state,
/// matching the macOS `HeartbeatService` POST.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HeartbeatEvent {
    pub tract: String,
    pub event: String,
    pub paused: bool,
    #[serde(flatten)]
    pub beacon: HealthBeacon,
}

impl HeartbeatEvent {
    /// Build the canonical `observe.status` heartbeat.
    pub fn status(paused: bool) -> Self {
        Self::observe_status(paused, HealthBeacon::default())
    }

    /// Build an `observe.status` heartbeat with diagnostics-only health fields.
    pub fn observe_status(paused: bool, beacon: HealthBeacon) -> Self {
        Self {
            tract: "observe".to_string(),
            event: "status".to_string(),
            paused,
            beacon,
        }
    }
}

// ── /app/observer/ingest/segments/<day> (reconcile) ──────────────────────────

/// One file recorded on the journal for a segment, used to reconcile by filename,
/// sha256, and held status. `current_path` is omitted because the client only
/// needs whether the file is held; segment `original_key` is omitted because the
/// ingest response tells the client which server key to reconcile and serde
/// ignores unknown fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerFile {
    pub name: String,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub submitted_name: Option<String>,
}

impl ServerFile {
    /// The client-submitted filename this entry corresponds to.
    fn submitted_or_name(&self) -> &str {
        self.submitted_name.as_deref().unwrap_or(self.name.as_str())
    }

    /// Physically held by the journal (present or relocated); `missing`/unknown proves nothing.
    fn is_held(&self) -> bool {
        matches!(self.status.as_deref(), Some("present") | Some("relocated"))
    }
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
    /// The journal proves it holds `(filename, sha)` under `segment_key`: some file
    /// entry whose submitted-or-name == filename AND sha256 == sha AND status ∈ {present, relocated}.
    pub fn proves_file_held(&self, segment_key: &str, filename: &str, sha: &str) -> bool {
        self.items.iter().any(|item| {
            item.key == segment_key
                && item.files.iter().any(|f| {
                    f.submitted_or_name() == filename
                        && f.sha256.as_deref() == Some(sha)
                        && f.is_held()
                })
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
        assert_eq!(
            json,
            r#"{"tract":"observe","event":"status","paused":true}"#
        );
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 3);
    }

    #[test]
    fn heartbeat_observe_status_serializes_populated_beacon() {
        let event = HeartbeatEvent::observe_status(
            false,
            HealthBeacon {
                name: Some("fedora".into()),
                stream_type: Some("desktop".into()),
                version: Some("0.3.1".into()),
                uptime: Some(120),
                last_successful_sync: Some(1_700_000_000_000),
                pending_queue_depth: Some(2),
                recent_error_count: Some(1),
                last_error_reason: Some("http_503".into()),
            },
        );

        let json = serde_json::to_string(&event).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(object.len(), 11);
        assert_eq!(object["tract"], "observe");
        assert_eq!(object["event"], "status");
        assert_eq!(object["paused"], false);
        assert_eq!(object["name"], "fedora");
        assert_eq!(object["stream_type"], "desktop");
        assert_eq!(object["version"], "0.3.1");
        assert_eq!(object["uptime"], 120);
        assert_eq!(object["last_successful_sync"], 1_700_000_000_000u64);
        assert_eq!(object["pending_queue_depth"], 2);
        assert_eq!(object["recent_error_count"], 1);
        assert_eq!(object["last_error_reason"], "http_503");

        let round_trip: HeartbeatEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(round_trip, event);
    }

    #[test]
    fn heartbeat_observe_status_omits_absent_name() {
        let event = HeartbeatEvent::observe_status(
            false,
            HealthBeacon {
                name: None,
                stream_type: Some("desktop".into()),
                version: Some("0.3.1".into()),
                uptime: Some(120),
                last_successful_sync: Some(1_700_000_000_000),
                pending_queue_depth: Some(2),
                recent_error_count: Some(1),
                last_error_reason: Some("http_503".into()),
            },
        );

        let json = serde_json::to_string(&event).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let object = value.as_object().unwrap();
        assert!(!object.contains_key("name"));
        assert_eq!(object["stream_type"], "desktop");
        assert_eq!(object["version"], "0.3.1");
    }

    #[test]
    fn segments_response_proves_file_held() {
        let raw = r#"{"items":[{"key":"143000_300","files":[{"name":"display_1_screen.mp4","submitted_name":"143000_300_display_1_screen.mp4","sha256":"abcd","size":10,"status":"present"},{"name":"missing.mp4","sha256":"abcd","size":10,"status":"missing"}]}],"total":1,"protocol_version":2}"#;
        let resp: SegmentsResponse = serde_json::from_str(raw).unwrap();
        assert!(resp.proves_file_held("143000_300", "143000_300_display_1_screen.mp4", "abcd"));
        assert!(!resp.proves_file_held("143000_300", "143000_300_display_1_screen.mp4", "ffff"));
        assert!(!resp.proves_file_held("000000_300", "143000_300_display_1_screen.mp4", "abcd"));
        assert!(!resp.proves_file_held("143000_300", "display_1_screen.mp4", "abcd"));
        assert!(!resp.proves_file_held("143000_300", "missing.mp4", "abcd"));
    }
}
