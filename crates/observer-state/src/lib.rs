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

use observer_model::{AppPhase, PauseReason, SourceKind, SourceReport, SourceState};

/// Intents and facts that move the observer's state. UI commands map to the
/// `Requested*` intents; the engine emits the `Source*`/`Engine*` facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppEvent {
    /// Operator asked to start observing.
    RequestedStart,
    /// Operator (or the system) asked to pause.
    RequestedPause(PauseReason),
    /// Operator asked to resume.
    RequestedResume,
    /// The engine finished construction + recovery and is ready.
    EngineReady,
    /// A source reported new honest state.
    SourceUpdated(SourceReport),
    /// A required source faulted.
    SourceFaulted(SourceKind),
}

/// The reducer's working memory. The public [`phase`](Self::phase) is always
/// recomputed from these facts — there is no settable phase field.
#[derive(Debug, Clone, Default)]
pub struct StateMachine {
    engine_ready: bool,
    run_requested: bool,
    paused: Option<PauseReason>,
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

    /// The computed phase. Reachability of `Observing` requires the engine ready,
    /// a run requested, no active pause, and every *required* source `Active`.
    pub fn phase(&self) -> AppPhase {
        if let Some(_reason) = self.paused {
            return AppPhase::Paused;
        }
        if !self.run_requested {
            return AppPhase::Idle;
        }
        if !self.engine_ready {
            return AppPhase::Starting;
        }
        if self.any_required_faulted() {
            return AppPhase::Error;
        }
        if self.all_required_active() {
            AppPhase::Observing
        } else {
            AppPhase::Starting
        }
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
        AppEvent::RequestedPause(reason) => {
            state.paused = Some(reason);
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
    }
    state.phase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use observer_model::ErrorReason;

    fn report(kind: SourceKind, state: SourceState) -> SourceReport {
        SourceReport { kind, state, device: None }
    }

    #[test]
    fn idle_until_start_requested() {
        let mut sm = StateMachine::new();
        assert_eq!(sm.phase(), AppPhase::Idle);
    }

    #[test]
    fn observing_is_computed_only_when_required_sources_active() {
        let mut sm = StateMachine::new();
        reduce(&mut sm, AppEvent::RequestedStart);
        reduce(&mut sm, AppEvent::EngineReady);
        // Still Starting: no sources active yet.
        assert_eq!(sm.phase(), AppPhase::Starting);
        reduce(&mut sm, AppEvent::SourceUpdated(report(SourceKind::Screen, SourceState::Active)));
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
        reduce(&mut sm, AppEvent::SourceUpdated(report(SourceKind::Screen, SourceState::Active)));
        reduce(&mut sm, AppEvent::SourceUpdated(report(SourceKind::SystemAudio, SourceState::Active)));
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
        reduce(&mut sm, AppEvent::SourceUpdated(report(SourceKind::Screen, SourceState::Active)));
        reduce(&mut sm, AppEvent::SourceUpdated(report(SourceKind::SystemAudio, SourceState::Active)));
        assert_eq!(sm.phase(), AppPhase::Observing);
        let p = reduce(
            &mut sm,
            AppEvent::SourceUpdated(report(
                SourceKind::SystemAudio,
                SourceState::Faulted { reason: ErrorReason::EndpointLost, detail: "endpoint gone".into() },
            )),
        );
        assert_eq!(p, AppPhase::Error);
    }

    #[test]
    fn pause_overrides_even_when_sources_active() {
        let mut sm = StateMachine::new();
        reduce(&mut sm, AppEvent::RequestedStart);
        reduce(&mut sm, AppEvent::EngineReady);
        reduce(&mut sm, AppEvent::SourceUpdated(report(SourceKind::Screen, SourceState::Active)));
        reduce(&mut sm, AppEvent::SourceUpdated(report(SourceKind::SystemAudio, SourceState::Active)));
        let p = reduce(&mut sm, AppEvent::RequestedPause(PauseReason::Operator));
        assert_eq!(p, AppPhase::Paused);
    }
}
