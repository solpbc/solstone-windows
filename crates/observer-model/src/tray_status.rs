// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::{AppPhase, PairingPhase, PauseSnapshot, SyncSnapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayVisual {
    Full,
    Half,
    Cloud,
    Error,
    Pending,
}

pub fn classify_tray(
    app: AppPhase,
    sync: &SyncSnapshot,
    pause: Option<&PauseSnapshot>,
) -> (TrayVisual, String) {
    match app {
        AppPhase::Idle => (TrayVisual::Pending, "solstone — idle".to_string()),
        AppPhase::Starting => (TrayVisual::Pending, "solstone — starting".to_string()),
        AppPhase::Paused => {
            let tooltip = match pause.and_then(|p| p.seconds_remaining) {
                Some(secs) => format!("solstone — paused, {} left", format_remaining(secs)),
                None => "solstone — paused".to_string(),
            };
            (TrayVisual::Cloud, tooltip)
        }
        AppPhase::Error => (TrayVisual::Error, "solstone — attention needed".to_string()),
        AppPhase::Observing => match (sync.pairing.phase, sync.upload.heartbeat_ok) {
            (PairingPhase::Paired, true) => (
                TrayVisual::Full,
                "solstone — observing, connected".to_string(),
            ),
            (PairingPhase::Paired, false) => (
                TrayVisual::Half,
                "solstone — observing, saved on this PC".to_string(),
            ),
            _ => (
                TrayVisual::Half,
                "solstone — observing, no journal connected".to_string(),
            ),
        },
    }
}

/// Human countdown for the tray tooltip: "14 min", "1 hr 2 min", "less than a
/// minute". Whole-minute granularity matches the tooltip's once-a-second refresh.
fn format_remaining(secs: u64) -> String {
    let mins = secs / 60;
    if mins == 0 {
        "less than a minute".to_string()
    } else if mins < 60 {
        format!("{mins} min")
    } else {
        let (h, m) = (mins / 60, mins % 60);
        if m == 0 {
            format!("{h} hr")
        } else {
            format!("{h} hr {m} min")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PairingState, PauseReason, UploadStatus};

    fn sync(phase: PairingPhase, heartbeat_ok: bool) -> SyncSnapshot {
        SyncSnapshot {
            pairing: PairingState {
                phase,
                journal_label: None,
                observer_name: None,
                detail: None,
            },
            upload: UploadStatus {
                heartbeat_ok,
                ..UploadStatus::default()
            },
        }
    }

    fn assert_tray(
        app: AppPhase,
        sync: &SyncSnapshot,
        pause: Option<&PauseSnapshot>,
        expected_visual: TrayVisual,
        expected_tooltip: &str,
    ) {
        let (visual, tooltip) = classify_tray(app, sync, pause);
        assert_eq!(visual, expected_visual);
        assert_eq!(tooltip, expected_tooltip);
    }

    #[test]
    fn classify_tray_basic_phases() {
        let sync = SyncSnapshot::default();
        assert_tray(
            AppPhase::Idle,
            &sync,
            None,
            TrayVisual::Pending,
            "solstone — idle",
        );
        assert_tray(
            AppPhase::Starting,
            &sync,
            None,
            TrayVisual::Pending,
            "solstone — starting",
        );
        assert_tray(
            AppPhase::Error,
            &sync,
            None,
            TrayVisual::Error,
            "solstone — attention needed",
        );
    }

    #[test]
    fn classify_tray_observing_sync_states() {
        assert_tray(
            AppPhase::Observing,
            &sync(PairingPhase::NotPaired, false),
            None,
            TrayVisual::Half,
            "solstone — observing, no journal connected",
        );
        assert_tray(
            AppPhase::Observing,
            &sync(PairingPhase::Paired, true),
            None,
            TrayVisual::Full,
            "solstone — observing, connected",
        );
        assert_tray(
            AppPhase::Observing,
            &sync(PairingPhase::Paired, false),
            None,
            TrayVisual::Half,
            "solstone — observing, saved on this PC",
        );
    }

    #[test]
    fn classify_tray_paused_states() {
        let sync = SyncSnapshot::default();
        let indefinite = PauseSnapshot {
            reason: PauseReason::Operator,
            seconds_remaining: None,
        };
        assert_tray(
            AppPhase::Paused,
            &sync,
            Some(&indefinite),
            TrayVisual::Cloud,
            "solstone — paused",
        );

        let bounded = PauseSnapshot {
            reason: PauseReason::Operator,
            seconds_remaining: Some(14 * 60 + 30),
        };
        assert_tray(
            AppPhase::Paused,
            &sync,
            Some(&bounded),
            TrayVisual::Cloud,
            "solstone — paused, 14 min left",
        );
    }

    #[test]
    fn remaining_formats_minutes_and_hours() {
        assert_eq!(format_remaining(0), "less than a minute");
        assert_eq!(format_remaining(59), "less than a minute");
        assert_eq!(format_remaining(60), "1 min");
        assert_eq!(format_remaining(14 * 60 + 30), "14 min");
        assert_eq!(format_remaining(60 * 60), "1 hr");
        assert_eq!(format_remaining(62 * 60), "1 hr 2 min");
    }
}
