// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The honest app-state reducer.
//!
//! The cardinal rule: [`AppPhase::Observing`] is **computed**, never set. The UI
//! and the IPC commands feed *intents* ([`AppEvent`]) into [`reduce`]; the
//! resulting phase is derived from the engine's run flag plus the real reported
//! state of the required sources. There is deliberately no public function that
//! sets the phase to `Observing` — the only way to reach it is for the world to
//! actually be in it.

#![forbid(unsafe_code)]

use observer_model::{AppPhase, PauseReason, PauseSnapshot, SourceKind, SourceReport, SourceState};

/// Intents and facts that move the observer's state. UI commands map to the
/// `Requested*` intents; the engine emits the `Source*`/`Engine*` facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppEvent {
    /// Operator asked to start observing.
    RequestedStart,
    /// Operator (or the system) asked to pause. `expires_at_epoch_secs` is an
    /// absolute auto-resume deadline the caller derives from a chosen duration
    /// against its own clock; `None` is an indefinite pause (the operator's
    /// "until I resume", or a system lock/suspend pause).
    RequestedPause {
        reason: PauseReason,
        expires_at_epoch_secs: Option<u64>,
    },
    /// Operator asked to resume.
    RequestedResume,
    /// The engine finished construction + recovery and is ready.
    EngineReady,
    /// A source reported new honest state.
    SourceUpdated(SourceReport),
    /// A required source faulted.
    SourceFaulted(SourceKind),
    /// Segment storage entered or left a persistence fault.
    StorageFaultChanged(bool),
}

/// A pause in effect: why, and an optional absolute deadline for automatic
/// resume. Reducer working memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PauseState {
    reason: PauseReason,
    expires_at_epoch_secs: Option<u64>,
}

/// The reducer's working memory. The public [`phase`](Self::phase) is always
/// recomputed from these facts — there is no settable phase field.
#[derive(Debug, Clone, Default)]
pub struct StateMachine {
    engine_ready: bool,
    run_requested: bool,
    paused: Option<PauseState>,
    storage_faulted: bool,
    screen: Option<SourceState>,
    system_audio: Option<SourceState>,
    // Mic is intentionally NOT required for `Observing`: a machine with no mic
    // (SourceState::NoInputDevice) is a fully valid observing configuration.
    mic: Option<SourceState>,
}

impl StateMachine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn engine_ready(&self) -> bool {
        self.engine_ready
    }

    /// The computed phase. Reachability of `Observing` requires the engine ready,
    /// a run requested, no active pause, and every *required* source `Active`.
    pub fn phase(&self) -> AppPhase {
        if self.paused.is_some() {
            return AppPhase::Paused;
        }
        if !self.run_requested {
            return AppPhase::Idle;
        }
        if !self.engine_ready {
            return AppPhase::Starting;
        }
        if self.storage_faulted || self.any_required_faulted() {
            return AppPhase::Error;
        }
        if self.all_required_active() {
            AppPhase::Observing
        } else {
            AppPhase::Starting
        }
    }

    /// The honest pause snapshot for the health dump, given the current clock.
    /// `None` when not paused; `seconds_remaining` is the live countdown to an
    /// automatic resume for a duration-bounded pause, `None` for an indefinite one.
    pub fn pause_snapshot(&self, now_epoch_secs: u64) -> Option<PauseSnapshot> {
        self.paused.map(|p| PauseSnapshot {
            reason: p.reason,
            seconds_remaining: p
                .expires_at_epoch_secs
                .map(|exp| exp.saturating_sub(now_epoch_secs)),
        })
    }

    /// True when a duration-bounded pause has reached its deadline and the engine
    /// should auto-resume. An indefinite pause never expires on a timer.
    pub fn pause_due_to_expire(&self, now_epoch_secs: u64) -> bool {
        self.paused
            .and_then(|p| p.expires_at_epoch_secs)
            .is_some_and(|exp| now_epoch_secs >= exp)
    }

    fn required(&self) -> [&Option<SourceState>; 2] {
        // Screen + system audio are required; mic is best-effort.
        [&self.screen, &self.system_audio]
    }

    fn any_required_faulted(&self) -> bool {
        self.required()
            .iter()
            .any(|s| matches!(s, Some(SourceState::Faulted { .. })))
    }

    fn all_required_active(&self) -> bool {
        self.required()
            .iter()
            .all(|s| matches!(s, Some(SourceState::Active)))
    }
}

