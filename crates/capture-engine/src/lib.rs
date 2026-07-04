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

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use observer_lifecycle::{BackoffConfig, Lifecycle, RetryDecision};
use observer_model::{
    AppPhase, CaptureChunk, CaptureSink, Clock, EncoderError, ErrorReason, HealthDump, MicSource,
    PauseReason, ScreenEncoder, ScreenFrame, ScreenSource, SourceError, SourceKind, SourceReport,
    SourceState, StorageHealth, SyncSnapshot, SystemAudioSource,
};
use observer_recovery::{recover_all, RecoveryFs, RecoveryOutcome};
use observer_segment::{
    seconds_until_next_boundary, segment_for, should_rotate, SegmentFs, DEFAULT_SEGMENT_SECS,
};
use observer_state::{reduce, AppEvent, StateMachine};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, watch};

pub const BREAKER_OPEN_MARKER: &str = "[breaker-open] ";

/// Commands the shell and lifecycle pump can send to the engine loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineCommand {
    /// Pause capture. `duration_secs` bounds an operator pause (15m / 30m / 1h)
    /// after which the engine auto-resumes; `None` is an indefinite pause (the
    /// operator's "until I resume", or a system lock/suspend pause).
    Pause {
        reason: PauseReason,
        duration_secs: Option<u64>,
    },
    Resume,
    /// Toggle between paused and observing — the global hotkey's action. Pauses
    /// indefinitely when observing, resumes when paused. The engine resolves the
    /// direction because it owns the authoritative phase.
    TogglePause,
    DisplayChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineExit {
    Shutdown,
    CommandChannelClosed,
    EventChannelClosed,
}

/// Real clock implementation for production.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_epoch_secs(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }
}

enum CaptureEvent {
    Audio(CaptureChunk),
    Screen(ScreenFrame),
}

struct EngineSink {
    tx: mpsc::UnboundedSender<CaptureEvent>,
}

impl CaptureSink for EngineSink {
    fn emit(&self, chunk: CaptureChunk) {
        let _ = self.tx.send(CaptureEvent::Audio(chunk));
    }

    fn emit_screen_frame(&self, frame: ScreenFrame) {
        let _ = self.tx.send(CaptureEvent::Screen(frame));
    }
}

struct OpenSegment {
    key: observer_model::SegmentKey,
    dir: String,
    opened_epoch_secs: u64,
    screen_chunks: u64,
    screen_encoder_open: bool,
}

/// The concrete platform sources injected into the engine. `capture-engine`
/// never names the platform crates; the binary constructs these and hands them
/// in, keeping the engine Tauri- and Windows-agnostic.
pub struct Sources {
    pub screen: Box<dyn ScreenSource>,
    pub screen_encoder: Box<dyn ScreenEncoder>,
    pub system_audio: Box<dyn SystemAudioSource>,
    pub mic: Box<dyn MicSource>,
}

/// Engine configuration.
#[derive(Debug, Clone, Copy)]
pub struct EngineConfig {
    pub segment_secs: u64,
    pub lifecycle: BackoffConfig,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            segment_secs: DEFAULT_SEGMENT_SECS,
            lifecycle: BackoffConfig::default(),
        }
    }
}

/// The orchestrator. Owns sources, segment fs, the reducer, and the chunk queue.
pub struct CaptureEngine<SFS: SegmentFs> {
    sources: Sources,
    state: StateMachine,
    config: EngineConfig,
    segment_fs: SFS,
    clock: Box<dyn Clock>,
    current_segment: Option<OpenSegment>,
    rx: mpsc::UnboundedReceiver<CaptureEvent>,
    sink: Arc<EngineSink>,
    source_reports: BTreeMap<SourceKind, SourceReport>,
    lifecycles: BTreeMap<SourceKind, Lifecycle>,
    retry_at_epoch_secs: BTreeMap<SourceKind, u64>,
    pending_seals: BTreeMap<observer_model::SegmentKey, Option<f64>>,
    write_stalled: bool,
    storage_fault: Option<String>,
    storage_lifecycle: Lifecycle,
    storage_retry_at: Option<u64>,
    shared_health: Arc<Mutex<HealthDump>>,
    health_tx: watch::Sender<HealthDump>,
    /// Wave-2 sync (pairing + upload) snapshot, published by the sync layer and
    /// folded into every `HealthDump`. Default = not-paired/idle, so the engine
    /// is unchanged when sync isn't running.
    sync: Arc<Mutex<SyncSnapshot>>,
}

