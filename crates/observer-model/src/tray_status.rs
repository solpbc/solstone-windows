// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::{
    AppPhase, ErrorReason, HealthDump, JournalLink, MarkColor, MarkIconSpec, MarkRenderSpec,
    PairingPhase, PauseSnapshot, SourceKind, SourceReport, SourceState, StorageHealth,
    SyncSnapshot,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayVisual {
    Healthy,
    Connecting,
    Paused,
    Offline,
    NeedsPermission,
    Error,
    Off,
}

/// Whether there is anything to pause. `other_source_on` is a source that runs
/// beside capture and leaves the phase idle, so an owner using only that source
/// can still pause it.
pub fn pause_enabled(phase: AppPhase, other_source_on: bool) -> bool {
    matches!(phase, AppPhase::Starting | AppPhase::Observing)
        || (phase == AppPhase::Idle && other_source_on)
}

pub fn resume_enabled(phase: AppPhase) -> bool {
    matches!(phase, AppPhase::Paused)
}

pub fn restart_enabled(phase: AppPhase) -> bool {
    matches!(phase, AppPhase::Error)
}

/// The one menu attention row, in tile order. A fault on a source the owner
/// turned on counts, including a refused permission. Activity is the screen row.
pub fn attention_source(
    sources: &[SourceReport],
    is_enabled: impl Fn(SourceKind) -> bool,
    browsers_need_attention: bool,
    obsidian_needs_attention: bool,
    paused: bool,
) -> Option<&'static str> {
    let faulted = |kind: SourceKind| {
        is_enabled(kind)
            && sources.iter().any(|source| {
                source.kind == kind && matches!(source.state, SourceState::Faulted { .. })
            })
    };
    if faulted(SourceKind::Mic) || faulted(SourceKind::SystemAudio) {
        return Some("audio");
    }
    // No microphone, computer sound wanted, and nothing taking in. That is steady,
    // so the menu names it. A pause still reads as paused. NoInputDevice stays
    // its own state: this is not a capture fault.
    let state_of = |kind: SourceKind| {
        sources
            .iter()
            .find(|source| source.kind == kind)
            .map(|source| &source.state)
    };
    let active = |kind: SourceKind| matches!(state_of(kind), Some(SourceState::Active));
    let mic_missing = matches!(state_of(SourceKind::Mic), Some(SourceState::NoInputDevice));
    if !paused
        && is_enabled(SourceKind::SystemAudio)
        && mic_missing
        && !active(SourceKind::Mic)
        && !active(SourceKind::SystemAudio)
    {
        return Some("audio");
    }
    if faulted(SourceKind::Screen) || faulted(SourceKind::DesktopActivity) {
        return Some("screen");
    }
    if browsers_need_attention {
        return Some("browsers");
    }
    if obsidian_needs_attention {
        return Some("Obsidian");
    }
    None
}

/// How the disabled menu status line reads while paused. `{n}` is whole minutes, rounded up.
pub enum PausedMenuStatus {
    UntilResume,
    LessThanAMinute,
    Minutes(u64),
}

pub fn paused_menu_status(seconds_left: Option<u64>) -> PausedMenuStatus {
    match seconds_left {
        None => PausedMenuStatus::UntilResume,
        Some(secs) if secs < 60 => PausedMenuStatus::LessThanAMinute,
        Some(secs) => PausedMenuStatus::Minutes(secs.div_ceil(60)),
    }
}

