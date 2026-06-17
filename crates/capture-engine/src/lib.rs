// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The capture engine — composition-tier orchestrator.
//!
//! Holds boxed `dyn ScreenSource` / `dyn SystemAudioSource` / `dyn MicSource`
//! (traits from `observer-model`), drives per-source segment writers, asks
//! `observer-segment` for rotation boundaries, folds source facts into the
//! `observer-state` reducer, feeds faults to `observer-lifecycle`, and runs
//! `observer-recovery` on construction. It depends on the **trait seams**, not
//! the platform crates — `src-tauri` injects the concrete WGC/WASAPI sources —
//! so the engine is host-testable end-to-end on Linux with fakes.
//!
//! Bootstrap state: the wiring shape and public surface are present; the live
//! capture loop is filled in by the Wave-1 capture-core work. There is no
//! `unsafe` here (all FFI is quarantined in the platform crates), hence the
//! forbid below — the engine itself stays in the safe, host-testable world.

#![forbid(unsafe_code)]

use observer_model::{MicSource, ScreenSource, SystemAudioSource};
use observer_recovery::{recover_all, RecoveryFs, RecoveryOutcome};
use observer_segment::DEFAULT_SEGMENT_SECS;
use observer_state::StateMachine;

/// The concrete platform sources injected into the engine. `capture-engine`
/// never names the platform crates; the binary constructs these and hands them
/// in, keeping the engine Tauri- and Windows-agnostic.
pub struct Sources {
    pub screen: Box<dyn ScreenSource>,
    pub system_audio: Box<dyn SystemAudioSource>,
    pub mic: Box<dyn MicSource>,
}

/// Engine configuration. `segment_secs` is the rotation period.
#[derive(Debug, Clone, Copy)]
pub struct EngineConfig {
    pub segment_secs: u64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            segment_secs: DEFAULT_SEGMENT_SECS,
        }
    }
}

/// The orchestrator. Owns the sources, the honest-state reducer, and (later) the
/// per-source writers and rotation clock.
pub struct CaptureEngine {
    sources: Sources,
    state: StateMachine,
    config: EngineConfig,
}

impl CaptureEngine {
    /// Construct the engine and run incomplete-segment recovery **before** any
    /// source starts. Returns the engine plus the recovery outcomes so the
    /// caller can surface what was finalized/quarantined.
    pub fn new<F: RecoveryFs>(
        sources: Sources,
        config: EngineConfig,
        recovery_fs: &mut F,
    ) -> Result<(Self, Vec<RecoveryOutcome>), F::Error> {
        let outcomes = recover_all(recovery_fs)?;
        let engine = Self {
            sources,
            state: StateMachine::new(),
            config,
        };
        Ok((engine, outcomes))
    }

    /// The honest-state reducer, for the shell/health layer to read.
    pub fn state(&self) -> &StateMachine {
        &self.state
    }

    /// Mutable reducer access (the run loop folds source facts here).
    pub fn state_mut(&mut self) -> &mut StateMachine {
        &mut self.state
    }

    /// The configured rotation period.
    pub fn segment_secs(&self) -> u64 {
        self.config.segment_secs
    }

    /// Start every source. The live capture/writer loop is filled in by the
    /// Wave-1 capture-core work; the skeleton just kicks the sources.
    pub fn start(&mut self) {
        let _ = self.sources.screen.start();
        let _ = self.sources.system_audio.start();
        let _ = self.sources.mic.start();
    }

    /// Stop every source.
    pub fn stop(&mut self) {
        self.sources.screen.stop();
        self.sources.system_audio.stop();
        self.sources.mic.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use observer_model::{SourceError, SourceState};

    #[derive(Default)]
    struct FakeScreen;
    impl ScreenSource for FakeScreen {
        fn start(&mut self) -> Result<(), SourceError> {
            Ok(())
        }
        fn stop(&mut self) {}
        fn state(&self) -> SourceState {
            SourceState::Active
        }
    }
    #[derive(Default)]
    struct FakeSysAudio;
    impl SystemAudioSource for FakeSysAudio {
        fn start(&mut self) -> Result<(), SourceError> {
            Ok(())
        }
        fn stop(&mut self) {}
        fn state(&self) -> SourceState {
            SourceState::Active
        }
    }
    #[derive(Default)]
    struct FakeMic;
    impl MicSource for FakeMic {
        fn start(&mut self) -> Result<(), SourceError> {
            Ok(())
        }
        fn stop(&mut self) {}
        fn state(&self) -> SourceState {
            SourceState::NoInputDevice
        }
    }

    struct NoStale;
    impl RecoveryFs for NoStale {
        type Error = ();
        fn scan_incomplete(&mut self) -> Result<Vec<observer_recovery::StaleSegment>, ()> {
            Ok(vec![])
        }
        fn finalize(&mut self, _s: &observer_recovery::StaleSegment) -> Result<(), ()> {
            Ok(())
        }
        fn quarantine(&mut self, _s: &observer_recovery::StaleSegment) -> Result<(), ()> {
            Ok(())
        }
    }

    #[test]
    fn engine_constructs_with_fakes_and_runs_recovery_first() {
        let sources = Sources {
            screen: Box::new(FakeScreen),
            system_audio: Box::new(FakeSysAudio),
            mic: Box::new(FakeMic),
        };
        let mut fs = NoStale;
        let (mut engine, outcomes) =
            CaptureEngine::new(sources, EngineConfig::default(), &mut fs).unwrap();
        assert!(outcomes.is_empty());
        assert_eq!(engine.segment_secs(), DEFAULT_SEGMENT_SECS);
        engine.start();
        engine.stop();
    }
}
