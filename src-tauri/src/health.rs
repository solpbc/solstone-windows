// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Health snapshot + the `--dump-state` / `/healthz` JSON.
//!
//! All three honest-state transports render the same
//! [`HealthDump`](observer_model::HealthDump) through `observer-health`, so they
//! can never disagree. `--dump-state` runs headless (no GUI runtime), which is
//! why this lives outside the Tauri app graph.

use observer_health::to_pretty_json;
use observer_model::{AppPhase, HealthDump};

/// The current honest snapshot. Skeleton: reports `Idle` with no sources until
/// the engine is wired in; the shape is the real, committed one.
pub fn current_dump() -> Result<HealthDump, serde_json::Error> {
    Ok(HealthDump {
        app_state: AppPhase::Idle,
        sources: vec![],
        frame_rate: None,
        segment_dir: None,
        segment_seconds_remaining: None,
        engine_ready: false,
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

/// Render the current snapshot as the canonical `--dump-state` / `/healthz` JSON.
pub fn dump_state_json() -> Result<String, serde_json::Error> {
    let dump = current_dump()?;
    to_pretty_json(&dump)
}