/// The disabled menu row. While paused it is the time left. Otherwise it is the
/// journal pill. A failed pairing reads as not paired: the dump does not say
/// whether the mark was refused or the link needs replacing.
pub fn menu_status_line(dump: &crate::HealthDump) -> String {
    if dump.app_state == AppPhase::Paused {
        let seconds = dump
            .pause
            .as_ref()
            .and_then(|pause| pause.seconds_remaining);
        return match paused_menu_status(seconds) {
            PausedMenuStatus::UntilResume => "paused until you resume".to_string(),
            PausedMenuStatus::LessThanAMinute => "paused, less than a minute left".to_string(),
            PausedMenuStatus::Minutes(mins) => format!("paused, {mins} min left"),
        };
    }
    let waiting = dump
        .sync
        .upload
        .pending_segments
        .saturating_add(dump.sync.imports_waiting);
    let count = if waiting == 0 {
        String::new()
    } else {
        format!(" · {waiting} waiting")
    };
    match dump.sync.pairing.phase {
        PairingPhase::NotPaired | PairingPhase::Failed => "not paired".to_string(),
        PairingPhase::Pairing => "connecting…".to_string(),
        PairingPhase::AwaitingConfirmation => {
            if waiting == 0 {
                "confirm the mark".to_string()
            } else {
                format!("confirm the mark{count}")
            }
        }
        PairingPhase::Paired => match dump.sync.journal_link {
            Some(crate::JournalLink::Offline) => format!("offline{count}"),
            Some(crate::JournalLink::Connected) => "connected".to_string(),
            Some(crate::JournalLink::Connecting) | None => "connecting…".to_string(),
        },
    }
}

const FRAME_REACHED_AGO_MS: u64 = 40 * 60 * 1000;

/// A shot can show a paired journal this PC has not reached.
/// Both gates have to be set. A normal launch leaves the live reading alone.
/// The rendered dump changes. Nothing here dials a journal or writes a pair.
pub fn apply_frame_journal(
    dump: &mut HealthDump,
    frame_png: Option<&str>,
    journal: Option<&str>,
    now_ms: u64,
) {
    let Some(frame) = frame_png else {
        return;
    };
    if frame.is_empty() {
        return;
    }
    let Some(raw) = journal else {
        return;
    };
    match raw {
        "connected" => paint_frame(dump, JournalLink::Connected, 0, now_ms),
        "offline" => paint_frame(
            dump,
            JournalLink::Offline,
            0,
            now_ms.saturating_sub(FRAME_REACHED_AGO_MS),
        ),
        other => {
            let Some(count) = other.strip_prefix("offline:") else {
                return;
            };
            let Ok(count) = count.parse::<u64>() else {
                return;
            };
            if count == 0 {
                return;
            }
            paint_frame(
                dump,
                JournalLink::Offline,
                count,
                now_ms.saturating_sub(FRAME_REACHED_AGO_MS),
            );
        }
    }
}

fn paint_frame(dump: &mut HealthDump, link: JournalLink, waiting: u64, reached_ms: u64) {
    dump.sync.pairing.phase = PairingPhase::Paired;
    dump.sync.pairing.mark = Some(frame_mark());
    dump.sync.journal_link = Some(link);
    dump.sync.upload.pending_segments = waiting;
    dump.sync.upload.last_successful_sync = Some(reached_ms);
}

fn frame_mark() -> MarkRenderSpec {
    MarkRenderSpec {
        icon1: frame_chip("amber", "#E07A3D"),
        icon2: frame_chip("clay", "#C4554A"),
        words: ["bright".into(), "harbor".into()],
    }
}

fn frame_chip(name: &str, hex: &str) -> MarkIconSpec {
    MarkIconSpec {
        name: name.into(),
        svg: r#"<circle cx="12" cy="12" r="4"/>"#.into(),
        color: MarkColor {
            name: name.into(),
            hex: hex.into(),
        },
        rot: 0,
    }
}

const STORAGE_FAULT_FOR_OWNER: &str = "can't save on this PC";

/// What the tray says stopped, in owner words. A fault's detail is a diagnostic
/// for the logs and the health dump, never shown here.
pub fn owner_fault_detail(
    sources: &[SourceReport],
    storage: Option<&StorageHealth>,
    is_enabled: impl Fn(crate::SourceKind) -> bool,
) -> Option<String> {
    if storage.is_some() {
        return Some(STORAGE_FAULT_FOR_OWNER.to_string());
    }

    sources
        .iter()
        .filter(|source| is_enabled(source.kind))
        .find_map(|source| match &source.state {
            SourceState::Faulted { reason, .. } if *reason != ErrorReason::AccessDenied => {
                Some(format!(
                    "the {} stopped going into your journal",
                    source_owner_name(source.kind)
                ))
            }
            _ => None,
        })
}

