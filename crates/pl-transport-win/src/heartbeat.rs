// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The heartbeat loop — the macOS `HeartbeatService` analog.
//!
//! Every [`HEARTBEAT_INTERVAL_SECS`](crate::HEARTBEAT_INTERVAL_SECS) it POSTs an
//! `observe.status` event to the paired journal so the journal's `last_seen`
//! tracks the observer as live. The `paused` flag is read from the live health
//! dump (the engine's honest `app_state`), so the journal sees a real pause, not
//! an asserted one. The most recent success/failure is published into the shared
//! sync snapshot (`heartbeat_ok`).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use observer_model::{AppPhase, HealthDump, SyncSnapshot};

use crate::client::ObserverClient;
use crate::HEARTBEAT_INTERVAL_SECS;

/// Run the heartbeat until `shutdown` fires.
pub async fn run_heartbeat(
    client: Arc<ObserverClient>,
    health: Arc<Mutex<HealthDump>>,
    sync: Arc<Mutex<SyncSnapshot>>,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
) {
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = tokio::time::sleep(Duration::from_secs(HEARTBEAT_INTERVAL_SECS)) => {
                let paused = health
                    .lock()
                    .map(|h| h.app_state == AppPhase::Paused)
                    .unwrap_or(false);
                let ok = client.heartbeat(paused).await.is_ok();
                if let Ok(mut snapshot) = sync.lock() {
                    snapshot.upload.heartbeat_ok = ok;
                }
            }
        }
    }
}
