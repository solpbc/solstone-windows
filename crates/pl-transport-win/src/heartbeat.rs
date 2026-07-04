// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The heartbeat loop — the macOS `HeartbeatService` analog.
//!
//! Every [`HEARTBEAT_INTERVAL_SECS`](crate::HEARTBEAT_INTERVAL_SECS) it POSTs an
//! `observe.status` event to the paired journal so the journal's `last_seen`
//! tracks the observer as live. The `paused` flag is read from the live health
//! dump (the engine's honest `app_state`), so the journal sees a real pause, not
//! an asserted one. The diagnostics beacon carries monotonic uptime in seconds
//! and last successful sync in epoch milliseconds. The most recent
//! success/failure is published into the shared sync snapshot (`heartbeat_ok`).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use observer_model::{AppPhase, HealthDump, SyncSnapshot, UploadStatus};
use observer_pl::wire::{HealthBeacon, HeartbeatEvent};

use crate::client::ObserverClient;
use crate::HEARTBEAT_INTERVAL_SECS;
use crate::{transport_error_code, TransportError};

#[derive(Debug, Clone, PartialEq, Eq)]
struct HeartbeatOutcome {
    ok: bool,
    warn_code: Option<String>,
}

fn classify_heartbeat(result: Result<(), TransportError>) -> HeartbeatOutcome {
    match result {
        Ok(()) => HeartbeatOutcome {
            ok: true,
            warn_code: None,
        },
        Err(error) => HeartbeatOutcome {
            ok: false,
            warn_code: Some(transport_error_code(&error)),
        },
    }
}

pub(crate) fn build_beacon(
    name: Option<String>,
    stream_type: &str,
    version: &str,
    uptime_secs: u64,
    upload: &UploadStatus,
) -> HealthBeacon {
    HealthBeacon {
        name,
        stream_type: Some(stream_type.to_string()),
        version: Some(version.to_string()),
        uptime: Some(uptime_secs),
        last_successful_sync: upload.last_successful_sync,
        pending_queue_depth: Some(upload.pending_segments),
        recent_error_count: Some(upload.recent_error_count),
        last_error_reason: upload.last_error_reason.clone(),
    }
}

async fn post_heartbeat_once(
    client: &ObserverClient,
    health: &Arc<Mutex<HealthDump>>,
    sync: &Arc<Mutex<SyncSnapshot>>,
    stream_type: &str,
    version: &str,
    started: Instant,
) {
    let paused = health
        .lock()
        .map(|h| h.app_state == AppPhase::Paused)
        .unwrap_or(false);
    let (name, upload) = sync
        .lock()
        .map(|snapshot| {
            (
                snapshot.pairing.observer_name.clone(),
                snapshot.upload.clone(),
            )
        })
        .unwrap_or_else(|_| (None, UploadStatus::default()));
    let beacon = build_beacon(
        name,
        stream_type,
        version,
        started.elapsed().as_secs(),
        &upload,
    );
    let event = HeartbeatEvent::observe_status(paused, beacon);
    let outcome = classify_heartbeat(client.heartbeat(&event).await);
    if let Some(code) = &outcome.warn_code {
        tracing::warn!(
            target: "pl_heartbeat",
            reason = code.as_str(),
            "heartbeat post failed"
        );
    }
    if let Ok(mut snapshot) = sync.lock() {
        snapshot.upload.heartbeat_ok = outcome.ok;
    }
}

/// Run the heartbeat until `shutdown` fires.
pub async fn run_heartbeat(
    client: Arc<ObserverClient>,
    health: Arc<Mutex<HealthDump>>,
    sync: Arc<Mutex<SyncSnapshot>>,
    stream_type: String,
    version: String,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
) {
    let started = Instant::now();
    post_heartbeat_once(&client, &health, &sync, &stream_type, &version, started).await;

    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = tokio::time::sleep(Duration::from_secs(HEARTBEAT_INTERVAL_SECS)) => {
                post_heartbeat_once(&client, &health, &sync, &stream_type, &version, started).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_beacon_maps_upload_status_without_verbose_error() {
        let mut upload = UploadStatus {
            pending_segments: 3,
            last_successful_sync: Some(1_700_000_000_000),
            last_error: Some("SECRET https://x/y?token=abc C:\\Users\\me\\seg.mp4".into()),
            ..UploadStatus::default()
        };
        upload.record_failure("http_503");

        let beacon = build_beacon(Some("fedora".into()), "desktop", "0.3.1", 120, &upload);

        assert_eq!(beacon.name.as_deref(), Some("fedora"));
        assert_eq!(beacon.stream_type.as_deref(), Some("desktop"));
        assert_eq!(beacon.version.as_deref(), Some("0.3.1"));
        assert_eq!(beacon.uptime, Some(120));
        assert_eq!(beacon.last_successful_sync, Some(1_700_000_000_000));
        assert_eq!(beacon.pending_queue_depth, Some(3));
        assert_eq!(beacon.recent_error_count, Some(1));
        assert_eq!(beacon.last_error_reason.as_deref(), Some("http_503"));

        let event = HeartbeatEvent::observe_status(false, beacon);
        let json = serde_json::to_string(&event).unwrap();
        assert!(!json.contains("last_error\":"));
        assert!(!json.contains("SECRET"));
        assert!(!json.contains("token"));
        assert!(!json.contains("Users"));
        assert!(!json.contains("https://"));
    }

    #[test]
    fn classify_heartbeat_redacts_and_flags_failure() {
        let outcome = classify_heartbeat(Err(TransportError::Rejected {
            status: 503,
            body: "SECRET https://10.0.0.5/y?token=abc C:\\Users\\me\\seg.mp4 sha256:abc".into(),
        }));

        assert!(!outcome.ok);
        assert_eq!(outcome.warn_code.as_deref(), Some("http_503"));
        let code = outcome.warn_code.unwrap();
        assert!(!code.contains("SECRET"));
        assert!(!code.contains("token"));
        assert!(!code.contains("Users"));
        assert!(!code.contains("https://"));
        assert!(!code.contains("sha256"));
        assert!(!code.contains("10.0.0.5"));

        let ok = classify_heartbeat(Ok(()));
        assert!(ok.ok);
        assert_eq!(ok.warn_code, None);
    }
}