/// The first enabled source that Windows is refusing access to, if any.
pub fn permission_blocked(
    sources: &[SourceReport],
    is_enabled: impl Fn(SourceKind) -> bool,
) -> Option<SourceKind> {
    sources
        .iter()
        .filter(|source| is_enabled(source.kind))
        .find(|source| {
            matches!(
                source.state,
                SourceState::Faulted {
                    reason: ErrorReason::AccessDenied,
                    ..
                }
            )
        })
        .map(|source| source.kind)
}

/// How a source is named in owner-visible copy.
pub fn source_owner_name(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Screen => "screen",
        SourceKind::SystemAudio => "system audio",
        SourceKind::Mic => "microphone",
        SourceKind::DesktopActivity => "desktop activity",
    }
}

pub fn classify_tray(
    app: AppPhase,
    sync: &SyncSnapshot,
    pause: Option<&PauseSnapshot>,
    fault_detail: Option<&str>,
    blocked: Option<SourceKind>,
    deliberate_off: bool,
) -> (TrayVisual, String) {
    let (visual, tooltip) = match (app, blocked) {
        // A pause or a fault outranks a refused permission; otherwise the owner
        // has something to turn on in Windows, whatever else is running.
        (AppPhase::Idle | AppPhase::Starting | AppPhase::Observing, Some(kind))
            if !deliberate_off =>
        {
            let needs = format!(
                "the {} needs permission in windows settings",
                source_owner_name(kind)
            );
            let tooltip = if app == AppPhase::Observing {
                format!("on, but {needs}")
            } else {
                needs
            };
            (TrayVisual::NeedsPermission, tooltip)
        }
        _ => classify_phase(app, sync, pause, fault_detail, deliberate_off),
    };

    // Additive: an unknown-journal sighting never changes the tray's visual or
    // otherwise-computed tooltip, it only appends a notice line — same wording
    // as the Settings detail card, earned independently of app phase or pairing
    // state (a sighting can happen while paired-and-healthy just as easily as
    // while reconnecting).
    match sync.unknown_journals.first() {
        Some(sighting) => {
            let notice = match &sighting.address {
                Some(address) => format!("unknown journal seen at {address}"),
                None => "unknown journal seen through the relay".to_string(),
            };
            (visual, format!("{tooltip}\n{notice}"))
        }
        None => (visual, tooltip),
    }
}

