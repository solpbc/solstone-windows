// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Health snapshot + the `--dump-state` / `/healthz` JSON.
//!
//! All three honest-state transports render the same
//! [`HealthDump`](observer_model::HealthDump) through `observer-health`, so they
//! can never disagree. `--dump-state` runs headless (no GUI runtime), which is
//! why this lives outside the Tauri app graph.
//!
//! The running app serves `/healthz` on a fixed loopback-only port. Binding and
//! querying `127.0.0.1` only is part of the data covenant: health stays local to
//! the owner's machine.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use observer_contract::{HEALTH_PORT, LOOPBACK_HOST};
use observer_health::{decide_dump, fetch_health_response, to_pretty_json, DumpChoice};
use observer_model::{classify_presence, AppPhase, HealthDump, Presence};

pub(crate) enum ListenerSlot {
    Health,
    Control,
}

pub(crate) fn record_listener_fault(
    health: &Arc<Mutex<HealthDump>>,
    slot: ListenerSlot,
    message: String,
) {
    let Ok(mut dump) = health.lock() else {
        return;
    };
    match slot {
        ListenerSlot::Health => dump.listener_faults.health = Some(message),
        ListenerSlot::Control => dump.listener_faults.control = Some(message),
    }
}

/// Honest snapshot for a process that is not currently running.
pub fn not_running_snapshot() -> HealthDump {
    HealthDump {
        app_state: AppPhase::Idle,
        sources: vec![],
        frame_rate: None,
        segment_dir: None,
        segment_seconds_remaining: None,
        engine_ready: false,
        version: env!("CARGO_PKG_VERSION").to_string(),
        sync: observer_model::SyncSnapshot::default(),
        screen_encoder: None,
        exclusions: None,
        storage: None,
        pause: None,
        views: Default::default(),
        pump_degraded: false,
        listener_faults: observer_model::ListenerFaults::default(),
    }
}

#[derive(Debug)]
pub enum DumpStateError {
    Unavailable { presence: Presence },
    Serialize(serde_json::Error),
}

impl std::fmt::Display for DumpStateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable { presence } => {
                let label = match presence {
                    Presence::Present => "present",
                    Presence::Unknown => "unknown",
                    Presence::Absent => "absent",
                };
                write!(f, "health endpoint unavailable (presence: {label})")
            }
            Self::Serialize(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for DumpStateError {}

/// Render the current snapshot as the canonical `--dump-state` / `/healthz` JSON.
pub fn dump_state_json() -> Result<String, DumpStateError> {
    let (mutex, population) =
        platform_win::probe_app_presence("Solstone", "solstone-windows-app.exe");
    let presence = classify_presence(mutex, population);
    let host: std::net::Ipv4Addr = LOOPBACK_HOST
        .parse()
        .expect("loopback host is an IPv4 address");
    let query = fetch_health_response(
        SocketAddr::from((host, HEALTH_PORT)),
        Duration::from_millis(500),
        Duration::from_secs(2),
    );
    match decide_dump(query, presence) {
        DumpChoice::Live(dump) => to_pretty_json(&dump).map_err(DumpStateError::Serialize),
        DumpChoice::NotRunning => {
            to_pretty_json(&not_running_snapshot()).map_err(DumpStateError::Serialize)
        }
        DumpChoice::Failed => Err(DumpStateError::Unavailable { presence }),
    }
}