impl<SFS> CaptureEngine<SFS>
where
    SFS: SegmentFs,
{
    /// Construct the engine and run incomplete-segment recovery **before** any
    /// source starts. The first segment opens in `start`, not construction.
    pub fn new<RFS>(
        sources: Sources,
        config: EngineConfig,
        recovery_fs: &mut RFS,
        segment_fs: SFS,
        clock: Box<dyn Clock>,
    ) -> Result<(Self, Vec<RecoveryOutcome>), RFS::Error>
    where
        RFS: RecoveryFs,
    {
        let outcomes = recover_all(recovery_fs)?;
        let (tx, rx) = mpsc::unbounded_channel();
        let (health_tx, _) = watch::channel(Self::empty_health());
        let sink = Arc::new(EngineSink { tx });
        let mut state = StateMachine::new();
        reduce(&mut state, AppEvent::EngineReady);

        let mut engine = Self {
            sources,
            state,
            config,
            segment_fs,
            clock,
            current_segment: None,
            rx,
            sink,
            source_reports: BTreeMap::new(),
            lifecycles: Self::new_lifecycles(config.lifecycle),
            retry_at_epoch_secs: BTreeMap::new(),
            pending_seals: BTreeMap::new(),
            write_stalled: false,
            storage_fault: None,
            storage_lifecycle: Lifecycle::new(Self::storage_backoff_config()),
            storage_retry_at: None,
            shared_health: Arc::new(Mutex::new(Self::empty_health())),
            health_tx,
            sync: Arc::new(Mutex::new(SyncSnapshot::default())),
        };
        engine.refresh_health();

        Ok((engine, outcomes))
    }

    /// The honest-state reducer, for the shell/health layer to read.
    pub fn state(&self) -> &StateMachine {
        &self.state
    }

    /// Mutable reducer access.
    pub fn state_mut(&mut self) -> &mut StateMachine {
        &mut self.state
    }

    /// The configured rotation period.
    pub fn segment_secs(&self) -> u64 {
        self.config.segment_secs
    }

    /// A cloneable sink tests and sources can emit chunks into.
    pub fn sink(&self) -> Arc<dyn CaptureSink> {
        self.sink.clone()
    }

    /// Shared health payload used by the loopback health server.
    pub fn health_handle(&self) -> Arc<Mutex<HealthDump>> {
        self.shared_health.clone()
    }

    /// Handle the Wave-2 sync layer publishes its pairing/upload snapshot into.
    /// The engine folds it into every `HealthDump` on the next tick, keeping one
    /// serialization of state across capture + sync.
    pub fn sync_handle(&self) -> Arc<Mutex<SyncSnapshot>> {
        self.sync.clone()
    }

    /// Subscribe to change-driven health updates.
    pub fn health_watch(&self) -> watch::Receiver<HealthDump> {
        self.health_tx.subscribe()
    }

    /// Produce the current honest health payload.
    pub fn health_dump(&self) -> HealthDump {
        let now = self.clock.now_epoch_secs();
        let app_state = self.state.phase();
        let observing = app_state == AppPhase::Observing;
        let segment_seconds_remaining =
            observing.then(|| seconds_until_next_boundary(now, self.config.segment_secs));
        let frame_rate = if observing {
            self.current_segment.as_ref().map(|segment| {
                let elapsed = now.saturating_sub(segment.opened_epoch_secs).max(1);
                (segment.screen_chunks / elapsed) as u32
            })
        } else {
            None
        };

        let sync = self
            .sync
            .lock()
            .map(|snapshot| snapshot.clone())
            .unwrap_or_default();

        HealthDump {
            app_state,
            sources: self.source_reports.values().cloned().collect(),
            frame_rate,
            segment_dir: self.current_segment.as_ref().map(|s| s.dir.clone()),
            segment_seconds_remaining,
            engine_ready: self.state.engine_ready(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            sync,
            screen_encoder: self
                .current_segment
                .is_some()
                .then(|| self.sources.screen_encoder.health()),
            exclusions: self.sources.screen.exclusion_health(),
            storage: self.storage_fault.as_ref().map(|detail| StorageHealth {
                detail: detail.clone(),
            }),
            pause: self.state.pause_snapshot(now),
            views: Default::default(),
            pump_degraded: false,
        }
    }

    /// Start every source and open the current segment. Source start failures are
    /// folded into state and do not abort the engine.
    pub fn start(&mut self) {
        self.reduce(AppEvent::RequestedStart);
        self.open_current_segment();
        for kind in [SourceKind::Screen, SourceKind::SystemAudio, SourceKind::Mic] {
            self.start_source(kind);
        }
        self.fold_source_states();
        self.refresh_health();
    }

    /// Stop every source and seal the current segment, when one is open.
    pub fn stop(&mut self) {
        self.sources.screen.stop();
        self.sources.system_audio.stop();
        self.sources.mic.stop();
        self.drain_events();

        if let Some(segment) = self.current_segment.take() {
            self.seal_segment(segment);
        }
        self.fold_source_states();
        self.refresh_health();
    }

    /// Forward display changes to the screen source. Segment rotation stays
    /// clock-driven.
    pub fn on_display_changed(&mut self) {
        self.sources.screen.on_display_changed();
    }

    /// Deterministic, host-testable unit of engine work.
    pub fn pump(&mut self) {
        self.drain_events();
        self.auto_resume_if_due();
        self.rotate_if_needed();
        self.service_storage();
        self.retry_due_sources();
        self.fold_source_states();
        self.refresh_health();
    }

    /// Production loop. Shutdown exits cleanly; source facts and health are
    /// refreshed on a one-second tick.
    pub async fn run(
        &mut self,
        mut shutdown: oneshot::Receiver<()>,
        mut command_rx: mpsc::UnboundedReceiver<EngineCommand>,
    ) -> EngineExit {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = &mut shutdown => {
                    return EngineExit::Shutdown;
                }
                cmd = command_rx.recv() => {
                    match cmd {
                        Some(command) => self.apply_command(command),
                        None => return EngineExit::CommandChannelClosed,
                    }
                }
                event = self.rx.recv() => {
                    if let Some(event) = event {
                        self.handle_event(event);
                    } else {
                        return EngineExit::EventChannelClosed;
                    }
                }
                _ = interval.tick() => {
                    self.auto_resume_if_due();
                    self.rotate_if_needed();
                    self.service_storage();
                    self.retry_due_sources();
                    self.fold_source_states();
                    self.refresh_health();
                }
            }
        }
    }

    fn reduce(&mut self, event: AppEvent) -> AppPhase {
        reduce(&mut self.state, event)
    }

    pub fn apply_command(&mut self, command: EngineCommand) {
        match command {
            EngineCommand::Pause {
                reason,
                duration_secs,
            } => {
                let now = self.clock.now_epoch_secs();
                let expires_at_epoch_secs = duration_secs.map(|d| now.saturating_add(d));
                self.reduce(AppEvent::RequestedPause {
                    reason,
                    expires_at_epoch_secs,
                });
                // A pause must actually stop capture, not merely relabel the
                // phase: stop every source so nothing is gathered while paused.
                // The open segment stays incomplete until a boundary or stop.
                self.pause_capture();
            }
            EngineCommand::Resume => {
                self.reduce(AppEvent::RequestedResume);
                self.resume_capture();
            }
            EngineCommand::TogglePause => {
                if self.state.phase() == AppPhase::Paused {
                    self.reduce(AppEvent::RequestedResume);
                    self.resume_capture();
                } else {
                    self.reduce(AppEvent::RequestedPause {
                        reason: PauseReason::Operator,
                        expires_at_epoch_secs: None,
                    });
                    self.pause_capture();
                }
            }
            EngineCommand::DisplayChanged => {
                self.on_display_changed();
            }
        }
        self.refresh_health();
    }

    /// Stop all sources so capture truly halts while paused. The open segment
    /// stays incomplete and is sealed only at a boundary or stop.
    fn pause_capture(&mut self) {
        self.sources.screen.stop();
        self.sources.system_audio.stop();
        self.sources.mic.stop();
        self.drain_events();
        self.write_stalled = false;
        self.attempt_clear_storage();
        self.fold_source_states();
    }

    /// Re-open the current segment and restart every source. The inverse of
    /// [`Self::pause_capture`], used by both an explicit resume and auto-resume.
    fn resume_capture(&mut self) {
        self.open_current_segment();
        for kind in [SourceKind::Screen, SourceKind::SystemAudio, SourceKind::Mic] {
            self.start_source(kind);
        }
        self.fold_source_states();
    }

    /// Auto-resume when a duration-bounded pause has reached its deadline.
    fn auto_resume_if_due(&mut self) {
        if self.state.pause_due_to_expire(self.clock.now_epoch_secs()) {
            self.reduce(AppEvent::RequestedResume);
            self.resume_capture();
        }
    }

    fn open_current_segment(&mut self) {
        if self.current_segment.is_some() {
            return;
        }
        let now = self.clock.now_epoch_secs();
        let key = segment_for(now, self.config.segment_secs);
        let dir = match self.segment_fs.open_incomplete(key) {
            Ok(dir) => dir,
            Err(error) => {
                self.note_storage_fault(format!("{error:?}"));
                return;
            }
        };
        self.current_segment = Some(OpenSegment {
            key,
            dir,
            opened_epoch_secs: now,
            screen_chunks: 0,
            screen_encoder_open: false,
        });
        self.attempt_clear_storage();
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            self.handle_event(event);
        }
    }

    fn handle_event(&mut self, event: CaptureEvent) {
        match event {
            CaptureEvent::Audio(chunk) => self.write_audio_chunk(chunk),
            CaptureEvent::Screen(frame) => {
                self.encode_screen_frame(frame);
            }
        }
    }

    fn write_audio_chunk(&mut self, chunk: CaptureChunk) {
        let Some(segment) = self.current_segment.as_mut() else {
            return;
        };

        match self.segment_fs.write_chunk(segment.key, &chunk) {
            Ok(()) => {
                if self.write_stalled {
                    self.write_stalled = false;
                    self.attempt_clear_storage();
                }
            }
            Err(error) => {
                self.write_stalled = true;
                self.note_storage_fault(format!("{error:?}"));
            }
        }
    }

    fn encode_screen_frame(&mut self, frame: ScreenFrame) {
        let Some((needs_open, dir)) = self.current_segment.as_mut().map(|segment| {
            segment.screen_chunks = segment.screen_chunks.saturating_add(1);
            (!segment.screen_encoder_open, segment.dir.clone())
        }) else {
            return;
        };

        if needs_open {
            // Display/resolution changes need no separate engine mechanism:
            // lazy open derives the next segment's native dimensions from its
            // first screen frame after WGC has restarted.
            if let Err(error) = self
                .sources
                .screen_encoder
                .open(&dir, frame.width, frame.height)
            {
                self.apply_encoder_error(error);
                return;
            }
            if let Some(segment) = self.current_segment.as_mut() {
                segment.screen_encoder_open = true;
            }
        }

        if let Err(error) = self.sources.screen_encoder.encode_frame(&frame) {
            self.apply_encoder_error(error);
        }
    }

    fn rotate_if_needed(&mut self) {
        let now = self.clock.now_epoch_secs();
        let Some(current) = self.current_segment.as_ref() else {
            return;
        };
        if !should_rotate(current.key, now, self.config.segment_secs) {
            return;
        }

        let old_segment = self.current_segment.take().expect("current checked above");
        self.seal_segment(old_segment);

        if self.should_be_capturing() {
            self.open_current_segment();
        }
    }

    fn finalize_screen_encoder(&mut self) -> Result<(), EncoderError> {
        match self.sources.screen_encoder.finalize() {
            Ok(()) => Ok(()),
            Err(error) => {
                self.apply_encoder_error(error.clone());
                Err(error)
            }
        }
    }

    fn video_end_for_finalized_segment(&self, segment: &OpenSegment) -> Option<f64> {
        segment
            .screen_encoder_open
            .then(|| self.sources.screen_encoder.video_end_secs())
            .flatten()
    }

    fn should_be_capturing(&self) -> bool {
        !matches!(self.state.phase(), AppPhase::Idle | AppPhase::Paused)
    }

    fn storage_healthy(&self) -> bool {
        self.pending_seals.is_empty()
            && !self.write_stalled
            && !(self.should_be_capturing() && self.current_segment.is_none())
    }

    fn note_storage_fault(&mut self, detail: String) {
        tracing::warn!(target: "storage", detail = %detail, "segment storage fault");
        self.storage_fault = Some(detail);
        self.reduce(AppEvent::StorageFaultChanged(true));
    }

    fn clear_storage_fault(&mut self) {
        self.storage_fault = None;
        self.storage_retry_at = None;
        self.storage_lifecycle.on_success();
        self.reduce(AppEvent::StorageFaultChanged(false));
    }

    fn attempt_clear_storage(&mut self) {
        if self.storage_fault.is_some() && self.storage_healthy() {
            self.clear_storage_fault();
        }
    }

    fn seal_segment(&mut self, segment: OpenSegment) {
        if self.finalize_screen_encoder().is_ok() {
            let video_end = self.video_end_for_finalized_segment(&segment);
            if let Err(error) = self.segment_fs.finalize(segment.key, video_end) {
                self.pending_seals.insert(segment.key, video_end);
                self.note_storage_fault(format!("{error:?}"));
            } else {
                self.attempt_clear_storage();
            }
        }
    }

    fn service_storage(&mut self) {
        if self.storage_fault.is_none() {
            return;
        }

        let now = self.clock.now_epoch_secs();
        let retry_due = self.storage_retry_at.is_none_or(|retry_at| now >= retry_at);
        if retry_due {
            let pending: Vec<_> = self
                .pending_seals
                .iter()
                .map(|(key, video_end)| (*key, *video_end))
                .collect();
            for (key, video_end) in pending {
                match self.segment_fs.finalize(key, video_end) {
                    Ok(()) => {
                        self.pending_seals.remove(&key);
                    }
                    Err(error) => {
                        self.note_storage_fault(format!("{error:?}"));
                    }
                }
            }

            if self.should_be_capturing() && self.current_segment.is_none() {
                self.open_current_segment();
            }
        }

        if self.storage_healthy() {
            self.clear_storage_fault();
        } else if retry_due {
            let delay_secs = match self.storage_lifecycle.on_failure() {
                RetryDecision::RetryAfter(delay) => delay.as_secs(),
                RetryDecision::GiveUp => {
                    // Storage uses u32::MAX as a practical never-open breaker; keep retrying anyway.
                    Self::storage_backoff_config().max.as_secs()
                }
            };
            self.storage_retry_at = Some(now.saturating_add(delay_secs));
        }
    }

    fn fold_source_states(&mut self) {
        let screen_state = if let Some(detail) = self.sources.screen_encoder.last_error() {
            SourceState::Faulted {
                reason: ErrorReason::WriteFailed,
                detail,
            }
        } else {
            self.sources.screen.state()
        };
        let reports = [
            SourceReport {
                kind: SourceKind::Screen,
                state: screen_state,
                device: None,
            },
            SourceReport {
                kind: SourceKind::SystemAudio,
                state: self.sources.system_audio.state(),
                device: None,
            },
            SourceReport {
                kind: SourceKind::Mic,
                state: self.sources.mic.state(),
                device: None,
            },
        ];

        for report in reports {
            self.apply_source_report(report);
        }
    }

    fn apply_source_report(&mut self, mut report: SourceReport) {
        let previous = self
            .source_reports
            .get(&report.kind)
            .map(|r| r.state.clone());
        self.handle_fault_transition(report.kind, previous.as_ref(), &mut report.state, false);
        if matches!(previous, Some(SourceState::Faulted { .. }))
            && !matches!(report.state, SourceState::Faulted { .. })
        {
            self.mark_source_success(report.kind);
        }

        self.source_reports.insert(report.kind, report.clone());
        self.reduce(AppEvent::SourceUpdated(report));
    }

    fn apply_encoder_error(&mut self, error: EncoderError) {
        self.apply_source_report(SourceReport {
            kind: SourceKind::Screen,
            state: SourceState::Faulted {
                reason: ErrorReason::WriteFailed,
                detail: error.detail,
            },
            device: None,
        });
    }

    fn apply_source_error(&mut self, kind: SourceKind, error: SourceError) {
        let mut state = SourceState::Faulted {
            reason: error.reason,
            detail: error.detail,
        };
        let previous = self.source_reports.get(&kind).map(|r| r.state.clone());
        self.handle_fault_transition(kind, previous.as_ref(), &mut state, true);
        let report = SourceReport {
            kind,
            state,
            device: None,
        };
        self.source_reports.insert(kind, report.clone());
        self.reduce(AppEvent::SourceUpdated(report));
    }

    fn handle_fault_transition(
        &mut self,
        kind: SourceKind,
        previous: Option<&SourceState>,
        current: &mut SourceState,
        force_failure: bool,
    ) {
        let SourceState::Faulted { detail, .. } = current else {
            return;
        };

        let entering_fault = !matches!(previous, Some(SourceState::Faulted { .. }));
        let should_record_failure = force_failure || entering_fault;

        if should_record_failure {
            let decision = self
                .lifecycles
                .get_mut(&kind)
                .expect("lifecycle exists for every source")
                .on_failure();
            match decision {
                RetryDecision::RetryAfter(delay) => {
                    let retry_at = self.clock.now_epoch_secs().saturating_add(delay.as_secs());
                    self.retry_at_epoch_secs.insert(kind, retry_at);
                }
                RetryDecision::GiveUp => {
                    self.retry_at_epoch_secs.remove(&kind);
                    Self::mark_breaker_open(detail);
                }
            }
        } else if self.lifecycles.get(&kind).is_some_and(|lifecycle| {
            matches!(lifecycle.breaker(), observer_lifecycle::BreakerState::Open)
        }) {
            Self::mark_breaker_open(detail);
        }
    }

    fn retry_due_sources(&mut self) {
        let now = self.clock.now_epoch_secs();
        let due: Vec<SourceKind> = self
            .retry_at_epoch_secs
            .iter()
            .filter_map(|(kind, retry_at)| (*retry_at <= now).then_some(*kind))
            .collect();

        for kind in due {
            self.retry_at_epoch_secs.remove(&kind);
            self.stop_source(kind);
            self.start_source(kind);
        }
    }

    fn refresh_health(&mut self) {
        let mut dump = self.health_dump();
        if let Ok(mut shared) = self.shared_health.lock() {
            // `views` is app-owned (the frontend writes it via the beacon command).
            // The engine rebuilds the dump from scratch each tick, so carry the
            // earned beacon forward — never clobber it back to empty.
            dump.views = shared.views.clone();
            // `pump_degraded` is app-owned like `views`; carry it forward too.
            dump.pump_degraded = shared.pump_degraded;
            *shared = dump.clone();
        }
        self.health_tx.send_replace(dump);
    }

    fn start_source(&mut self, kind: SourceKind) {
        let sink = self.sink();
        let result = match kind {
            SourceKind::Screen => self.sources.screen.start(sink),
            SourceKind::SystemAudio => self.sources.system_audio.start(sink),
            SourceKind::Mic => self.sources.mic.start(sink),
        };

        match result {
            Ok(()) => self.mark_source_success(kind),
            Err(error) => self.apply_source_error(kind, error),
        }
    }

    fn stop_source(&mut self, kind: SourceKind) {
        match kind {
            SourceKind::Screen => self.sources.screen.stop(),
            SourceKind::SystemAudio => self.sources.system_audio.stop(),
            SourceKind::Mic => self.sources.mic.stop(),
        }
    }

    fn mark_source_success(&mut self, kind: SourceKind) {
        self.retry_at_epoch_secs.remove(&kind);
        self.lifecycles
            .get_mut(&kind)
            .expect("lifecycle exists for every source")
            .on_success();
    }

    fn mark_breaker_open(detail: &mut String) {
        if !detail.starts_with(BREAKER_OPEN_MARKER) {
            *detail = format!("{BREAKER_OPEN_MARKER}{detail}");
        }
    }

    fn new_lifecycles(config: BackoffConfig) -> BTreeMap<SourceKind, Lifecycle> {
        BTreeMap::from([
            (SourceKind::Screen, Lifecycle::new(config)),
            (SourceKind::SystemAudio, Lifecycle::new(config)),
            (SourceKind::Mic, Lifecycle::new(config)),
        ])
    }

    fn storage_backoff_config() -> BackoffConfig {
        BackoffConfig {
            breaker_threshold: u32::MAX,
            ..BackoffConfig::default()
        }
    }

    fn empty_health() -> HealthDump {
        HealthDump {
            app_state: AppPhase::Idle,
            sources: Vec::new(),
            frame_rate: None,
            segment_dir: None,
            segment_seconds_remaining: None,
            engine_ready: false,
            version: env!("CARGO_PKG_VERSION").to_string(),
            sync: SyncSnapshot::default(),
            screen_encoder: None,
            exclusions: None,
            storage: None,
            pause: None,
            views: Default::default(),
            pump_degraded: false,
        }
    }
}