fn classify_phase(
    app: AppPhase,
    sync: &SyncSnapshot,
    pause: Option<&PauseSnapshot>,
    fault_detail: Option<&str>,
    deliberate_off: bool,
) -> (TrayVisual, String) {
    match app {
        AppPhase::Idle => {
            if deliberate_off {
                (TrayVisual::Off, "off".to_string())
            } else {
                (TrayVisual::Connecting, "connecting".to_string())
            }
        }
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
            PairingPhase::AwaitingConfirmation if sync.pairing.finish_connecting => (
                TrayVisual::Paused,
                "finish connecting to your journal".to_string(),
            ),
            PairingPhase::AwaitingConfirmation => (
                TrayVisual::Paused,
                "solstone · waiting for you to confirm your journal's mark".to_string(),
            ),
            PairingPhase::NotPaired | PairingPhase::Failed => (
                TrayVisual::Paused,
                "on, not connected to a journal".to_string(),
            ),
            // The presented link, never the upload counters: see `journal_link`.
            PairingPhase::Paired => match sync.journal_link {
                Some(JournalLink::Connected) => (
                    TrayVisual::Healthy,
                    "on, connected to your journal".to_string(),
                ),
                Some(JournalLink::Offline) => {
                    (TrayVisual::Offline, "on, saved on this PC".to_string())
                }
                Some(JournalLink::Connecting) | None => {
                    (TrayVisual::Connecting, "connecting".to_string())
                }
            },
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
                ..Default::default()
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
        classify_tray(app, sync, pause, fault_detail, None, false).0
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
    fn observing_paired_shows_the_presented_link() {
        let mut paired = sync(PairingPhase::Paired);
        for (link, visual) in [
            (Some(JournalLink::Connected), TrayVisual::Healthy),
            (Some(JournalLink::Connecting), TrayVisual::Connecting),
            (Some(JournalLink::Offline), TrayVisual::Offline),
            (None, TrayVisual::Connecting),
        ] {
            paired.journal_link = link;
            assert_eq!(
                tray_visual(AppPhase::Observing, &paired, None, None),
                visual
            );
        }
    }

    #[test]
    fn upload_counters_do_not_decide_the_link() {
        // A rejected segment, a failed tick or uploads this session are not
        // the link: the journal answered, or it did not.
        let mut paired = sync(PairingPhase::Paired);
        paired.journal_link = Some(JournalLink::Connected);
        paired.upload = UploadStatus {
            recent_error_count: 1,
            last_error_reason: Some("http_503".into()),
            last_error: Some("http_413".into()),
            failed_segments: 4,
            ..UploadStatus::default()
        };
        assert_eq!(
            tray_visual(AppPhase::Observing, &paired, None, None),
            TrayVisual::Healthy
        );

        paired.journal_link = Some(JournalLink::Connecting);
        paired.upload = UploadStatus {
            uploaded_segments: 3,
            last_successful_sync: Some(1),
            ..UploadStatus::default()
        };
        assert_eq!(
            tray_visual(AppPhase::Observing, &paired, None, None),
            TrayVisual::Connecting
        );
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
    fn storage_fault_outranks_and_never_shows_its_diagnostic() {
        let sources = vec![faulted_source(SourceKind::Screen, "screen gone")];
        let storage = StorageHealth {
            detail: "Os { code: 112, kind: StorageFull }".into(),
        };

        let detail = owner_fault_detail(&sources, Some(&storage), |_| true).unwrap();
        assert!(!detail.contains("Os {"));
        assert!(!detail.contains("screen gone"));
    }

    #[test]
    fn source_fault_names_the_source_not_its_diagnostic() {
        let sources = vec![faulted_source(
            SourceKind::Screen,
            "[breaker-open] graphics capture item closed",
        )];
        let detail = owner_fault_detail(&sources, None, |_| true);

        let (visual, tooltip) = classify_tray(
            AppPhase::Error,
            &SyncSnapshot::default(),
            None,
            detail.as_deref(),
            None,
            false,
        );
        assert_eq!(visual, TrayVisual::Error);
        assert!(tooltip.contains(source_owner_name(SourceKind::Screen)));
        assert!(!tooltip.contains("graphics capture"));
    }

    #[test]
    fn no_fault_detail_uses_error_fallback() {
        assert_eq!(owner_fault_detail(&[], None, |_| true), None);
        assert_eq!(
            tray_visual(AppPhase::Error, &SyncSnapshot::default(), None, None),
            TrayVisual::Error
        );
    }

    #[test]
    fn tray_action_enablement_matches_phase() {
        assert!(pause_enabled(AppPhase::Starting, false));
        assert!(pause_enabled(AppPhase::Observing, false));
        assert!(!pause_enabled(AppPhase::Paused, false));
        assert!(!pause_enabled(AppPhase::Error, false));

        // Idle has nothing to pause, unless a source beside capture is on.
        assert!(!pause_enabled(AppPhase::Idle, false));
        assert!(pause_enabled(AppPhase::Idle, true));
        assert!(!pause_enabled(AppPhase::Paused, true));
        assert!(!pause_enabled(AppPhase::Error, true));

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

    fn sample_mark() -> crate::MarkRenderSpec {
        crate::MarkRenderSpec {
            icon1: crate::MarkIconSpec {
                name: "piano".into(),
                svg: "<path d=\"...\"/>".into(),
                color: crate::MarkColor {
                    name: "blue".into(),
                    hex: "#3b82f6".into(),
                },
                rot: 45,
            },
            icon2: crate::MarkIconSpec {
                name: "key".into(),
                svg: "<path d=\"...\"/>".into(),
                color: crate::MarkColor {
                    name: "purple".into(),
                    hex: "#a855f7".into(),
                },
                rot: 0,
            },
            words: ["liquefy".into(), "smock".into()],
        }
    }

    fn sighting(address: Option<&str>) -> crate::UnknownJournalSighting {
        crate::UnknownJournalSighting {
            address: address.map(|a| a.to_string()),
            expected_mark: sample_mark(),
            responding_mark: None,
        }
    }

    #[test]
    fn unknown_journal_sighting_appends_notice_with_address() {
        let mut clean = sync(PairingPhase::Paired);
        clean.journal_link = Some(JournalLink::Connected);
        clean.unknown_journals = vec![sighting(Some("192.168.1.50:7657"))];

        let (visual, tooltip) = classify_tray(AppPhase::Observing, &clean, None, None, None, false);
        assert_eq!(visual, TrayVisual::Healthy);
        assert_eq!(
            tooltip,
            "on, connected to your journal\nunknown journal seen at 192.168.1.50:7657"
        );
    }

    #[test]
    fn unknown_journal_sighting_appends_notice_through_relay() {
        let mut clean = sync(PairingPhase::Paired);
        clean.journal_link = Some(JournalLink::Connected);
        clean.unknown_journals = vec![sighting(None)];

        let (_, tooltip) = classify_tray(AppPhase::Observing, &clean, None, None, None, false);
        assert_eq!(
            tooltip,
            "on, connected to your journal\nunknown journal seen through the relay"
        );
    }

    #[test]
    fn unknown_journal_sighting_appends_regardless_of_phase() {
        let paused_sync = SyncSnapshot {
            unknown_journals: vec![sighting(Some("10.0.0.1:443"))],
            ..Default::default()
        };
        let indefinite = PauseSnapshot {
            reason: PauseReason::Operator,
            seconds_remaining: None,
        };

        let (visual, tooltip) = classify_tray(
            AppPhase::Paused,
            &paused_sync,
            Some(&indefinite),
            None,
            None,
            false,
        );
        assert_eq!(visual, TrayVisual::Paused);
        assert_eq!(tooltip, "paused\nunknown journal seen at 10.0.0.1:443");
    }

    #[test]
    fn no_unknown_journal_sighting_leaves_tooltip_untouched() {
        let idle = SyncSnapshot::default();
        let (_, tooltip) = classify_tray(AppPhase::Idle, &idle, None, None, None, false);
        assert_eq!(tooltip, "connecting");
        assert!(!tooltip.contains('\n'));
    }

    #[test]
    fn mark_confirmation_classify_tray_awaiting_confirmation_tooltip() {
        let awaiting_sync = sync(PairingPhase::AwaitingConfirmation);
        let (visual, tooltip) =
            classify_tray(AppPhase::Observing, &awaiting_sync, None, None, None, false);
        assert_eq!(visual, TrayVisual::Paused);
        assert_eq!(
            tooltip,
            "solstone · waiting for you to confirm your journal's mark"
        );

        let mut with_sighting = awaiting_sync;
        with_sighting.unknown_journals = vec![sighting(Some("10.0.0.1:443"))];
        let (visual, tooltip) =
            classify_tray(AppPhase::Observing, &with_sighting, None, None, None, false);
        assert_eq!(visual, TrayVisual::Paused);
        assert_eq!(
            tooltip,
            "solstone · waiting for you to confirm your journal's mark\nunknown journal seen at 10.0.0.1:443"
        );
    }

    #[test]
    fn finish_connecting_tooltip_asks_for_no_mark() {
        let mut finishing = sync(PairingPhase::AwaitingConfirmation);
        finishing.pairing.finish_connecting = true;
        let (visual, tooltip) =
            classify_tray(AppPhase::Observing, &finishing, None, None, None, false);
        assert_eq!(visual, TrayVisual::Paused);
        assert!(!tooltip.contains("mark"));
    }

    #[test]
    fn classify_deliberate_off_visual() {
        let idle = SyncSnapshot::default();
        let (visual, tooltip) = classify_tray(AppPhase::Idle, &idle, None, None, None, true);
        assert_eq!(visual, TrayVisual::Off);
        assert_eq!(tooltip, "off");

        // Without deliberate_off, Idle stays connecting
        let (visual2, tooltip2) = classify_tray(AppPhase::Idle, &idle, None, None, None, false);
        assert_eq!(visual2, TrayVisual::Connecting);
        assert_eq!(tooltip2, "connecting");
    }

    #[test]
    fn refused_permission_is_never_shown_as_healthy() {
        let mut clean = sync(PairingPhase::Paired);
        clean.journal_link = Some(JournalLink::Connected);
        let blocked = Some(SourceKind::Mic);

        let (visual, _) = classify_tray(AppPhase::Observing, &clean, None, None, blocked, false);
        assert_eq!(visual, TrayVisual::NeedsPermission);
        // With nothing else enabled the reducer reports idle; still not "connecting".
        let (visual, _) = classify_tray(AppPhase::Idle, &clean, None, None, blocked, false);
        assert_eq!(visual, TrayVisual::NeedsPermission);
    }

    #[test]
    fn pause_and_fault_outrank_a_refused_permission() {
        let idle = SyncSnapshot::default();
        let blocked = Some(SourceKind::Mic);
        let pause = PauseSnapshot {
            reason: PauseReason::Operator,
            seconds_remaining: None,
        };
        let (visual, _) =
            classify_tray(AppPhase::Paused, &idle, Some(&pause), None, blocked, false);
        assert_eq!(visual, TrayVisual::Paused);
        let (visual, _) = classify_tray(AppPhase::Error, &idle, None, None, blocked, false);
        assert_eq!(visual, TrayVisual::Error);
    }

    #[test]
    fn permission_blocked_reads_only_enabled_access_denied_sources() {
        let denied = SourceReport {
            kind: SourceKind::Mic,
            state: SourceState::Faulted {
                reason: ErrorReason::AccessDenied,
                detail: String::new(),
            },
            device: None,
        };
        let other_fault = faulted_source(SourceKind::Screen, "screen gone");
        let sources = [other_fault, denied];
        assert_eq!(
            permission_blocked(&sources, |_| true),
            Some(SourceKind::Mic)
        );
        assert_eq!(
            permission_blocked(&sources, |kind| kind != SourceKind::Mic),
            None
        );
    }

    #[test]
    fn owner_fault_detail_skips_disabled_source() {
        let sources = [
            faulted_source(SourceKind::Screen, "screen broken"),
            faulted_source(SourceKind::Mic, "mic broken"),
        ];
        // Screen is disabled, mic is enabled -> names the mic
        let detail = owner_fault_detail(&sources, None, |k| k == SourceKind::Mic).unwrap();
        assert!(detail.contains(source_owner_name(SourceKind::Mic)));

        // All disabled -> returns None
        let detail_none = owner_fault_detail(&sources, None, |_| false);
        assert_eq!(detail_none, None);
    }

    fn denied(kind: SourceKind) -> SourceReport {
        SourceReport {
            kind,
            state: SourceState::Faulted {
                reason: ErrorReason::AccessDenied,
                detail: "denied".into(),
            },
            device: None,
        }
    }

    #[test]
    fn attention_names_the_first_source_and_skips_one_that_is_off() {
        let mic = [
            denied(SourceKind::Mic),
            faulted_source(SourceKind::Screen, "x"),
        ];
        assert_eq!(
            attention_source(&mic, |_| true, true, true, false),
            Some("audio")
        );
        let screen = [faulted_source(SourceKind::DesktopActivity, "x")];
        assert_eq!(
            attention_source(&screen, |_| true, false, false, false),
            Some("screen")
        );
        assert_eq!(
            attention_source(&[], |_| true, true, true, false),
            Some("browsers")
        );
        assert_eq!(
            attention_source(&[], |_| true, false, true, false),
            Some("Obsidian")
        );
        assert_eq!(attention_source(&[], |_| true, false, false, false), None);
        let off = [denied(SourceKind::Mic)];
        assert_eq!(attention_source(&off, |_| false, false, false, false), None);
    }

    #[test]
    fn a_missing_microphone_with_computer_sound_wanted_and_nothing_taking_in_is_audio() {
        let sources = [
            SourceReport {
                kind: SourceKind::Mic,
                state: SourceState::NoInputDevice,
                device: None,
            },
            SourceReport {
                kind: SourceKind::SystemAudio,
                state: SourceState::Inactive,
                device: None,
            },
        ];
        let enabled = |kind: SourceKind| kind == SourceKind::SystemAudio;
        assert_eq!(
            attention_source(&sources, enabled, false, false, false),
            Some("audio")
        );
        assert_eq!(
            attention_source(&sources, enabled, false, false, true),
            None
        );
        assert_eq!(
            attention_source(&sources, |_| false, false, false, false),
            None
        );
    }

    fn dump_with(
        phase: PairingPhase,
        link: Option<crate::JournalLink>,
        pending: u64,
    ) -> crate::HealthDump {
        crate::HealthDump {
            app_state: AppPhase::Observing,
            sources: Vec::new(),
            frame_rate: None,
            segment_dir: None,
            segment_seconds_remaining: None,
            engine_ready: true,
            version: "test".into(),
            sync: SyncSnapshot {
                pairing: PairingState {
                    phase,
                    ..Default::default()
                },
                upload: UploadStatus {
                    pending_segments: pending,
                    ..Default::default()
                },
                journal_link: link,
                ..Default::default()
            },
            screen_encoder: None,
            exclusions: None,
            storage: None,
            pause: None,
            views: Default::default(),
            pump_degraded: false,
            listener_faults: Default::default(),
            setup: Default::default(),
        }
    }

    #[test]
    fn menu_status_uses_the_pill_and_rounds_a_pause() {
        let connected = dump_with(PairingPhase::Paired, Some(crate::JournalLink::Connected), 0);
        let offline = dump_with(PairingPhase::Paired, Some(crate::JournalLink::Offline), 3);
        assert_ne!(menu_status_line(&connected), menu_status_line(&offline));
        assert!(menu_status_line(&offline).contains('3'));
        assert_eq!(
            menu_status_line(&dump_with(PairingPhase::NotPaired, None, 0)),
            menu_status_line(&dump_with(PairingPhase::Failed, None, 4))
        );

        let mut paused = dump_with(PairingPhase::Paired, Some(crate::JournalLink::Connected), 0);
        paused.app_state = AppPhase::Paused;
        paused.pause = Some(PauseSnapshot {
            reason: PauseReason::Operator,
            seconds_remaining: Some(61),
        });
        let one_minute = {
            let mut dump = paused.clone();
            dump.pause = Some(PauseSnapshot {
                reason: PauseReason::Operator,
                seconds_remaining: Some(60),
            });
            menu_status_line(&dump)
        };
        let two_minutes = menu_status_line(&paused);
        assert!(two_minutes.contains('2'));
        assert_ne!(two_minutes, one_minute);
        paused.pause = Some(PauseSnapshot {
            reason: PauseReason::Operator,
            seconds_remaining: None,
        });
        assert_ne!(menu_status_line(&paused), two_minutes);
    }

    #[test]
    fn menu_status_line_is_never_blank() {
        let phases = [
            PairingPhase::NotPaired,
            PairingPhase::Failed,
            PairingPhase::Pairing,
            PairingPhase::AwaitingConfirmation,
            PairingPhase::Paired,
        ];
        for phase in phases {
            for link in [
                None,
                Some(crate::JournalLink::Connected),
                Some(crate::JournalLink::Connecting),
                Some(crate::JournalLink::Offline),
            ] {
                for pending in [0, 4] {
                    let line = menu_status_line(&dump_with(phase, link, pending));
                    assert!(!line.trim().is_empty());
                }
            }
        }
        let quiet = menu_status_line(&dump_with(
            PairingPhase::Paired,
            Some(crate::JournalLink::Offline),
            0,
        ));
        let counted = menu_status_line(&dump_with(
            PairingPhase::Paired,
            Some(crate::JournalLink::Offline),
            4,
        ));
        assert_ne!(quiet, counted);
        assert!(counted.contains('4'));
        assert!(!quiet.chars().any(|ch| ch.is_ascii_digit()));
        assert!(quiet.contains("offline"));
    }

    #[test]
    fn frame_journal_stays_off_without_both_gates() {
        let mut dump = dump_with(PairingPhase::NotPaired, None, 0);
        let before = dump.clone();
        apply_frame_journal(&mut dump, None, Some("connected"), 1_000);
        assert_eq!(dump, before);
        apply_frame_journal(&mut dump, Some(""), Some("connected"), 1_000);
        assert_eq!(dump, before);
        apply_frame_journal(&mut dump, Some("frame"), None, 1_000);
        assert_eq!(dump, before);
        apply_frame_journal(&mut dump, Some("frame"), Some("nope"), 1_000);
        assert_eq!(dump, before);
        apply_frame_journal(&mut dump, Some("frame"), Some("offline:0"), 1_000);
        assert_eq!(dump, before);
        apply_frame_journal(&mut dump, Some("frame"), Some("offline:no"), 1_000);
        assert_eq!(dump, before);
    }

    #[test]
    fn frame_journal_connected_and_offline_are_a_paired_reading() {
        let now = 1_700_000_000_000_u64;
        let mut connected = dump_with(PairingPhase::NotPaired, None, 4);
        apply_frame_journal(&mut connected, Some("frame"), Some("connected"), now);
        assert_eq!(connected.sync.pairing.phase, PairingPhase::Paired);
        assert_eq!(connected.sync.journal_link, Some(JournalLink::Connected));
        assert_eq!(connected.sync.upload.pending_segments, 0);
        assert_eq!(connected.sync.upload.last_successful_sync, Some(now));
        assert_eq!(
            connected
                .sync
                .pairing
                .mark
                .as_ref()
                .map(|mark| mark.words.clone()),
            Some(["bright".into(), "harbor".into()])
        );
        assert_eq!(menu_status_line(&connected), "connected");

        let mut offline = dump_with(PairingPhase::NotPaired, None, 0);
        apply_frame_journal(&mut offline, Some("frame"), Some("offline:12"), now);
        assert_eq!(offline.sync.journal_link, Some(JournalLink::Offline));
        assert_eq!(offline.sync.upload.pending_segments, 12);
        assert_eq!(
            offline.sync.upload.last_successful_sync,
            Some(now - 40 * 60 * 1000)
        );
        assert_eq!(menu_status_line(&offline), "offline · 12 waiting");

        let mut quiet = dump_with(PairingPhase::NotPaired, None, 9);
        apply_frame_journal(&mut quiet, Some("frame"), Some("offline"), now);
        assert_eq!(quiet.sync.upload.pending_segments, 0);
        assert_eq!(menu_status_line(&quiet), "offline");
    }

    #[test]
    fn paused_menu_status_rounds_up_and_keeps_indefinite_apart() {
        assert!(matches!(
            paused_menu_status(None),
            PausedMenuStatus::UntilResume
        ));
        assert!(matches!(
            paused_menu_status(Some(59)),
            PausedMenuStatus::LessThanAMinute
        ));
        assert!(matches!(
            paused_menu_status(Some(60)),
            PausedMenuStatus::Minutes(1)
        ));
        assert!(matches!(
            paused_menu_status(Some(61)),
            PausedMenuStatus::Minutes(2)
        ));
    }
}
