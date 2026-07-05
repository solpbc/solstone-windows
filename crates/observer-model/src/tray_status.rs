// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::{
    AppPhase, PairingPhase, PauseSnapshot, SourceReport, SourceState, StorageHealth, SyncSnapshot,
    BREAKER_OPEN_MARKER,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayVisual {
    Full,
    Half,
    Cloud,
    Error,
    Pending,
}

pub fn pause_enabled(phase: AppPhase) -> bool {
    matches!(phase, AppPhase::Starting | AppPhase::Observing)
}

pub fn resume_enabled(phase: AppPhase) -> bool {
    matches!(phase, AppPhase::Paused)
}

pub fn restart_enabled(phase: AppPhase) -> bool {
    matches!(phase, AppPhase::Error)
}

pub fn owner_fault_detail(
    sources: &[SourceReport],
    storage: Option<&StorageHealth>,
) -> Option<String> {
    fn detail_for_owner(detail: &str) -> Option<String> {
        let detail = detail
            .strip_prefix(BREAKER_OPEN_MARKER)
            .unwrap_or(detail)
            .trim();
        (!detail.is_empty()).then(|| detail.to_string())
    }

    if let Some(detail) = storage.and_then(|storage| detail_for_owner(&storage.detail)) {
        return Some(detail);
    }

    sources.iter().find_map(|source| match &source.state {
        SourceState::Faulted { detail, .. } => detail_for_owner(detail),
        _ => None,
    })
}

pub fn classify_tray(
    app: AppPhase,
    sync: &SyncSnapshot,
    pause: Option<&PauseSnapshot>,
    fault_detail: Option<&str>,
) -> (TrayVisual, String) {
    match app {
        AppPhase::Idle => (TrayVisual::Pending, "sol — idle".to_string()),
        AppPhase::Starting => (TrayVisual::Pending, "sol — starting…".to_string()),
        AppPhase::Paused => {
            let tooltip = match pause.and_then(|p| p.seconds_remaining) {
                Some(secs) => format!("sol — paused, {} left", format_remaining(secs)),
                None => "sol — paused".to_string(),
            };
            (TrayVisual::Cloud, tooltip)
        }
        AppPhase::Error => match fault_detail {
            Some(detail) if !detail.is_empty() => (TrayVisual::Error, format!("sol — {detail}")),
            _ => (TrayVisual::Error, "sol — needs attention".to_string()),
        },
        AppPhase::Observing => match (sync.pairing.phase, sync.upload.heartbeat_ok) {
            (PairingPhase::Paired, true) => (
                TrayVisual::Full,
                "sol — on, connected to your journal".to_string(),
            ),
            (PairingPhase::Paired, false) => {
                (TrayVisual::Half, "sol — on, saved on this PC".to_string())
            }
            _ => (
                TrayVisual::Half,
                "sol — on, no journal connected".to_string(),
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
    use crate::{ErrorReason, PairingState, PauseReason, SourceKind, UploadStatus};

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
        fault_detail: Option<&str>,
        expected_visual: TrayVisual,
        expected_tooltip: &str,
    ) {
        let (visual, tooltip) = classify_tray(app, sync, pause, fault_detail);
        assert_eq!(visual, expected_visual);
        assert_eq!(tooltip, expected_tooltip);
    }

    fn faulted_source(kind: SourceKind, detail: impl Into<String>) -> SourceReport {
        SourceReport {
            kind,
            state: SourceState::Faulted {
                reason: ErrorReason::EndpointLost,
                detail: detail.into(),
            },
            device: None,
        }
    }

    #[test]
    fn classify_tray_basic_phases() {
        let sync = SyncSnapshot::default();
        assert_tray(
            AppPhase::Idle,
            &sync,
            None,
            None,
            TrayVisual::Pending,
            "sol — idle",
        );
        assert_tray(
            AppPhase::Starting,
            &sync,
            None,
            None,
            TrayVisual::Pending,
            "sol — starting…",
        );
        assert_tray(
            AppPhase::Error,
            &sync,
            None,
            None,
            TrayVisual::Error,
            "sol — needs attention",
        );
    }

    #[test]
    fn classify_tray_observing_sync_states() {
        assert_tray(
            AppPhase::Observing,
            &sync(PairingPhase::NotPaired, false),
            None,
            None,
            TrayVisual::Half,
            "sol — on, no journal connected",
        );
        assert_tray(
            AppPhase::Observing,
            &sync(PairingPhase::Paired, true),
            None,
            None,
            TrayVisual::Full,
            "sol — on, connected to your journal",
        );
        assert_tray(
            AppPhase::Observing,
            &sync(PairingPhase::Paired, false),
            None,
            None,
            TrayVisual::Half,
            "sol — on, saved on this PC",
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
            None,
            TrayVisual::Cloud,
            "sol — paused",
        );

        let bounded = PauseSnapshot {
            reason: PauseReason::Operator,
            seconds_remaining: Some(14 * 60 + 30),
        };
        assert_tray(
            AppPhase::Paused,
            &sync,
            Some(&bounded),
            None,
            TrayVisual::Cloud,
            "sol — paused, 14 min left",
        );
    }

    #[test]
    fn owner_fault_detail_prefers_storage_detail() {
        let sources = vec![faulted_source(SourceKind::Screen, "screen gone")];
        let storage = StorageHealth {
            detail: "disk full".into(),
        };

        assert_eq!(
            owner_fault_detail(&sources, Some(&storage)).as_deref(),
            Some("disk full")
        );
    }

    #[test]
    fn marker_stripped_detail_reaches_error_tooltip() {
        let storage = StorageHealth {
            detail: "[breaker-open] disk full".into(),
        };
        let detail = owner_fault_detail(&[], Some(&storage));

        assert_tray(
            AppPhase::Error,
            &SyncSnapshot::default(),
            None,
            detail.as_deref(),
            TrayVisual::Error,
            "sol — disk full",
        );
    }

    #[test]
    fn no_fault_detail_uses_error_fallback() {
        assert_eq!(owner_fault_detail(&[], None), None);

        assert_tray(
            AppPhase::Error,
            &SyncSnapshot::default(),
            None,
            None,
            TrayVisual::Error,
            "sol — needs attention",
        );
    }

    #[test]
    fn tray_action_enablement_matches_phase() {
        assert!(pause_enabled(AppPhase::Starting));
        assert!(pause_enabled(AppPhase::Observing));
        assert!(!pause_enabled(AppPhase::Paused));
        assert!(!pause_enabled(AppPhase::Error));

        assert!(!resume_enabled(AppPhase::Starting));
        assert!(!resume_enabled(AppPhase::Observing));
        assert!(resume_enabled(AppPhase::Paused));
        assert!(!resume_enabled(AppPhase::Error));

        assert!(!restart_enabled(AppPhase::Starting));
        assert!(!restart_enabled(AppPhase::Observing));
        assert!(!restart_enabled(AppPhase::Paused));
        assert!(restart_enabled(AppPhase::Error));
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
