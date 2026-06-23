// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Health serialization.
//!
//! The [`HealthDump`](observer_model::HealthDump) is rendered to JSON here, once,
//! and that same bytes-shape is what the CLI prints for `--dump-state`, what the
//! localhost `/healthz` endpoint returns, and what rides the `health://changed`
//! event. Keeping the encoding in one pure crate means the three transports can
//! never disagree about what "observing" looks like on the wire.

#![forbid(unsafe_code)]

use observer_model::HealthDump;

/// Render a [`HealthDump`] as the canonical `--dump-state` / `/healthz` JSON
/// (pretty, stable). Returns an error only on a serializer fault.
pub fn to_pretty_json(dump: &HealthDump) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(dump)
}

/// Render a [`HealthDump`] as compact JSON for event payloads.
pub fn to_compact_json(dump: &HealthDump) -> Result<String, serde_json::Error> {
    serde_json::to_string(dump)
}

#[cfg(test)]
mod tests {
    use super::*;
    use observer_model::AppPhase;

    fn sample() -> HealthDump {
        HealthDump {
            app_state: AppPhase::Idle,
            sources: vec![],
            frame_rate: None,
            segment_dir: None,
            segment_seconds_remaining: None,
            engine_ready: false,
            version: "0.1.0".into(),
            sync: observer_model::SyncSnapshot::default(),
            screen_encoder: None,
            exclusions: None,
            pause: None,
        }
    }

    #[test]
    fn round_trips_through_json() {
        let dump = sample();
        let json = to_pretty_json(&dump).unwrap();
        let back: HealthDump = serde_json::from_str(&json).unwrap();
        assert_eq!(dump, back);
    }

    #[test]
    fn app_state_serializes_as_snake_case_token() {
        let json = to_compact_json(&sample()).unwrap();
        assert!(json.contains("\"app_state\":\"idle\""));
    }
}
