// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::{
    AppPhase, PairingPhase, PauseSnapshot, SourceReport, SourceState, StorageHealth, SyncSnapshot,
    BREAKER_OPEN_MARKER,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayVisual {
    Healthy,
    Connecting,
    Paused,
    Offline,
    Error,
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
        AppPhase::Idle => (TrayVisual::Connecting, "connecting".to_string()),
        AppPhase::Starting => (TrayVisual::Connecting, "starting…".to_string()),
        AppPhase::Paused => {
            let tooltip = match pause.and_then(|p| p.seconds_remaining) {
                Some(secs) => format!("paused, {} left", format_remaining(secs)),
                None => "paused".to_string(),
            };
            (TrayVisual::Paused, tooltip)
        }
        AppPhase::Error => match fault_detail.filter(|detail| !detail.is_empty()) {
            Some(detail) => (TrayVisual::Error, detail.to_string()),
            None => (TrayVisual::Error, "needs a restart".to_string()),
        },
        AppPhase::Observing => match sync.pairing.phase {
            PairingPhase::Pairing => (TrayVisual::Connecting, "connecting".to_string()),
            PairingPhase::NotPaired | PairingPhase::Failed => (
                TrayVisual::Paused,
                "on, not connected to a journal".to_string(),
            ),
            PairingPhase::Paired => {
                let upload = &sync.upload;
                if upload.recent_error_count > 0
                    || upload.last_error_reason.is_some()
                    || upload.last_error.is_some()
                {
                    (TrayVisual::Offline, "on, saved on this PC".to_string())
                } else if upload.uploaded_segments > 0 && upload.last_successful_sync.is_some() {
                    (
                        TrayVisual::Healthy,
                        "on, connected to your journal".to_string(),
                    )
                } else {
                    (TrayVisual::Connecting, "connecting".to_string())
                }
            }
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

    fn sync(phase: PairingPhase) -> SyncSnapshot {
        SyncSnapshot {
            pairing: PairingState {
                phase,
                journal_label: None,
                detail: None,
            },
            upload: UploadStatus::default(),
            ..Default::default()
        }
    }

    fn tray_visual(
        app: AppPhase,
        sync: &SyncSnapshot,
        pause: Option<&PauseSnapshot>,
        fault_detail: Option<&str>,
    ) -> TrayVisual {
        classify_tray(app, sync, pause, fault_detail).0
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
    fn classify_connecting_visuals() {
        let idle = SyncSnapshot::default();
        assert_eq!(
            tray_visual(AppPhase::Idle, &idle, None, None),
            TrayVisual::Connecting
        );
        assert_eq!(
            tray_visual(AppPhase::Starting, &idle, None, None),
            TrayVisual::Connecting
        );
        assert_eq!(
            tray_visual(
                AppPhase::Observing,
                &sync(PairingPhase::Pairing),
                None,
                None
            ),
            TrayVisual::Connecting
        );
    }

    #[test]
    fn classify_paused_visuals() {
        let idle = SyncSnapshot::default();
        let indefinite = PauseSnapshot {
            reason: PauseReason::Operator,
            seconds_remaining: None,
        };
        assert_eq!(
            tray_visual(AppPhase::Paused, &idle, Some(&indefinite), None),
            TrayVisual::Paused
        );

        let bounded = PauseSnapshot {
            reason: PauseReason::Operator,
            seconds_remaining: Some(14 * 60 + 30),
        };
        assert_eq!(
            tray_visual(AppPhase::Paused, &idle, Some(&bounded), None),
            TrayVisual::Paused
        );

        assert_eq!(
            tray_visual(
                AppPhase::Observing,
                &sync(PairingPhase::NotPaired),
                None,
                None
            ),
            TrayVisual::Paused
        );
        assert_eq!(
            tray_visual(AppPhase::Observing, &sync(PairingPhase::Failed), None, None),
            TrayVisual::Paused
        );
    }

    #[test]
    fn classify_observing_paired_visuals() {
        let mut clean_upload = sync(PairingPhase::Paired);
        clean_upload.upload.uploaded_segments = 1;
        clean_upload.upload.last_successful_sync = Some(1);
        assert_eq!(
            tray_visual(AppPhase::Observing, &clean_upload, None, None),
            TrayVisual::Healthy
        );

        let empty_success = SyncSnapshot {
            pairing: PairingState {
                phase: PairingPhase::Paired,
                ..Default::default()
            },
            upload: UploadStatus {
                last_successful_sync: Some(1),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            tray_visual(AppPhase::Observing, &empty_success, None, None),
            TrayVisual::Connecting
        );

        for upload in [
            UploadStatus {
                recent_error_count: 1,
                ..UploadStatus::default()
            },
            UploadStatus {
                last_error_reason: Some("http_503".into()),
                ..UploadStatus::default()
            },
            UploadStatus {
                last_error: Some("uploader_stopped".into()),
                ..UploadStatus::default()
            },
        ] {
            let offline = SyncSnapshot {
                pairing: PairingState {
                    phase: PairingPhase::Paired,
                    ..Default::default()
                },
                upload,
                ..Default::default()
            };
            assert_eq!(
                tray_visual(AppPhase::Observing, &offline, None, None),
                TrayVisual::Offline
            );
        }
    }

    #[test]
    fn observing_not_paired_is_paused_not_offline() {
        let visual = tray_visual(
            AppPhase::Observing,
            &sync(PairingPhase::NotPaired),
            None,
            None,
        );
        assert_eq!(visual, TrayVisual::Paused);
        assert_ne!(visual, TrayVisual::Offline);
    }

    #[test]
    fn observing_failed_is_paused_not_error() {
        let visual = tray_visual(AppPhase::Observing, &sync(PairingPhase::Failed), None, None);
        assert_eq!(visual, TrayVisual::Paused);
        assert_ne!(visual, TrayVisual::Error);
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

        let (visual, tooltip) = classify_tray(
            AppPhase::Error,
            &SyncSnapshot::default(),
            None,
            detail.as_deref(),
        );
        assert_eq!(visual, TrayVisual::Error);
        assert_eq!(tooltip, "disk full");
    }

    #[test]
    fn no_fault_detail_uses_error_fallback() {
        assert_eq!(owner_fault_detail(&[], None), None);
        assert_eq!(
            tray_visual(AppPhase::Error, &SyncSnapshot::default(), None, None),
            TrayVisual::Error
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