/// The honest-state reduction. Folds one [`AppEvent`] into the machine and
/// returns the freshly *computed* phase. Note: no arm sets `Observing` — it can
/// only emerge from [`StateMachine::phase`] once the world warrants it.
pub fn reduce(state: &mut StateMachine, event: AppEvent) -> AppPhase {
    match event {
        AppEvent::RequestedStart => {
            state.run_requested = true;
            state.paused = None;
        }
        AppEvent::RequestedPause {
            reason,
            expires_at_epoch_secs,
        } => {
            state.paused = Some(PauseState {
                reason,
                expires_at_epoch_secs,
            });
        }
        AppEvent::RequestedResume => {
            state.paused = None;
        }
        AppEvent::EngineReady => {
            state.engine_ready = true;
        }
        AppEvent::SourceUpdated(report) => match report.kind {
            SourceKind::Screen => state.screen = Some(report.state),
            SourceKind::SystemAudio => state.system_audio = Some(report.state),
            SourceKind::Mic => state.mic = Some(report.state),
        },
        AppEvent::SourceFaulted(kind) => {
            let faulted = SourceState::Faulted {
                reason: observer_model::ErrorReason::Unknown,
                detail: "source faulted".into(),
            };
            match kind {
                SourceKind::Screen => state.screen = Some(faulted),
                SourceKind::SystemAudio => state.system_audio = Some(faulted),
                SourceKind::Mic => state.mic = Some(faulted),
            }
        }
        AppEvent::StorageFaultChanged(faulted) => {
            state.storage_faulted = faulted;
        }
    }
    state.phase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use observer_model::ErrorReason;

    fn report(kind: SourceKind, state: SourceState) -> SourceReport {
        SourceReport {
            kind,
            state,
            device: None,
        }
    }

    #[test]
    fn idle_until_start_requested() {
        let sm = StateMachine::new();
        assert_eq!(sm.phase(), AppPhase::Idle);
    }

    #[test]
    fn observing_is_computed_only_when_required_sources_active() {
        let mut sm = StateMachine::new();
        reduce(&mut sm, AppEvent::RequestedStart);
        reduce(&mut sm, AppEvent::EngineReady);
        // Still Starting: no sources active yet.
        assert_eq!(sm.phase(), AppPhase::Starting);
        reduce(
            &mut sm,
            AppEvent::SourceUpdated(report(SourceKind::Screen, SourceState::Active)),
        );
        assert_eq!(sm.phase(), AppPhase::Starting);
        let p = reduce(
            &mut sm,
            AppEvent::SourceUpdated(report(SourceKind::SystemAudio, SourceState::Active)),
        );
        assert_eq!(p, AppPhase::Observing);
    }

    #[test]
    fn no_mic_does_not_block_observing() {
        let mut sm = StateMachine::new();
        reduce(&mut sm, AppEvent::RequestedStart);
        reduce(&mut sm, AppEvent::EngineReady);
        reduce(
            &mut sm,
            AppEvent::SourceUpdated(report(SourceKind::Screen, SourceState::Active)),
        );
        reduce(
            &mut sm,
            AppEvent::SourceUpdated(report(SourceKind::SystemAudio, SourceState::Active)),
        );
        let p = reduce(
            &mut sm,
            AppEvent::SourceUpdated(report(SourceKind::Mic, SourceState::NoInputDevice)),
        );
        assert_eq!(p, AppPhase::Observing);
    }

    #[test]
    fn required_fault_drops_out_of_observing() {
        let mut sm = StateMachine::new();
        reduce(&mut sm, AppEvent::RequestedStart);
        reduce(&mut sm, AppEvent::EngineReady);
        reduce(
            &mut sm,
            AppEvent::SourceUpdated(report(SourceKind::Screen, SourceState::Active)),
        );
        reduce(
            &mut sm,
            AppEvent::SourceUpdated(report(SourceKind::SystemAudio, SourceState::Active)),
        );
        assert_eq!(sm.phase(), AppPhase::Observing);
        let p = reduce(
            &mut sm,
            AppEvent::SourceUpdated(report(
                SourceKind::SystemAudio,
                SourceState::Faulted {
                    reason: ErrorReason::EndpointLost,
                    detail: "endpoint gone".into(),
                },
            )),
        );
        assert_eq!(p, AppPhase::Error);
    }

    #[test]
    fn storage_fault_drops_out_of_observing_but_pause_still_wins() {
        let mut sm = StateMachine::new();
        reduce(&mut sm, AppEvent::RequestedStart);
        reduce(&mut sm, AppEvent::EngineReady);
        reduce(
            &mut sm,
            AppEvent::SourceUpdated(report(SourceKind::Screen, SourceState::Active)),
        );
        reduce(
            &mut sm,
            AppEvent::SourceUpdated(report(SourceKind::SystemAudio, SourceState::Active)),
        );
        assert_eq!(sm.phase(), AppPhase::Observing);

        let p = reduce(&mut sm, AppEvent::StorageFaultChanged(true));
        assert_eq!(p, AppPhase::Error);

        let p = reduce(
            &mut sm,
            AppEvent::RequestedPause {
                reason: PauseReason::Operator,
                expires_at_epoch_secs: None,
            },
        );
        assert_eq!(p, AppPhase::Paused);

        reduce(&mut sm, AppEvent::StorageFaultChanged(false));
        reduce(&mut sm, AppEvent::RequestedResume);
        assert_eq!(sm.phase(), AppPhase::Observing);
    }

    #[test]
    fn pause_overrides_even_when_sources_active() {
        let mut sm = StateMachine::new();
        reduce(&mut sm, AppEvent::RequestedStart);
        reduce(&mut sm, AppEvent::EngineReady);
        reduce(
            &mut sm,
            AppEvent::SourceUpdated(report(SourceKind::Screen, SourceState::Active)),
        );
        reduce(
            &mut sm,
            AppEvent::SourceUpdated(report(SourceKind::SystemAudio, SourceState::Active)),
        );
        let p = reduce(
            &mut sm,
            AppEvent::RequestedPause {
                reason: PauseReason::Operator,
                expires_at_epoch_secs: None,
            },
        );
        assert_eq!(p, AppPhase::Paused);
    }

    #[test]
    fn indefinite_pause_has_no_remaining_and_never_expires() {
        let mut sm = StateMachine::new();
        reduce(
            &mut sm,
            AppEvent::RequestedPause {
                reason: PauseReason::Operator,
                expires_at_epoch_secs: None,
            },
        );
        let snap = sm.pause_snapshot(1_000).expect("paused");
        assert_eq!(snap.reason, PauseReason::Operator);
        assert_eq!(snap.seconds_remaining, None);
        assert!(!sm.pause_due_to_expire(u64::MAX));
    }

    #[test]
    fn duration_pause_counts_down_then_expires() {
        let mut sm = StateMachine::new();
        // Paused at t=1000 for 900s -> deadline 1900.
        reduce(
            &mut sm,
            AppEvent::RequestedPause {
                reason: PauseReason::Operator,
                expires_at_epoch_secs: Some(1_900),
            },
        );
        assert_eq!(
            sm.pause_snapshot(1_000).unwrap().seconds_remaining,
            Some(900)
        );
        assert_eq!(
            sm.pause_snapshot(1_870).unwrap().seconds_remaining,
            Some(30)
        );
        assert!(!sm.pause_due_to_expire(1_899));
        assert!(sm.pause_due_to_expire(1_900));
        // Past the deadline, remaining saturates at 0 rather than underflowing.
        assert_eq!(sm.pause_snapshot(2_000).unwrap().seconds_remaining, Some(0));
    }

    #[test]
    fn resume_clears_pause_snapshot() {
        let mut sm = StateMachine::new();
        reduce(
            &mut sm,
            AppEvent::RequestedPause {
                reason: PauseReason::Operator,
                expires_at_epoch_secs: Some(1_900),
            },
        );
        reduce(&mut sm, AppEvent::RequestedResume);
        assert!(sm.pause_snapshot(1_000).is_none());
        assert!(!sm.pause_due_to_expire(u64::MAX));
    }
}