/// Minimal loopback-only HTTP health responder. The caller owns binding and must
/// bind to 127.0.0.1, never a wildcard address.
pub async fn serve_health(listener: TcpListener, dump: Arc<Mutex<HealthDump>>) -> io::Result<()> {
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                tracing::error!(target: "health", error = %error, "health server accept failed");
                return Err(error);
            }
        };
        if let Err(error) = handle_health_connection(stream, &dump).await {
            tracing::warn!(target: "health", error = %error, "health connection failed");
        }
    }
}

async fn handle_health_connection(
    mut stream: TcpStream,
    dump: &Arc<Mutex<HealthDump>>,
) -> io::Result<()> {
    let mut request = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        request.extend_from_slice(&buf[..n]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }

    let body = {
        let locked = dump
            .lock()
            .map_err(|_| io::Error::other("health dump mutex poisoned"))?;
        observer_health::to_pretty_json(&locked).map_err(io::Error::other)?
    };
    let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeSet, VecDeque};
    use std::sync::atomic::{AtomicU64, Ordering};

    use observer_model::{
        EncoderErrorKind, EncoderHealth, ErrorReason, ScreenPixelFormat, SegmentKey,
        ViewRenderState,
    };
    use observer_recovery::StaleSegment;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    #[derive(Clone)]
    struct FakeClock {
        now: Arc<AtomicU64>,
    }

    impl FakeClock {
        fn new(now: u64) -> Self {
            Self {
                now: Arc::new(AtomicU64::new(now)),
            }
        }

        fn set(&self, now: u64) {
            self.now.store(now, Ordering::Relaxed);
        }
    }

    impl Clock for FakeClock {
        fn now_epoch_secs(&self) -> u64 {
            self.now.load(Ordering::Relaxed)
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum FsEvent {
        Open(SegmentKey),
        Write(SegmentKey, SourceKind, u64),
        Finalize(SegmentKey),
        EncoderOpen(u32, u32),
        EncoderEncode(u64),
        EncoderFinalize,
    }

    #[derive(Default)]
    struct FakeSegmentState {
        writes: Vec<(SegmentKey, SourceKind, u64)>,
        video_ends: Vec<Option<f64>>,
        fail_open: bool,
        write_failures_remaining: usize,
        finalize_failures_remaining: usize,
    }

    #[derive(Clone, Default)]
    struct FakeSegmentFs {
        state: Arc<Mutex<FakeSegmentState>>,
        events: Arc<Mutex<Vec<FsEvent>>>,
    }

    impl FakeSegmentFs {
        fn events(&self) -> Vec<FsEvent> {
            self.events.lock().unwrap().clone()
        }

        fn writes(&self) -> Vec<(SegmentKey, SourceKind, u64)> {
            self.state.lock().unwrap().writes.clone()
        }

        fn video_ends(&self) -> Vec<Option<f64>> {
            self.state.lock().unwrap().video_ends.clone()
        }

        fn event_log(&self) -> Arc<Mutex<Vec<FsEvent>>> {
            self.events.clone()
        }

        fn fail_next_writes(&self, count: usize) {
            self.state.lock().unwrap().write_failures_remaining = count;
        }

        fn fail_next_finalizes(&self, count: usize) {
            self.state.lock().unwrap().finalize_failures_remaining = count;
        }
    }

    impl SegmentFs for FakeSegmentFs {
        type Error = &'static str;

        fn open_incomplete(&mut self, key: SegmentKey) -> Result<String, Self::Error> {
            let state = self.state.lock().unwrap();
            if state.fail_open {
                return Err("open failed");
            }
            self.events.lock().unwrap().push(FsEvent::Open(key));
            Ok(format!("/segments/{}.incomplete", key.index))
        }

        fn write_chunk(
            &mut self,
            key: SegmentKey,
            chunk: &CaptureChunk,
        ) -> Result<(), Self::Error> {
            let mut state = self.state.lock().unwrap();
            if state.write_failures_remaining > 0 {
                state.write_failures_remaining -= 1;
                return Err("write failed");
            }
            self.events
                .lock()
                .unwrap()
                .push(FsEvent::Write(key, chunk.source, chunk.seq));
            state.writes.push((key, chunk.source, chunk.seq));
            Ok(())
        }

        fn finalize(
            &mut self,
            key: SegmentKey,
            video_end_secs: Option<f64>,
        ) -> Result<(), Self::Error> {
            let mut state = self.state.lock().unwrap();
            if state.finalize_failures_remaining > 0 {
                state.finalize_failures_remaining -= 1;
                return Err("finalize failed");
            }
            state.video_ends.push(video_end_secs);
            self.events.lock().unwrap().push(FsEvent::Finalize(key));
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeRecoveryFs {
        scans: usize,
        finalized: Vec<SegmentKey>,
        quarantined: Vec<SegmentKey>,
        stale: Vec<StaleSegment>,
    }

    impl RecoveryFs for FakeRecoveryFs {
        type Error = ();

        fn scan_incomplete(&mut self) -> Result<Vec<StaleSegment>, ()> {
            self.scans += 1;
            Ok(self.stale.clone())
        }

        fn finalize(&mut self, seg: &StaleSegment) -> Result<(), ()> {
            self.finalized.push(seg.key);
            Ok(())
        }

        fn quarantine(&mut self, seg: &StaleSegment) -> Result<(), ()> {
            self.quarantined.push(seg.key);
            Ok(())
        }
    }

    #[derive(Clone)]
    struct FakeSourceHandle {
        inner: Arc<Mutex<FakeSourceState>>,
    }

    struct FakeSourceState {
        state: SourceState,
        starts: usize,
        stops: usize,
        display_changes: usize,
        start_errors: VecDeque<SourceError>,
    }

    impl FakeSourceHandle {
        fn new(state: SourceState) -> Self {
            Self {
                inner: Arc::new(Mutex::new(FakeSourceState {
                    state,
                    starts: 0,
                    stops: 0,
                    display_changes: 0,
                    start_errors: VecDeque::new(),
                })),
            }
        }

        fn state(&self) -> SourceState {
            self.inner.lock().unwrap().state.clone()
        }

        fn starts(&self) -> usize {
            self.inner.lock().unwrap().starts
        }

        fn stops(&self) -> usize {
            self.inner.lock().unwrap().stops
        }

        fn display_changes(&self) -> usize {
            self.inner.lock().unwrap().display_changes
        }

        fn start(&self) -> Result<(), SourceError> {
            let mut inner = self.inner.lock().unwrap();
            inner.starts += 1;
            if let Some(error) = inner.start_errors.pop_front() {
                Err(error)
            } else {
                Ok(())
            }
        }

        fn stop(&self) {
            self.inner.lock().unwrap().stops += 1;
        }

        fn on_display_changed(&self) {
            self.inner.lock().unwrap().display_changes += 1;
        }
    }

    struct FakeScreen {
        handle: FakeSourceHandle,
    }

    impl ScreenSource for FakeScreen {
        fn start(&mut self, _sink: Arc<dyn CaptureSink>) -> Result<(), SourceError> {
            self.handle.start()
        }

        fn stop(&mut self) {
            self.handle.stop();
        }

        fn state(&self) -> SourceState {
            self.handle.state()
        }

        fn on_display_changed(&mut self) {
            self.handle.on_display_changed();
        }
    }

    struct FakeSystemAudio {
        handle: FakeSourceHandle,
    }

    impl SystemAudioSource for FakeSystemAudio {
        fn start(&mut self, _sink: Arc<dyn CaptureSink>) -> Result<(), SourceError> {
            self.handle.start()
        }

        fn stop(&mut self) {
            self.handle.stop();
        }

        fn state(&self) -> SourceState {
            self.handle.state()
        }
    }

    struct FakeMic {
        handle: FakeSourceHandle,
    }

    impl MicSource for FakeMic {
        fn start(&mut self, _sink: Arc<dyn CaptureSink>) -> Result<(), SourceError> {
            self.handle.start()
        }

        fn stop(&mut self) {
            self.handle.stop();
        }

        fn state(&self) -> SourceState {
            self.handle.state()
        }
    }

    #[derive(Clone, Default)]
    struct FakeScreenEncoder {
        inner: Arc<Mutex<FakeScreenEncoderState>>,
        events: Option<Arc<Mutex<Vec<FsEvent>>>>,
    }

    #[derive(Default)]
    struct FakeScreenEncoderState {
        opened: Option<(String, u32, u32)>,
        frames_consumed: u64,
        samples_written: u64,
        video_end_secs: Option<f64>,
        last_error: Option<String>,
        open_errors: VecDeque<EncoderError>,
        encode_errors: VecDeque<EncoderError>,
        finalize_errors: VecDeque<EncoderError>,
    }

    impl FakeScreenEncoder {
        fn with_events(events: Arc<Mutex<Vec<FsEvent>>>) -> Self {
            Self {
                inner: Arc::new(Mutex::new(FakeScreenEncoderState::default())),
                events: Some(events),
            }
        }

        fn push_encode_error(&self, error: EncoderError) {
            self.inner.lock().unwrap().encode_errors.push_back(error);
        }

        fn push_finalize_error(&self, error: EncoderError) {
            self.inner.lock().unwrap().finalize_errors.push_back(error);
        }

        fn health_snapshot(&self) -> EncoderHealth {
            self.inner.lock().unwrap().health()
        }

        fn set_video_end_secs(&self, video_end_secs: Option<f64>) {
            self.inner.lock().unwrap().video_end_secs = video_end_secs;
        }

        fn open_count(&self) -> usize {
            self.events
                .as_ref()
                .map(|events| {
                    events
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|event| matches!(event, FsEvent::EncoderOpen(_, _)))
                        .count()
                })
                .unwrap_or(0)
        }
    }

    impl FakeScreenEncoderState {
        fn health(&self) -> EncoderHealth {
            EncoderHealth {
                frames_consumed: self.frames_consumed,
                frames_dropped: 0,
                samples_written: self.samples_written,
                clamp_events: 0,
                last_error: self.last_error.clone(),
            }
        }
    }

    impl ScreenEncoder for FakeScreenEncoder {
        fn open(&mut self, dir: &str, width: u32, height: u32) -> Result<(), EncoderError> {
            let mut inner = self.inner.lock().unwrap();
            if let Some(error) = inner.open_errors.pop_front() {
                inner.last_error = Some(error.detail.clone());
                return Err(error);
            }
            inner.opened = Some((dir.to_string(), width, height));
            inner.last_error = None;
            if let Some(events) = &self.events {
                events
                    .lock()
                    .unwrap()
                    .push(FsEvent::EncoderOpen(width, height));
            }
            Ok(())
        }

        fn encode_frame(&mut self, frame: &ScreenFrame) -> Result<(), EncoderError> {
            let mut inner = self.inner.lock().unwrap();
            inner.frames_consumed = inner.frames_consumed.saturating_add(1);
            let Some((_, width, height)) = inner.opened.as_ref() else {
                let error = EncoderError::new(EncoderErrorKind::EncodeFailed, "encoder not open");
                inner.last_error = Some(error.detail.clone());
                return Err(error);
            };
            if *width != frame.width || *height != frame.height {
                let error = EncoderError::new(
                    EncoderErrorKind::InvalidFrameDimensions,
                    format!(
                        "frame dimensions {}x{} do not match opened {}x{}",
                        frame.width, frame.height, width, height
                    ),
                );
                inner.last_error = Some(error.detail.clone());
                return Err(error);
            }
            if let Some(error) = inner.encode_errors.pop_front() {
                inner.last_error = Some(error.detail.clone());
                return Err(error);
            }
            inner.samples_written = inner.samples_written.saturating_add(1);
            if let Some(events) = &self.events {
                events
                    .lock()
                    .unwrap()
                    .push(FsEvent::EncoderEncode(frame.seq));
            }
            Ok(())
        }

        fn finalize(&mut self) -> Result<(), EncoderError> {
            if let Some(events) = &self.events {
                events.lock().unwrap().push(FsEvent::EncoderFinalize);
            }
            let mut inner = self.inner.lock().unwrap();
            inner.opened = None;
            if let Some(error) = inner.finalize_errors.pop_front() {
                inner.last_error = Some(error.detail.clone());
                return Err(error);
            }
            Ok(())
        }

        fn frames_consumed(&self) -> u64 {
            self.inner.lock().unwrap().frames_consumed
        }

        fn samples_written(&self) -> u64 {
            self.inner.lock().unwrap().samples_written
        }

        fn last_error(&self) -> Option<String> {
            self.inner.lock().unwrap().last_error.clone()
        }

        fn health(&self) -> EncoderHealth {
            self.inner.lock().unwrap().health()
        }

        fn video_end_secs(&self) -> Option<f64> {
            self.inner.lock().unwrap().video_end_secs
        }
    }

    struct Handles {
        screen: FakeSourceHandle,
        screen_encoder: FakeScreenEncoder,
        system_audio: FakeSourceHandle,
        mic: FakeSourceHandle,
    }

    fn fake_sources(
        screen_state: SourceState,
        system_audio_state: SourceState,
        mic_state: SourceState,
    ) -> (Sources, Handles) {
        let handles = Handles {
            screen: FakeSourceHandle::new(screen_state),
            screen_encoder: FakeScreenEncoder::default(),
            system_audio: FakeSourceHandle::new(system_audio_state),
            mic: FakeSourceHandle::new(mic_state),
        };
        let sources = Sources {
            screen: Box::new(FakeScreen {
                handle: handles.screen.clone(),
            }),
            screen_encoder: Box::new(handles.screen_encoder.clone()),
            system_audio: Box::new(FakeSystemAudio {
                handle: handles.system_audio.clone(),
            }),
            mic: Box::new(FakeMic {
                handle: handles.mic.clone(),
            }),
        };
        (sources, handles)
    }

    fn active_sources_with_encoder(screen_encoder: FakeScreenEncoder) -> (Sources, Handles) {
        let (mut sources, mut handles) = active_sources();
        sources.screen_encoder = Box::new(screen_encoder.clone());
        handles.screen_encoder = screen_encoder;
        (sources, handles)
    }

    fn active_sources() -> (Sources, Handles) {
        fake_sources(
            SourceState::Active,
            SourceState::Active,
            SourceState::NoInputDevice,
        )
    }

    fn engine_with(
        clock: FakeClock,
        segment_fs: FakeSegmentFs,
        config: EngineConfig,
        sources: Sources,
    ) -> CaptureEngine<FakeSegmentFs> {
        let mut recovery = FakeRecoveryFs::default();
        CaptureEngine::new(sources, config, &mut recovery, segment_fs, Box::new(clock))
            .unwrap()
            .0
    }

    fn emit_screen(sink: &Arc<dyn CaptureSink>, seqs: impl Iterator<Item = u64>) {
        for seq in seqs {
            sink.emit_screen_frame(ScreenFrame {
                seq,
                arrival_100ns: 0,
                width: 2,
                height: 2,
                pixel_format: ScreenPixelFormat::Rgba8,
                pixels: Arc::from(vec![seq as u8; 16]),
            });
        }
    }

    fn emit_screen_size(sink: &Arc<dyn CaptureSink>, seq: u64, width: u32, height: u32) {
        let len = width as usize * height as usize * 4;
        sink.emit_screen_frame(ScreenFrame {
            seq,
            arrival_100ns: 0,
            width,
            height,
            pixel_format: ScreenPixelFormat::Rgba8,
            pixels: Arc::from(vec![seq as u8; len]),
        });
    }

    fn emit_audio(
        sink: &Arc<dyn CaptureSink>,
        source: SourceKind,
        seqs: impl Iterator<Item = u64>,
    ) {
        for seq in seqs {
            sink.emit(CaptureChunk {
                source,
                seq,
                data: vec![seq as u8],
                format: None,
            });
        }
    }

    #[test]
    fn engine_constructs_with_fakes_and_runs_recovery_first() {
        let (sources, handles) = active_sources();
        let segment_fs = FakeSegmentFs::default();
        let clock = FakeClock::new(0);
        let mut recovery = FakeRecoveryFs::default();

        let (mut engine, outcomes) = CaptureEngine::new(
            sources,
            EngineConfig::default(),
            &mut recovery,
            segment_fs,
            Box::new(clock),
        )
        .unwrap();

        assert!(outcomes.is_empty());
        assert_eq!(recovery.scans, 1);
        assert_eq!(handles.screen.starts(), 0);
        assert_eq!(engine.segment_secs(), DEFAULT_SEGMENT_SECS);

        engine.start();
        assert_eq!(handles.screen.starts(), 1);
        assert_eq!(handles.system_audio.starts(), 1);
        assert_eq!(handles.mic.starts(), 1);
        engine.stop();
    }

    #[test]
    fn apply_command_folds_refresh_health() {
        let (sources, handles) = active_sources();
        let mut engine = engine_with(
            FakeClock::new(0),
            FakeSegmentFs::default(),
            EngineConfig::default(),
            sources,
        );
        let mut rx = engine.health_watch();

        engine.start();

        rx.mark_unchanged();
        engine.apply_command(EngineCommand::Pause {
            reason: PauseReason::Operator,
            duration_secs: None,
        });
        assert!(rx.has_changed().unwrap());
        assert_eq!(engine.health_dump().app_state, AppPhase::Paused);

        rx.mark_unchanged();
        engine.apply_command(EngineCommand::Resume);
        assert!(rx.has_changed().unwrap());
        assert_eq!(engine.health_dump().app_state, AppPhase::Observing);

        let display_changes = handles.screen.display_changes();
        rx.mark_unchanged();
        engine.apply_command(EngineCommand::DisplayChanged);
        assert!(rx.has_changed().unwrap());
        assert_eq!(handles.screen.display_changes(), display_changes + 1);
        assert_eq!(engine.health_dump().app_state, AppPhase::Observing);
    }

    #[test]
    fn pause_stops_capture_seals_at_boundary_and_auto_resumes_at_deadline() {
        let clock = FakeClock::new(1_000);
        let segment_fs = FakeSegmentFs::default();
        let segment_view = segment_fs.clone();
        let (sources, handles) = active_sources();
        let mut engine = engine_with(clock.clone(), segment_fs, EngineConfig::default(), sources);
        engine.start();
        assert!(
            engine.health_dump().segment_dir.is_some(),
            "a segment is open while observing"
        );
        let stops_before = handles.screen.stops();

        // Pause for 900s at t=1000 -> auto-resume deadline 1900.
        engine.apply_command(EngineCommand::Pause {
            reason: PauseReason::Operator,
            duration_secs: Some(900),
        });

        // A real pause: sources stopped, phase Paused, and the open segment
        // remains incomplete until its clock boundary.
        assert_eq!(engine.health_dump().app_state, AppPhase::Paused);
        assert!(
            handles.screen.stops() > stops_before,
            "screen source stopped"
        );
        assert!(
            engine.health_dump().segment_dir.is_some(),
            "the open segment stays unsealed while paused"
        );
        assert!(
            segment_view
                .events()
                .iter()
                .all(|e| !matches!(e, FsEvent::Finalize(_))),
            "pause must not finalize the segment"
        );
        assert_eq!(
            engine.health_dump().pause.unwrap().seconds_remaining,
            Some(900)
        );

        // Before the boundary and deadline, pumping keeps it paused and unsealed.
        clock.set(1_199);
        engine.pump();
        assert_eq!(engine.health_dump().app_state, AppPhase::Paused);
        assert_eq!(
            engine.health_dump().pause.unwrap().seconds_remaining,
            Some(701)
        );
        assert!(
            segment_view
                .events()
                .iter()
                .all(|e| !matches!(e, FsEvent::Finalize(_))),
            "pre-boundary pump must not finalize"
        );

        // At the boundary, the paused segment seals once and no successor opens.
        clock.set(1_200);
        engine.pump();
        assert_eq!(engine.health_dump().app_state, AppPhase::Paused);
        assert!(
            segment_view
                .events()
                .iter()
                .any(|e| matches!(e, FsEvent::Finalize(_))),
            "the segment is finalized on the first boundary tick"
        );
        assert!(engine.health_dump().segment_dir.is_none());

        // At the deadline, the engine auto-resumes and re-opens a segment.
        clock.set(1_900);
        engine.pump();
        assert_eq!(engine.health_dump().app_state, AppPhase::Observing);
        assert!(engine.health_dump().pause.is_none());
        assert!(
            engine.health_dump().segment_dir.is_some(),
            "a fresh segment is open after auto-resume"
        );
    }

    #[test]
    fn toggle_pause_flips_between_observing_and_paused() {
        let (sources, _) = active_sources();
        let mut engine = engine_with(
            FakeClock::new(0),
            FakeSegmentFs::default(),
            EngineConfig::default(),
            sources,
        );
        engine.start();
        assert_eq!(engine.health_dump().app_state, AppPhase::Observing);

        // Hotkey once -> indefinite pause.
        engine.apply_command(EngineCommand::TogglePause);
        assert_eq!(engine.health_dump().app_state, AppPhase::Paused);
        assert_eq!(engine.health_dump().pause.unwrap().seconds_remaining, None);

        // Hotkey again -> resume.
        engine.apply_command(EngineCommand::TogglePause);
        assert_eq!(engine.health_dump().app_state, AppPhase::Observing);
        assert!(engine.health_dump().pause.is_none());
    }

    #[test]
    fn indefinite_pause_never_auto_resumes() {
        let clock = FakeClock::new(1_000);
        let (sources, _) = active_sources();
        let mut engine = engine_with(
            clock.clone(),
            FakeSegmentFs::default(),
            EngineConfig::default(),
            sources,
        );
        engine.start();
        engine.apply_command(EngineCommand::Pause {
            reason: PauseReason::Operator,
            duration_secs: None,
        });
        assert_eq!(engine.health_dump().pause.unwrap().seconds_remaining, None);
        clock.set(u64::MAX / 2);
        engine.pump();
        assert_eq!(engine.health_dump().app_state, AppPhase::Paused);
    }

    #[test]
    fn refresh_health_carries_app_owned_views_forward() {
        let (sources, _) = active_sources();
        let mut engine = engine_with(
            FakeClock::new(0),
            FakeSegmentFs::default(),
            EngineConfig::default(),
            sources,
        );
        // Seed the app-owned views into the shared dump (as the beacon command would).
        let handle = engine.health_handle();
        handle
            .lock()
            .unwrap()
            .views
            .insert("settings".to_string(), ViewRenderState::Rendered);

        // A wholesale refresh rebuilds the dump from scratch (empty views) ...
        assert!(engine.health_dump().views.is_empty());
        engine.refresh_health();

        // ... but must preserve the earned beacon on both the shared handle and the
        // watch channel.
        assert_eq!(
            handle.lock().unwrap().views.get("settings"),
            Some(&ViewRenderState::Rendered)
        );
        assert_eq!(
            engine.health_watch().borrow().views.get("settings"),
            Some(&ViewRenderState::Rendered)
        );
    }

    #[test]
    fn refresh_health_carries_pump_degraded_forward() {
        let (sources, _) = active_sources();
        let mut engine = engine_with(
            FakeClock::new(0),
            FakeSegmentFs::default(),
            EngineConfig::default(),
            sources,
        );
        let handle = engine.health_handle();
        handle.lock().unwrap().pump_degraded = true;

        assert!(!engine.health_dump().pump_degraded);
        engine.refresh_health();

        assert!(handle.lock().unwrap().pump_degraded);
        assert!(engine.health_watch().borrow().pump_degraded);
    }

    #[test]
    fn rotation_preserves_every_audio_chunk_once_and_splits_segments() {
        let clock = FakeClock::new(299);
        let segment_fs = FakeSegmentFs::default();
        let segment_view = segment_fs.clone();
        let (sources, _) = active_sources();
        let mut engine = engine_with(clock.clone(), segment_fs, EngineConfig::default(), sources);
        engine.start();
        let sink = engine.sink();

        emit_audio(&sink, SourceKind::SystemAudio, 0..5);
        engine.pump();

        clock.set(300);
        emit_audio(&sink, SourceKind::SystemAudio, 5..8);
        engine.pump();

        emit_audio(&sink, SourceKind::SystemAudio, 8..11);
        engine.pump();

        let writes = segment_view.writes();
        let seqs: BTreeSet<u64> = writes.iter().map(|(_, _, seq)| *seq).collect();
        assert_eq!(seqs, (0..11).collect());
        assert_eq!(writes.len(), 11);

        let keys: BTreeSet<SegmentKey> = writes.iter().map(|(key, _, _)| *key).collect();
        assert!(keys.len() >= 2, "writes did not span multiple segment keys");

        let old = segment_for(299, DEFAULT_SEGMENT_SECS);
        let new = segment_for(300, DEFAULT_SEGMENT_SECS);
        let events = segment_view.events();
        let finalize_old = events
            .iter()
            .position(|event| *event == FsEvent::Finalize(old))
            .unwrap();
        let open_new = events
            .iter()
            .position(|event| *event == FsEvent::Open(new))
            .unwrap();
        assert!(finalize_old < open_new);
    }

    #[test]
    fn rotation_finalizes_encoder_before_sealing_segment() {
        let clock = FakeClock::new(299);
        let segment_fs = FakeSegmentFs::default();
        let segment_view = segment_fs.clone();
        let encoder = FakeScreenEncoder::with_events(segment_view.event_log());
        let (sources, _) = active_sources_with_encoder(encoder);
        let mut engine = engine_with(clock.clone(), segment_fs, EngineConfig::default(), sources);
        engine.start();

        emit_screen(&engine.sink(), 0..1);
        engine.pump();

        clock.set(300);
        engine.pump();

        let old = segment_for(299, DEFAULT_SEGMENT_SECS);
        let events = segment_view.events();
        let encoder_finalize = events
            .iter()
            .position(|event| *event == FsEvent::EncoderFinalize)
            .unwrap();
        let segment_finalize = events
            .iter()
            .position(|event| *event == FsEvent::Finalize(old))
            .unwrap();
        assert!(encoder_finalize < segment_finalize);
    }

    #[test]
    fn stop_threads_video_end_hint_to_segment_finalize() {
        let segment_fs = FakeSegmentFs::default();
        let segment_view = segment_fs.clone();
        let encoder = FakeScreenEncoder::default();
        encoder.set_video_end_secs(Some(123.0));
        let (sources, _) = active_sources_with_encoder(encoder);
        let mut engine = engine_with(
            FakeClock::new(0),
            segment_fs,
            EngineConfig::default(),
            sources,
        );

        engine.start();
        emit_screen(&engine.sink(), 0..1);
        engine.pump();
        engine.stop();

        assert_eq!(segment_view.video_ends(), vec![Some(123.0)]);
    }

    #[test]
    fn stop_does_not_reuse_stale_video_end_when_segment_had_no_screen_frames() {
        let segment_fs = FakeSegmentFs::default();
        let segment_view = segment_fs.clone();
        let encoder = FakeScreenEncoder::default();
        encoder.set_video_end_secs(Some(123.0));
        let (sources, _) = active_sources_with_encoder(encoder);
        let mut engine = engine_with(
            FakeClock::new(0),
            segment_fs,
            EngineConfig::default(),
            sources,
        );

        engine.start();
        engine.stop();

        assert_eq!(segment_view.video_ends(), vec![None]);
    }

    #[test]
    fn fault_provenance_is_preserved_in_health() {
        let (sources, _) = fake_sources(
            SourceState::Faulted {
                reason: ErrorReason::EndpointLost,
                detail: "x".into(),
            },
            SourceState::Active,
            SourceState::NoInputDevice,
        );
        let mut engine = engine_with(
            FakeClock::new(0),
            FakeSegmentFs::default(),
            EngineConfig::default(),
            sources,
        );

        engine.start();
        engine.pump();

        let dump = engine.health_dump();
        let screen = dump
            .sources
            .iter()
            .find(|source| source.kind == SourceKind::Screen)
            .unwrap();
        assert_eq!(
            screen.state,
            SourceState::Faulted {
                reason: ErrorReason::EndpointLost,
                detail: "x".into()
            }
        );
        assert_eq!(dump.app_state, AppPhase::Error);
    }

    #[test]
    fn breaker_visibility_marks_give_up_but_not_transient_faults() {
        let config = EngineConfig {
            lifecycle: BackoffConfig {
                breaker_threshold: 1,
                ..BackoffConfig::default()
            },
            ..EngineConfig::default()
        };
        let (sources, _) = fake_sources(
            SourceState::Faulted {
                reason: ErrorReason::EndpointLost,
                detail: "gone".into(),
            },
            SourceState::Active,
            SourceState::NoInputDevice,
        );
        let mut engine = engine_with(FakeClock::new(0), FakeSegmentFs::default(), config, sources);
        engine.start();

        let screen = engine
            .health_dump()
            .sources
            .into_iter()
            .find(|source| source.kind == SourceKind::Screen)
            .unwrap();
        match screen.state {
            SourceState::Faulted { detail, .. } => {
                assert!(detail.contains(BREAKER_OPEN_MARKER));
            }
            other => panic!("expected faulted state, got {other:?}"),
        }

        let config = EngineConfig {
            lifecycle: BackoffConfig {
                breaker_threshold: 2,
                ..BackoffConfig::default()
            },
            ..EngineConfig::default()
        };
        let (sources, _) = fake_sources(
            SourceState::Faulted {
                reason: ErrorReason::EndpointLost,
                detail: "gone".into(),
            },
            SourceState::Active,
            SourceState::NoInputDevice,
        );
        let mut engine = engine_with(FakeClock::new(0), FakeSegmentFs::default(), config, sources);
        engine.start();

        let screen = engine
            .health_dump()
            .sources
            .into_iter()
            .find(|source| source.kind == SourceKind::Screen)
            .unwrap();
        match screen.state {
            SourceState::Faulted { detail, .. } => {
                assert!(!detail.contains(BREAKER_OPEN_MARKER));
            }
            other => panic!("expected faulted state, got {other:?}"),
        }
    }

    #[test]
    fn no_mic_does_not_block_observing() {
        let (sources, _) = active_sources();
        let mut engine = engine_with(
            FakeClock::new(0),
            FakeSegmentFs::default(),
            EngineConfig::default(),
            sources,
        );

        engine.start();
        engine.pump();

        assert_eq!(engine.health_dump().app_state, AppPhase::Observing);
    }

    #[test]
    fn storage_finalize_failures_retry_past_source_breaker_threshold_and_recover() {
        let config = EngineConfig {
            segment_secs: 10_000,
            ..EngineConfig::default()
        };
        let clock = FakeClock::new(9_999);
        let segment_fs = FakeSegmentFs::default();
        let segment_view = segment_fs.clone();
        segment_fs.fail_next_finalizes(10);
        let (sources, _) = active_sources();
        let mut engine = engine_with(clock.clone(), segment_fs, config, sources);

        engine.start();
        clock.set(10_000);
        engine.pump();

        assert!(engine.health_dump().storage.is_some());
        assert_ne!(engine.health_dump().app_state, AppPhase::Observing);
        assert_eq!(engine.pending_seals.len(), 1);

        for _ in 0..10 {
            if engine.pending_seals.is_empty() {
                break;
            }
            let retry_at = engine.storage_retry_at.unwrap();
            clock.set(retry_at.saturating_add(60));
            engine.pump();
        }

        assert!(engine.pending_seals.is_empty());
        assert!(engine.health_dump().storage.is_none());
        assert_eq!(engine.health_dump().app_state, AppPhase::Observing);

        clock.set(20_000);
        engine.pump();
        let later = segment_for(10_000, config.segment_secs);
        assert!(segment_view.events().contains(&FsEvent::Finalize(later)));
    }

    #[test]
    fn storage_write_fault_uses_storage_facet_not_audio_source_fault() {
        let clock = FakeClock::new(0);
        let segment_fs = FakeSegmentFs::default();
        segment_fs.fail_next_writes(1);
        let (sources, handles) = active_sources();
        let mut engine = engine_with(clock, segment_fs, EngineConfig::default(), sources);
        engine.start();

        emit_audio(&engine.sink(), SourceKind::SystemAudio, 0..1);
        engine.pump();

        let dump = engine.health_dump();
        assert!(dump.storage.is_some());
        assert_ne!(dump.app_state, AppPhase::Observing);
        for kind in [SourceKind::SystemAudio, SourceKind::Mic] {
            let report = dump
                .sources
                .iter()
                .find(|source| source.kind == kind)
                .unwrap();
            assert!(
                !matches!(
                    report.state,
                    SourceState::Faulted {
                        reason: ErrorReason::WriteFailed,
                        ..
                    }
                ),
                "{kind:?} should not be blamed for storage writes"
            );
        }

        let display_changes = handles.screen.display_changes();
        engine.apply_command(EngineCommand::DisplayChanged);
        assert_eq!(handles.screen.display_changes(), display_changes + 1);
    }

    #[test]
    fn pending_seal_sweep_accumulates_multiple_past_keys_and_never_seals_live_key() {
        let clock = FakeClock::new(299);
        let segment_fs = FakeSegmentFs::default();
        let segment_view = segment_fs.clone();
        segment_fs.fail_next_finalizes(10);
        let (sources, _) = active_sources();
        let mut engine = engine_with(
            clock.clone(),
            segment_fs.clone(),
            EngineConfig::default(),
            sources,
        );

        engine.start();
        clock.set(300);
        engine.pump();
        clock.set(600);
        engine.pump();

        let first = segment_for(299, DEFAULT_SEGMENT_SECS);
        let second = segment_for(300, DEFAULT_SEGMENT_SECS);
        let live = segment_for(600, DEFAULT_SEGMENT_SECS);
        assert_eq!(engine.pending_seals.len(), 2);
        assert!(engine.pending_seals.contains_key(&first));
        assert!(engine.pending_seals.contains_key(&second));
        assert_eq!(engine.current_segment.as_ref().unwrap().key, live);

        segment_fs.fail_next_finalizes(0);
        let retry_at = engine.storage_retry_at.unwrap();
        clock.set(retry_at.saturating_add(1));
        engine.pump();

        assert!(engine.pending_seals.is_empty());
        let events = segment_view.events();
        assert!(events.contains(&FsEvent::Finalize(first)));
        assert!(events.contains(&FsEvent::Finalize(second)));
        assert!(!events.contains(&FsEvent::Finalize(live)));
    }

    #[test]
    fn display_change_forwards_to_screen_source() {
        let (sources, handles) = active_sources();
        let mut engine = engine_with(
            FakeClock::new(0),
            FakeSegmentFs::default(),
            EngineConfig::default(),
            sources,
        );

        engine.on_display_changed();

        assert_eq!(handles.screen.display_changes(), 1);
    }

    #[test]
    fn frame_rate_is_measured_from_screen_chunks_and_elapsed_time() {
        let clock = FakeClock::new(0);
        let (sources, _) = active_sources();
        let mut engine = engine_with(
            clock.clone(),
            FakeSegmentFs::default(),
            EngineConfig::default(),
            sources,
        );
        engine.start();
        let sink = engine.sink();

        clock.set(5);
        emit_screen(&sink, 0..10);
        engine.pump();

        assert_eq!(engine.health_dump().frame_rate, Some(2));
    }

    #[test]
    fn finalize_error_leaves_segment_incomplete_and_faults_screen_write_failed() {
        let clock = FakeClock::new(299);
        let segment_fs = FakeSegmentFs::default();
        let segment_view = segment_fs.clone();
        let encoder = FakeScreenEncoder::with_events(segment_view.event_log());
        encoder.push_finalize_error(EncoderError::new(
            EncoderErrorKind::FinalizeFailed,
            "finalize failed",
        ));
        let (sources, _) = active_sources_with_encoder(encoder);
        let mut engine = engine_with(clock.clone(), segment_fs, EngineConfig::default(), sources);
        engine.start();
        emit_screen(&engine.sink(), 0..1);
        engine.pump();

        clock.set(300);
        engine.pump();

        let old = segment_for(299, DEFAULT_SEGMENT_SECS);
        assert!(!segment_view.events().contains(&FsEvent::Finalize(old)));
        let screen = engine
            .health_dump()
            .sources
            .into_iter()
            .find(|source| source.kind == SourceKind::Screen)
            .unwrap();
        assert_eq!(
            screen.state,
            SourceState::Faulted {
                reason: ErrorReason::WriteFailed,
                detail: "finalize failed".into()
            }
        );
    }

    #[test]
    fn encoder_health_is_folded_into_health_dump() {
        let encoder = FakeScreenEncoder::default();
        let encoder_view = encoder.clone();
        let (sources, _) = active_sources_with_encoder(encoder);
        let mut engine = engine_with(
            FakeClock::new(0),
            FakeSegmentFs::default(),
            EngineConfig::default(),
            sources,
        );
        engine.start();
        emit_screen(&engine.sink(), 0..3);
        engine.pump();

        let health = engine.health_dump().screen_encoder.unwrap();
        assert_eq!(health, encoder_view.health_snapshot());
        assert_eq!(health.frames_consumed, 3);
        assert_eq!(health.samples_written, 3);
        assert!(health.last_error.is_none());
    }

    #[test]
    fn encode_error_faults_screen_with_write_failed() {
        let encoder = FakeScreenEncoder::default();
        encoder.push_encode_error(EncoderError::new(
            EncoderErrorKind::EncodeFailed,
            "write sample failed",
        ));
        let (sources, _) = active_sources_with_encoder(encoder);
        let mut engine = engine_with(
            FakeClock::new(0),
            FakeSegmentFs::default(),
            EngineConfig::default(),
            sources,
        );
        engine.start();
        emit_screen(&engine.sink(), 0..1);
        engine.pump();

        let screen = engine
            .health_dump()
            .sources
            .into_iter()
            .find(|source| source.kind == SourceKind::Screen)
            .unwrap();
        assert_eq!(
            screen.state,
            SourceState::Faulted {
                reason: ErrorReason::WriteFailed,
                detail: "write sample failed".into()
            }
        );
    }

    #[test]
    fn drops_mismatched_resolution_until_next_rotation_and_reports_delta() {
        let clock = FakeClock::new(299);
        let encoder = FakeScreenEncoder::default();
        let encoder_view = encoder.clone();
        let (sources, _) = active_sources_with_encoder(encoder);
        let mut engine = engine_with(
            clock.clone(),
            FakeSegmentFs::default(),
            EngineConfig::default(),
            sources,
        );
        engine.start();
        let sink = engine.sink();

        emit_screen_size(&sink, 0, 2, 2);
        emit_screen_size(&sink, 1, 4, 2);
        engine.pump();

        let health = encoder_view.health_snapshot();
        assert_eq!(health.frames_consumed, 2);
        assert_eq!(health.samples_written, 1);
        assert!(health.last_error.unwrap().contains("do not match"));

        clock.set(300);
        engine.pump();
        emit_screen_size(&sink, 2, 4, 2);
        engine.pump();

        let health = encoder_view.health_snapshot();
        assert_eq!(health.frames_consumed, 3);
        assert_eq!(health.samples_written, 2);
        assert!(health.last_error.is_none());
    }

    #[test]
    fn device_removed_finalize_failure_leaves_incomplete_and_next_open_retries_mft() {
        let clock = FakeClock::new(299);
        let segment_fs = FakeSegmentFs::default();
        let segment_view = segment_fs.clone();
        let encoder = FakeScreenEncoder::with_events(segment_view.event_log());
        let encoder_view = encoder.clone();
        encoder.push_finalize_error(EncoderError::new(
            EncoderErrorKind::DeviceLost,
            "DXGI_ERROR_DEVICE_REMOVED",
        ));
        let (sources, _) = active_sources_with_encoder(encoder);
        let mut engine = engine_with(clock.clone(), segment_fs, EngineConfig::default(), sources);
        engine.start();

        emit_screen(&engine.sink(), 0..1);
        engine.pump();
        clock.set(300);
        engine.pump();
        emit_screen(&engine.sink(), 1..2);
        engine.pump();

        let old = segment_for(299, DEFAULT_SEGMENT_SECS);
        assert!(!segment_view.events().contains(&FsEvent::Finalize(old)));
        assert_eq!(encoder_view.open_count(), 2);
        assert!(encoder_view.health_snapshot().last_error.is_none());
    }

    #[test]
    fn zero_frame_window_produces_no_screen_file_and_empty_upload_is_dropped() {
        let clock = FakeClock::new(299);
        let segment_fs = FakeSegmentFs::default();
        let segment_view = segment_fs.clone();
        let encoder = FakeScreenEncoder::with_events(segment_view.event_log());
        let (sources, _) = active_sources_with_encoder(encoder);
        let mut engine = engine_with(clock.clone(), segment_fs, EngineConfig::default(), sources);
        engine.start();

        clock.set(300);
        engine.pump();

        assert!(!segment_view.events().iter().any(|event| matches!(
            event,
            FsEvent::EncoderOpen(_, _) | FsEvent::EncoderEncode(_)
        )));
        assert!(segment_view.writes().is_empty());
    }

    #[tokio::test]
    async fn loopback_health_serves_fixed_dump() {
        let fed = HealthDump {
            app_state: AppPhase::Idle,
            sources: Vec::new(),
            frame_rate: None,
            segment_dir: None,
            segment_seconds_remaining: None,
            engine_ready: true,
            version: "test".into(),
            sync: SyncSnapshot::default(),
            screen_encoder: None,
            exclusions: None,
            storage: None,
            pause: None,
            views: Default::default(),
            pump_degraded: false,
        };
        let expected = observer_health::to_pretty_json(&fed).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(serve_health(listener, Arc::new(Mutex::new(fed))));

        let mut bad = TcpStream::connect(addr).await.unwrap();
        bad.shutdown().await.unwrap();
        drop(bad);

        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        task.abort();

        let response = String::from_utf8(response).unwrap();
        let (_, body) = response.split_once("\r\n\r\n").unwrap();
        assert_eq!(body, expected);
    }
}
