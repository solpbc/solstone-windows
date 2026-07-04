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

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use observer_health::to_pretty_json;
use observer_model::{AppPhase, HealthDump};

/// Fixed loopback health port in the IANA dynamic/private range.
pub const HEALTH_PORT: u16 = 49247;

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
    }
}

/// Render the current snapshot as the canonical `--dump-state` / `/healthz` JSON.
pub fn dump_state_json() -> Result<String, serde_json::Error> {
    if let Some(body) = query_running_app() {
        Ok(body)
    } else {
        to_pretty_json(&not_running_snapshot())
    }
}

fn query_running_app() -> Option<String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], HEALTH_PORT));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(500)).ok()?;
    let timeout = Some(Duration::from_secs(2));
    stream.set_read_timeout(timeout).ok()?;
    stream.set_write_timeout(timeout).ok()?;
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .ok()?;

    let mut response = Vec::new();
    stream.read_to_end(&mut response).ok()?;
    let response = String::from_utf8(response).ok()?;
    let (_, body) = response.split_once("\r\n\r\n")?;
    (!body.is_empty()).then(|| body.to_string())
}
