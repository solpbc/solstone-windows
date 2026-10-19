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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use observer_lifecycle::{BackoffConfig, Lifecycle, RetryDecision};
use observer_model::setup::{SetupAuthority, SetupSnapshot};
use observer_model::{
    AppPhase, CaptureChunk, CaptureSink, Clock, EncoderError, ErrorReason, HealthDump, MicSource,
    PauseReason, PreviewSnapshot, ScreenEncoder, ScreenFrame, ScreenPixelFormat,
    ScreenPreviewFrame, ScreenSource, SourceError, SourceKind, SourceReport, SourceState,
    StorageHealth, SyncSnapshot, SystemAudioSource,
};
use observer_recovery::{recover_all, RecoveryFs, RecoveryOutcome};
use observer_segment::{
    seconds_until_next_boundary, segment_for, should_rotate, SegmentFs, DEFAULT_SEGMENT_SECS,
};
use observer_state::{reduce, AppEvent, OwnerPause, StateMachine};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, watch};

use observer_model::desktop_activity::{DesktopActivityRow, DesktopActivitySource};
pub use observer_model::BREAKER_OPEN_MARKER;

pub mod desktop_activity;
use desktop_activity::DesktopActivityScheduler;

mod pause_store;
pub use pause_store::{FilePauseStore, HeldPause, PauseStore};

/// Ephemeral preview consumer gate. Tracks the generation and current source kind.
#[derive(Debug, Default)]
pub struct ProofGate {
    generation: u64,
    source: Option<SourceKind>,
}

impl ProofGate {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn open(&mut self, kind: SourceKind) -> u64 {
        self.generation += 1;
        self.source = Some(kind);
        self.generation
    }

    pub fn close(&mut self) -> u64 {
        self.generation += 1;
        self.source = None;
        self.generation
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn source(&self) -> Option<SourceKind> {
        self.source
    }
}

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
    /// The owner resumed: ends the owner's pause and any system pause.
    Resume,
    /// The session unlocked or the machine woke: ends a lock/suspend pause and
    /// leaves an owner's pause in place.
    SystemResume,
    /// Toggle between paused and observing — the global hotkey's action. Pauses
    /// indefinitely when observing, resumes when paused. The engine resolves the
    /// direction because it owns the authoritative phase.
    TogglePause,
    DisplayChanged,
    SetSourceEnabled {
        kind: SourceKind,
        enabled: bool,
    },
    SetDesktopActivityPreferences {
        window_titles: bool,
        page_addresses: bool,
    },
    ClearPreviews,
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

    fn now_epoch_us(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros()
            .min(u64::MAX as u128) as u64
    }

    fn now_monotonic_us(&self) -> u64 {
        observer_model::monotonic_us()
    }
}

struct InputAdmission {
    source_generation: u64,
    stream_generation: u64,
    proof_generation: u64,
}

enum CaptureEvent {
    Audio(SourceKind, InputAdmission, CaptureChunk),
    Screen(InputAdmission, ScreenFrame),
}

struct EngineSink {
    tx: mpsc::UnboundedSender<CaptureEvent>,
    generations: BTreeMap<SourceKind, Arc<AtomicU64>>,
    streams: BTreeMap<SourceKind, Arc<AtomicU64>>,
    proof: Arc<Mutex<ProofGate>>,
}

impl EngineSink {
    fn admission(&self, kind: SourceKind) -> InputAdmission {
        InputAdmission {
            source_generation: self.generations[&kind].load(Ordering::SeqCst),
            stream_generation: self.streams[&kind].load(Ordering::SeqCst),
            proof_generation: self.proof.lock().map(|p| p.generation()).unwrap_or(0),
        }
    }
}

impl CaptureSink for EngineSink {
    fn emit(&self, chunk: CaptureChunk) {
        let admission = self.admission(chunk.source);
        let _ = self
            .tx
            .send(CaptureEvent::Audio(chunk.source, admission, chunk));
    }

    fn emit_screen_frame(&self, frame: ScreenFrame) {
        let admission = self.admission(SourceKind::Screen);
        let _ = self.tx.send(CaptureEvent::Screen(admission, frame));
    }
}

struct SourceSink {
    kind: SourceKind,
    generation: u64,
    stream: Arc<AtomicU64>,
    proof: Arc<Mutex<ProofGate>>,
    tx: mpsc::UnboundedSender<CaptureEvent>,
}

impl SourceSink {
    fn admission(&self) -> InputAdmission {
        InputAdmission {
            source_generation: self.generation,
            stream_generation: self.stream.load(Ordering::SeqCst),
            proof_generation: self.proof.lock().map(|p| p.generation()).unwrap_or(0),
        }
    }
}

impl CaptureSink for SourceSink {
    fn begin_stream(&self) {
        self.stream.fetch_add(1, Ordering::SeqCst);
    }

    fn emit(&self, chunk: CaptureChunk) {
        let _ = self
            .tx
            .send(CaptureEvent::Audio(self.kind, self.admission(), chunk));
    }

    fn emit_screen_frame(&self, frame: ScreenFrame) {
        let _ = self.tx.send(CaptureEvent::Screen(self.admission(), frame));
    }
}

struct OpenSegment {
    key: observer_model::SegmentKey,
    dir: String,
    opened_epoch_secs: u64,
    opened_monotonic_us: u64,
    boundary_offset_us: u64,
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
    pub desktop_activity: Box<dyn DesktopActivitySource>,
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

#[derive(Debug, Clone)]
pub struct PendingActivityRow {
    pub key: observer_model::SegmentKey,
    pub file_name: String,
    pub line: String,
    pub committed_len: u64,
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
    /// Where the owner's pause is kept across restarts; `None` keeps it in memory.
    pause_store: Option<Box<dyn PauseStore>>,
    /// Wave-2 sync (pairing + upload) snapshot, published by the sync layer and
    /// folded into every `HealthDump`. Default = not-paired/idle, so the engine
    /// is unchanged when sync isn't running.
    sync: Arc<Mutex<SyncSnapshot>>,
    generations: BTreeMap<SourceKind, Arc<AtomicU64>>,
    preview: PreviewSnapshot,
    preview_tx: watch::Sender<PreviewSnapshot>,
    last_mic_preview_us: Option<u64>,
    last_sys_preview_us: Option<u64>,
    last_screen_preview_100ns: Option<i64>,
    setup: SetupSnapshot,
    setup_authority: Arc<Mutex<SetupAuthority>>,
    proof: Arc<Mutex<ProofGate>>,
    desktop_activity_scheduler: DesktopActivityScheduler,
    pending_activity_rows: std::collections::VecDeque<PendingActivityRow>,
    committed_activity_lens: std::collections::HashMap<(observer_model::SegmentKey, String), u64>,
    activity_batch_written: bool,
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
        let (preview_tx, _) = watch::channel(PreviewSnapshot::default());
        let generations = BTreeMap::from([
            (SourceKind::Screen, Arc::new(AtomicU64::new(0))),
            (SourceKind::SystemAudio, Arc::new(AtomicU64::new(0))),
            (SourceKind::Mic, Arc::new(AtomicU64::new(0))),
            (SourceKind::DesktopActivity, Arc::new(AtomicU64::new(0))),
        ]);
        let proof = Arc::new(Mutex::new(ProofGate::new()));
        let streams = BTreeMap::from([
            (SourceKind::Screen, Arc::new(AtomicU64::new(0))),
            (SourceKind::SystemAudio, Arc::new(AtomicU64::new(0))),
            (SourceKind::Mic, Arc::new(AtomicU64::new(0))),
            (SourceKind::DesktopActivity, Arc::new(AtomicU64::new(0))),
        ]);
        let sink = Arc::new(EngineSink {
            tx,
            generations: generations.clone(),
            streams,
            proof: proof.clone(),
        });
        let mut state = StateMachine::new();
        reduce(&mut state, AppEvent::EngineReady);

        let setup_authority = Arc::new(Mutex::new(SetupAuthority::new()));

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
            pause_store: None,
            sync: Arc::new(Mutex::new(SyncSnapshot::default())),
            generations,
            preview: PreviewSnapshot::default(),
            preview_tx,
            last_mic_preview_us: None,
            last_sys_preview_us: None,
            last_screen_preview_100ns: None,
            setup: SetupSnapshot::default(),
            setup_authority,
            proof,
            desktop_activity_scheduler: DesktopActivityScheduler::new(
                false,
                false,
                observer_exclusion::ExclusionRules::default(),
                format!(
                    "{:016x}",
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos() as u64
                        ^ (std::process::id() as u64)
                ),
            ),
            pending_activity_rows: std::collections::VecDeque::new(),
            committed_activity_lens: std::collections::HashMap::new(),
            activity_batch_written: false,
        };
        for outcome in &outcomes {
            if let RecoveryOutcome::Deferred(key) = outcome {
                engine.pending_seals.insert(*key, None);
            }
        }
        if !engine.pending_seals.is_empty() {
            engine.note_storage_fault("recovered segment awaits sealing".into());
        }
        engine.refresh_health();

        Ok((engine, outcomes))
    }

    /// Stores the shared exclusion rules handle for desktop activity.
    pub fn set_exclusion_rules_handle(
        &mut self,
        handle: Arc<RwLock<observer_exclusion::ExclusionRules>>,
    ) {
        self.desktop_activity_scheduler
            .set_exclusion_rules_handle(handle);
    }

    /// Stores the shared exclusion availability handle for desktop activity.
    pub fn set_exclusion_availability_handle(
        &mut self,
        handle: Arc<std::sync::atomic::AtomicBool>,
    ) {
        self.desktop_activity_scheduler
            .set_exclusion_availability_handle(handle);
    }

    /// Setup authority handle.
    pub fn setup_authority(&self) -> Arc<Mutex<SetupAuthority>> {
        self.setup_authority.clone()
    }

    /// Proof gate handle.
    pub fn proof_gate(&self) -> Arc<Mutex<ProofGate>> {
        self.proof.clone()
    }

    /// Bind the setup storage root.
    pub fn set_setup_root(&self, root: std::path::PathBuf) {
        if let Ok(mut auth) = self.setup_authority.lock() {
            auth.set_root(root);
        }
    }

    /// Open ephemeral proof consumer for a source kind.
    pub fn open_proof_consumer(&mut self, kind: SourceKind) -> u64 {
        let gen = self.proof.lock().map(|mut p| p.open(kind)).unwrap_or(0);
        self.clear_previews();
        gen
    }

    /// Close ephemeral proof consumer.
    pub fn close_proof_consumer(&mut self) -> u64 {
        let gen = self.proof.lock().map(|mut p| p.close()).unwrap_or(0);
        self.clear_previews();
        gen
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
            listener_faults: observer_model::ListenerFaults::default(),
            setup: self.setup.clone(),
        }
    }

    /// Subscribe to ephemeral preview updates.
    pub fn preview_watch(&self) -> watch::Receiver<PreviewSnapshot> {
        self.preview_tx.subscribe()
    }

    /// Read the latest ephemeral preview snapshot.
    pub fn preview_snapshot(&self) -> PreviewSnapshot {
        self.preview.clone()
    }

    /// Update configured source choices and start-at-login preference from `setup`.
    /// Stores `setup` and reduces `SourceEnabled` for each kind without starting sources
    /// or clearing a pause.
    pub fn set_source_choices(&mut self, setup: SetupSnapshot) {
        if let Ok(mut auth) = self.setup_authority.lock() {
            auth.seed(&setup);
        }
        self.reduce(AppEvent::SourceEnabled {
            kind: SourceKind::Screen,
            enabled: setup.screen,
        });
        self.reduce(AppEvent::SourceEnabled {
            kind: SourceKind::SystemAudio,
            enabled: setup.system_audio,
        });
        self.reduce(AppEvent::SourceEnabled {
            kind: SourceKind::Mic,
            enabled: setup.mic,
        });
        self.reduce(AppEvent::SourceEnabled {
            kind: SourceKind::DesktopActivity,
            enabled: setup.desktop_activity,
        });
        let now_ms = self.clock.now_epoch_secs() * 1000;
        let closed = self.desktop_activity_scheduler.set_preferences(
            self.sources.desktop_activity.as_mut(),
            setup.desktop_activity_window_titles,
            setup.desktop_activity_page_addresses,
            now_ms,
        );
        for row in closed {
            self.append_activity_row(&row);
        }
        self.refresh_health();
    }

    /// Enable or disable a single source.
    pub fn set_source_enabled(&mut self, kind: SourceKind, enabled: bool) {
        if let Ok(mut auth) = self.setup_authority.lock() {
            auth.note_running(kind, enabled);
        }
        if !enabled {
            self.increment_generation(kind);
            self.stop_source(kind);
            self.retry_at_epoch_secs.remove(&kind);
            self.clear_source_preview(kind);
            self.reduce(AppEvent::SourceEnabled {
                kind,
                enabled: false,
            });
            self.apply_source_report(SourceReport {
                kind,
                state: SourceState::Inactive,
                device: None,
            });
        } else {
            self.reduce(AppEvent::SourceEnabled {
                kind,
                enabled: true,
            });
            if self.state.phase() == AppPhase::Paused || !self.state.run_requested() {
                self.refresh_health();
                return;
            }
            self.ensure_segment_for_now();
            self.start_source(kind);
        }
        self.fold_source_states();
        self.refresh_health();
    }

    /// Clear all ephemeral preview levels and screen frames.
    pub fn clear_previews(&mut self) {
        let consumer_generation = self.proof.lock().map(|p| p.generation()).unwrap_or(0);
        self.preview = PreviewSnapshot {
            consumer_generation,
            ..Default::default()
        };
        self.last_mic_preview_us = None;
        self.last_sys_preview_us = None;
        self.last_screen_preview_100ns = None;
        let _ = self.preview_tx.send(self.preview.clone());
    }

    /// Clear ephemeral preview state for a single source.
    pub fn clear_source_preview(&mut self, kind: SourceKind) {
        let consumer_generation = self.proof.lock().map(|p| p.generation()).unwrap_or(0);
        self.preview.consumer_generation = consumer_generation;
        match kind {
            SourceKind::Mic => {
                self.preview.mic_level = None;
                self.last_mic_preview_us = None;
            }
            SourceKind::SystemAudio => {
                self.preview.system_audio_level = None;
                self.last_sys_preview_us = None;
            }
            SourceKind::Screen => {
                self.preview.screen_frame = None;
                self.last_screen_preview_100ns = None;
            }
            SourceKind::DesktopActivity => {}
        }
        let _ = self.preview_tx.send(self.preview.clone());
    }

    fn increment_generation(&self, kind: SourceKind) {
        if let Some(gen) = self.generations.get(&kind) {
            gen.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Keep the owner's pause in `store` from now on, restoring one still held
    /// from before a restart. Call before [`Self::start`] so a held pause is in
    /// effect before any source can start. A timed pause whose deadline has
    /// passed is removed and does not apply.
    pub fn hold_owner_pause_in(&mut self, store: Box<dyn PauseStore>) {
        let now = self.clock.now_epoch_secs();
        let expires_at_epoch_secs = match store.load() {
            None => None,
            Some(HeldPause::UntilResumed) => Some(None),
            Some(HeldPause::Until(deadline)) if deadline > now => Some(Some(deadline)),
            Some(HeldPause::Until(_)) => {
                if let Err(error) = store.clear() {
                    tracing::warn!(target: "engine", %error, "could not remove an ended pause");
                }
                None
            }
        };
        if let Some(expires_at_epoch_secs) = expires_at_epoch_secs {
            tracing::info!(target: "engine", timed = expires_at_epoch_secs.is_some(), "owner pause restored");
            self.reduce(AppEvent::RequestedPause {
                reason: PauseReason::Operator,
                expires_at_epoch_secs,
            });
        }
        self.pause_store = Some(store);
        self.refresh_health();
    }

    /// Start enabled sources and open the current segment. Source start failures are
    /// folded into state and do not abort the engine. A pause in effect (an
    /// owner's pause restored at launch) holds: nothing starts until it ends.
    pub fn start(&mut self) {
        self.reduce(AppEvent::RequestedStart);
        self.adopt_current_segment();
        if self.state.phase() == AppPhase::Paused {
            self.fold_source_states();
            self.refresh_health();
            return;
        }
        if self.state.any_source_enabled() {
            self.ensure_segment_for_now();
            for kind in [
                SourceKind::Screen,
                SourceKind::SystemAudio,
                SourceKind::Mic,
                SourceKind::DesktopActivity,
            ] {
                if self.state.is_source_enabled(kind) {
                    self.start_source(kind);
                }
            }
        }
        self.fold_source_states();
        self.refresh_health();
    }

    /// Stop every source and seal the current segment, when one is open.
    pub fn stop(&mut self) {
        self.rotate_segments(true);
        self.sources.screen.stop();
        self.sources.system_audio.stop();
        self.sources.mic.stop();
        let now_ms = self.clock.now_epoch_us() / 1000;
        let closed = self
            .desktop_activity_scheduler
            .close_all(self.sources.desktop_activity.as_mut(), now_ms);
        for row in closed {
            self.append_activity_row(&row);
        }
        self.sources.desktop_activity.stop();
        self.drain_events();
        self.flush_activity_queue();

        if let Some(segment) = self.current_segment.take() {
            self.seal_segment(segment);
        }
        self.clear_previews();
        self.fold_source_states();
        self.refresh_health();
    }

    /// Forward display changes to the screen source. Segment rotation stays
    /// clock-driven.
    pub fn on_display_changed(&mut self) {
        if !self.state.is_source_enabled(SourceKind::Screen) {
            return;
        }
        self.clear_source_preview(SourceKind::Screen);
        self.sources.screen.on_display_changed();
    }

    /// Deterministic, host-testable unit of engine work.
    pub fn pump(&mut self) {
        self.drain_events();
        self.auto_resume_if_due();
        self.rotate_if_needed();
        self.service_storage();
        self.retry_due_sources();
        self.pump_desktop_activity();
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
                    self.stop();
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
                    self.pump_desktop_activity();
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
        self.rotate_if_needed();
        let was_paused = self.state.phase() == AppPhase::Paused;
        let owner_before = self.state.owner_pause();
        // A resume also restarts sources that faulted or tripped their breaker,
        // so it restarts capture even when nothing was paused.
        let restart = matches!(command, EngineCommand::Resume | EngineCommand::SystemResume);
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
            }
            EngineCommand::Resume => {
                self.reduce(AppEvent::RequestedResume);
            }
            EngineCommand::SystemResume => {
                self.reduce(AppEvent::SystemResumed);
            }
            EngineCommand::TogglePause => {
                if was_paused {
                    self.reduce(AppEvent::RequestedResume);
                } else {
                    self.reduce(AppEvent::RequestedPause {
                        reason: PauseReason::Operator,
                        expires_at_epoch_secs: None,
                    });
                }
            }
            EngineCommand::DisplayChanged => {
                self.on_display_changed();
            }
            EngineCommand::SetSourceEnabled { kind, enabled } => {
                self.set_source_enabled(kind, enabled);
                return;
            }
            EngineCommand::SetDesktopActivityPreferences {
                window_titles,
                page_addresses,
            } => {
                let now_ms = self.clock.now_epoch_us() / 1000;
                let closed = self.desktop_activity_scheduler.set_preferences(
                    self.sources.desktop_activity.as_mut(),
                    window_titles,
                    page_addresses,
                    now_ms,
                );
                for row in closed {
                    self.append_activity_row(&row);
                }
                self.refresh_health();
                return;
            }
            EngineCommand::ClearPreviews => {
                self.clear_previews();
                return;
            }
        }
        self.settle_pause(was_paused, restart, owner_before);
        self.refresh_health();
    }

    /// Act on a pause change: stop capture when a pause began, restart it when
    /// the last pause ended (or on `restart` when nothing is paused), and record
    /// the owner's pause when it changed.
    fn settle_pause(&mut self, was_paused: bool, restart: bool, owner_before: Option<OwnerPause>) {
        let paused = self.state.phase() == AppPhase::Paused;
        if paused && !was_paused {
            // A pause must actually stop capture, not merely relabel the
            // phase: stop every source so nothing is gathered while paused.
            // The open segment stays incomplete until a boundary or stop.
            self.pause_capture();
        } else if !paused && (was_paused || restart) {
            self.resume_capture();
        }
        let owner = self.state.owner_pause();
        if owner != owner_before {
            self.record_owner_pause(owner);
        }
    }

    fn record_owner_pause(&self, owner: Option<OwnerPause>) {
        let Some(store) = &self.pause_store else {
            return;
        };
        let result = match owner {
            Some(OwnerPause {
                expires_at_epoch_secs: Some(deadline),
            }) => store.save(HeldPause::Until(deadline)),
            Some(OwnerPause {
                expires_at_epoch_secs: None,
            }) => store.save(HeldPause::UntilResumed),
            None => store.clear(),
        };
        if let Err(error) = result {
            tracing::warn!(target: "engine", %error, "could not record the owner pause");
        }
    }

    /// Stop all sources so capture truly halts while paused. The open segment
    /// stays incomplete and is sealed only at a boundary or stop.
    fn pause_capture(&mut self) {
        self.sources.screen.stop();
        self.sources.system_audio.stop();
        self.sources.mic.stop();
        self.rotate_segments(true);
        let now_ms = self.clock.now_epoch_us() / 1000;
        let closed = self
            .desktop_activity_scheduler
            .close_all(self.sources.desktop_activity.as_mut(), now_ms);
        for row in closed {
            self.append_activity_row(&row);
        }
        self.sources.desktop_activity.stop();
        self.drain_events();
        self.write_stalled = false;
        self.attempt_clear_storage();
        self.clear_previews();
        self.fold_source_states();
    }

    /// Re-open the current segment and restart every source. The inverse of
    /// [`Self::pause_capture`], used by both an explicit resume and auto-resume.
    fn resume_capture(&mut self) {
        if !self.state.any_source_enabled() {
            self.fold_source_states();
            return;
        }
        self.ensure_segment_for_now();
        for kind in [
            SourceKind::Screen,
            SourceKind::SystemAudio,
            SourceKind::Mic,
            SourceKind::DesktopActivity,
        ] {
            if self.state.is_source_enabled(kind) {
                self.start_source(kind);
            }
        }
        self.fold_source_states();
    }

    /// Auto-resume when a duration-bounded pause has reached its deadline.
    /// Capture restarts only if no lock/suspend pause is also in effect.
    fn auto_resume_if_due(&mut self) {
        if self.state.pause_due_to_expire(self.clock.now_epoch_secs()) {
            let owner_before = self.state.owner_pause();
            self.reduce(AppEvent::OwnerPauseExpired);
            self.settle_pause(true, false, owner_before);
        }
    }

    fn ensure_segment_for_now(&mut self) {
        self.rotate_if_needed();
        if self.current_segment.is_none() {
            self.open_current_segment();
        }
    }

    fn open_segment_key(&mut self, key: observer_model::SegmentKey) {
        if self.current_segment.is_some() {
            return;
        }
        let dir = match self.segment_fs.open_incomplete(key) {
            Ok(dir) => dir,
            Err(error) => {
                self.note_storage_fault(format!("{error:?}"));
                return;
            }
        };
        self.attach_segment(key, dir);
        self.attempt_clear_storage();
    }

    fn open_current_segment(&mut self) {
        let now = self.clock.now_epoch_secs();
        let key = segment_for(now, self.config.segment_secs);
        self.open_segment_key(key);
    }

    fn adopt_current_segment(&mut self) {
        if self.current_segment.is_some() {
            return;
        }
        let key = segment_for(self.clock.now_epoch_secs(), self.config.segment_secs);
        match self.segment_fs.existing_incomplete(key) {
            Ok(Some(dir)) => self.attach_segment(key, dir),
            Ok(None) => {}
            Err(error) => self.note_storage_fault(format!("{error:?}")),
        }
    }

    fn attach_segment(&mut self, key: observer_model::SegmentKey, dir: String) {
        self.current_segment = Some(OpenSegment {
            key,
            dir,
            opened_epoch_secs: self.clock.now_epoch_secs(),
            opened_monotonic_us: self.clock.now_monotonic_us(),
            boundary_offset_us: self
                .clock
                .now_epoch_us()
                .saturating_sub(key.boundary_epoch_secs.saturating_mul(1_000_000)),
            screen_chunks: 0,
            screen_encoder_open: false,
        });
        self.flush_activity_queue();
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            self.handle_event(event);
        }
    }

    fn handle_event(&mut self, event: CaptureEvent) {
        match event {
            CaptureEvent::Audio(kind, admission, chunk) => {
                let current_gen = self
                    .generations
                    .get(&kind)
                    .map(|g| g.load(Ordering::SeqCst))
                    .unwrap_or(0);
                if admission.source_generation != current_gen
                    || admission.stream_generation
                        != self.sink.streams[&kind].load(Ordering::SeqCst)
                {
                    return;
                }
                if !self.state.is_source_enabled(kind) {
                    return;
                }
                let st = self.source_state(kind);
                if matches!(
                    st,
                    SourceState::Inactive
                        | SourceState::NoInputDevice
                        | SourceState::Faulted { .. }
                ) {
                    self.clear_source_preview(kind);
                    return;
                }
                self.update_audio_preview(kind, &chunk, admission.proof_generation);
                self.write_audio_chunk(chunk);
            }
            CaptureEvent::Screen(admission, frame) => {
                let current_gen = self
                    .generations
                    .get(&SourceKind::Screen)
                    .map(|g| g.load(Ordering::SeqCst))
                    .unwrap_or(0);
                if admission.source_generation != current_gen
                    || admission.stream_generation
                        != self.sink.streams[&SourceKind::Screen].load(Ordering::SeqCst)
                {
                    return;
                }
                if !self.state.is_source_enabled(SourceKind::Screen) {
                    return;
                }
                let st = self.source_state(SourceKind::Screen);
                if matches!(
                    st,
                    SourceState::Inactive
                        | SourceState::NoInputDevice
                        | SourceState::Faulted { .. }
                ) {
                    self.clear_source_preview(SourceKind::Screen);
                    return;
                }
                self.update_screen_preview(&frame, admission.proof_generation);
                self.encode_screen_frame(frame);
            }
        }
    }

    fn write_audio_chunk(&mut self, mut chunk: CaptureChunk) {
        let Some(segment) = self.current_segment.as_mut() else {
            return;
        };

        if let Some(at_us) = chunk.at_us {
            chunk.at_us = Some(
                segment
                    .boundary_offset_us
                    .saturating_add(at_us.saturating_sub(segment.opened_monotonic_us)),
            );
        }

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
        self.rotate_segments(false);
    }

    fn rotate_segments(&mut self, closing: bool) {
        let now = self.clock.now_epoch_secs();
        while let Some(current) = self.current_segment.as_ref() {
            if !should_rotate(current.key, now, self.config.segment_secs) {
                break;
            }

            if now < current.key.boundary_epoch_secs {
                // A backward clock step cannot be reached by advancing boundaries.
                // End old intervals at their own start, then require a fresh probe.
                let rows = self.desktop_activity_scheduler.close_all(
                    self.sources.desktop_activity.as_mut(),
                    now.saturating_mul(1000),
                );
                for row in rows {
                    self.append_activity_row(&row);
                }
                self.sources.desktop_activity.stop();
                let old = self.current_segment.take().expect("current checked above");
                self.seal_segment(old);
                if closing || (self.should_be_capturing() && self.state.any_source_enabled()) {
                    self.open_segment_key(segment_for(now, self.config.segment_secs));
                }
                break;
            }

            let next_boundary_secs = current.key.boundary_epoch_secs + self.config.segment_secs;
            let boundary_ms = next_boundary_secs * 1000;
            let closed = self.desktop_activity_scheduler.on_boundary(boundary_ms);
            for row in closed {
                self.append_activity_row(&row);
            }

            let old_segment = self.current_segment.take().expect("current checked above");
            self.seal_segment(old_segment);

            if closing || (self.should_be_capturing() && self.state.any_source_enabled()) {
                let next_key = segment_for(next_boundary_secs, self.config.segment_secs);
                self.open_segment_key(next_key);
            } else {
                break;
            }
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
            && self.pending_activity_rows.is_empty()
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
            if self
                .pending_activity_rows
                .iter()
                .any(|row| row.key == segment.key)
            {
                self.pending_seals.insert(segment.key, video_end);
                self.note_storage_fault("activity rows await durable write before sealing".into());
                self.renew_artifact_id();
                return;
            }
            if let Err(error) = self.segment_fs.finalize(segment.key, video_end) {
                self.pending_seals.insert(segment.key, video_end);
                self.note_storage_fault(format!("{error:?}"));
            } else {
                self.pending_seals.remove(&segment.key);
                self.renew_artifact_id();
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
            self.flush_activity_queue();
            let pending: Vec<_> = self
                .pending_seals
                .iter()
                .map(|(key, video_end)| (*key, *video_end))
                .collect();
            for (key, video_end) in pending {
                if self
                    .current_segment
                    .as_ref()
                    .is_some_and(|segment| segment.key == key)
                    || self.pending_activity_rows.iter().any(|row| row.key == key)
                {
                    continue;
                }
                match self.segment_fs.finalize(key, video_end) {
                    Ok(()) => {
                        self.pending_seals.remove(&key);
                        self.renew_artifact_id();
                    }
                    Err(error) => {
                        self.note_storage_fault(format!("{error:?}"));
                    }
                }
            }

            if self.should_be_capturing()
                && self.state.any_source_enabled()
                && self.current_segment.is_none()
            {
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

    fn source_state(&self, kind: SourceKind) -> SourceState {
        match kind {
            SourceKind::Mic => self.sources.mic.state(),
            SourceKind::SystemAudio => self.sources.system_audio.state(),
            SourceKind::Screen => self.sources.screen.state(),
            SourceKind::DesktopActivity => {
                if !self.desktop_activity_scheduler.is_exclusion_available() {
                    SourceState::Faulted {
                        reason: ErrorReason::Unknown,
                        detail: String::new(),
                    }
                } else {
                    self.sources.desktop_activity.state()
                }
            }
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
                state: self.source_state(SourceKind::SystemAudio),
                device: None,
            },
            SourceReport {
                kind: SourceKind::Mic,
                state: self.source_state(SourceKind::Mic),
                device: None,
            },
            SourceReport {
                kind: SourceKind::DesktopActivity,
                state: self.source_state(SourceKind::DesktopActivity),
                device: None,
            },
        ];

        for report in reports {
            self.apply_source_report(report);
        }
    }

    pub fn set_exclusion_rules(&mut self, rules: observer_exclusion::ExclusionRules) {
        let now_ms = self.clock.now_epoch_us() / 1000;
        let closed = self.desktop_activity_scheduler.set_exclusion_rules(
            self.sources.desktop_activity.as_mut(),
            rules,
            now_ms,
        );
        for row in closed {
            self.append_activity_row(&row);
        }
    }

    fn generate_artifact_id(&self) -> String {
        static ARTIFACT_COUNTER: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(1);
        let count = ARTIFACT_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        let id_val = nanos ^ (std::process::id() as u64).rotate_left(16) ^ count;
        format!("{:016x}", id_val)
    }

    pub fn renew_artifact_id(&mut self) {
        self.desktop_activity_scheduler.artifact_id = self.generate_artifact_id();
        self.activity_batch_written = false;
    }

    fn ensure_unique_artifact_id(&mut self, key: observer_model::SegmentKey) {
        let dir_path_buf = match self.current_segment.as_ref() {
            Some(s) => std::path::PathBuf::from(&s.dir),
            None => return,
        };
        let sealed_dir = dir_path_buf.parent().map(|p| p.join(key.index.to_string()));

        for _ in 0..100 {
            let file_name = desktop_activity::activity_file_name(
                key.boundary_epoch_secs,
                0,
                &self.desktop_activity_scheduler.artifact_id,
                self.config.segment_secs,
            );
            let in_incomplete = dir_path_buf.join(&file_name).exists();
            let in_sealed = sealed_dir
                .as_ref()
                .map(|p| p.join(&file_name).exists())
                .unwrap_or(false);
            if in_incomplete || in_sealed {
                self.renew_artifact_id();
            } else {
                break;
            }
        }
    }

    fn append_activity_row(&mut self, row: &DesktopActivityRow) {
        let line = match row.to_jsonl_line() {
            Ok(line) => line,
            Err(error) => {
                tracing::warn!(target: "engine", %error, "could not serialize desktop activity row");
                return;
            }
        };
        let key = segment_for(row.ts / 1000, self.config.segment_secs);
        if !self.activity_batch_written {
            self.ensure_unique_artifact_id(key);
            self.activity_batch_written = true;
        }

        let file_name = desktop_activity::activity_file_name(
            key.boundary_epoch_secs,
            0,
            &self.desktop_activity_scheduler.artifact_id,
            self.config.segment_secs,
        );
        let committed_len_entry = self
            .committed_activity_lens
            .entry((key, file_name.clone()))
            .or_insert(0);
        let committed_len = *committed_len_entry;
        *committed_len_entry += line.len() as u64;

        self.pending_activity_rows.push_back(PendingActivityRow {
            key,
            file_name,
            line,
            committed_len,
        });
        self.flush_activity_queue();
    }

    fn flush_activity_queue(&mut self) {
        if self.pending_activity_rows.is_empty() {
            return;
        }

        while let Some(pending) = self.pending_activity_rows.front() {
            match self.segment_fs.append_activity(
                pending.key,
                &pending.file_name,
                &pending.line,
                pending.committed_len,
            ) {
                Ok(()) => {
                    self.pending_activity_rows.pop_front();
                }
                Err(error) => {
                    self.note_storage_fault(format!("{error:?}"));
                    return;
                }
            }
        }

        if self.pending_activity_rows.is_empty() {
            self.attempt_clear_storage();
        }
    }

    fn pump_desktop_activity(&mut self) {
        if !self.should_be_capturing() || !self.state.is_source_enabled(SourceKind::DesktopActivity)
        {
            return;
        }
        if self.sources.desktop_activity.state() != SourceState::Active {
            let _ = self.sources.desktop_activity.start();
        }
        let now_ms = self.clock.now_epoch_us() / 1000;
        if self.sources.desktop_activity.state() != SourceState::Active {
            let rows = self
                .desktop_activity_scheduler
                .source_unavailable(self.sources.desktop_activity.as_mut(), now_ms);
            for row in rows {
                self.append_activity_row(&row);
            }
            return;
        }
        let rows = self
            .desktop_activity_scheduler
            .poll_and_step(self.sources.desktop_activity.as_mut(), now_ms);
        for row in rows {
            self.append_activity_row(&row);
        }
    }

    fn apply_source_report(&mut self, mut report: SourceReport) {
        if matches!(
            report.state,
            SourceState::Inactive | SourceState::NoInputDevice | SourceState::Faulted { .. }
        ) {
            self.clear_source_preview(report.kind);
        }
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
        self.clear_source_preview(SourceKind::Screen);
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
        self.clear_source_preview(kind);
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
        if self.state.phase() == AppPhase::Paused {
            return;
        }
        let now = self.clock.now_epoch_secs();
        let due: Vec<SourceKind> = self
            .retry_at_epoch_secs
            .iter()
            .filter_map(|(kind, retry_at)| (*retry_at <= now).then_some(*kind))
            .collect();

        for kind in due {
            self.retry_at_epoch_secs.remove(&kind);
            if self.state.is_source_enabled(kind) {
                self.stop_source(kind);
                self.start_source(kind);
            }
        }
    }

    fn refresh_health(&mut self) {
        if let Ok(auth) = self.setup_authority.lock() {
            self.setup = auth.projection();
        }
        let mut dump = self.health_dump();
        if let Ok(mut shared) = self.shared_health.lock() {
            // `views` is app-owned (the frontend writes it via the beacon command).
            // The engine rebuilds the dump from scratch each tick, so carry the
            // earned beacon forward — never clobber it back to empty.
            dump.views = shared.views.clone();
            // `pump_degraded` is app-owned like `views`; carry it forward too.
            dump.pump_degraded = shared.pump_degraded;
            // `listener_faults` is app-owned like `pump_degraded`; carry it forward too.
            dump.listener_faults = shared.listener_faults.clone();
            *shared = dump.clone();
        }
        self.health_tx.send_replace(dump);
    }

    fn start_source(&mut self, kind: SourceKind) {
        if !self.state.is_source_enabled(kind) {
            return;
        }
        let gen = self
            .generations
            .get(&kind)
            .expect("generations map contains every source")
            .fetch_add(1, Ordering::SeqCst)
            + 1;
        let sink: Arc<dyn CaptureSink> = Arc::new(SourceSink {
            kind,
            generation: gen,
            stream: self.sink.streams[&kind].clone(),
            proof: self.proof.clone(),
            tx: self.sink.tx.clone(),
        });
        let result = match kind {
            SourceKind::Screen => self.sources.screen.start(sink),
            SourceKind::SystemAudio => self.sources.system_audio.start(sink),
            SourceKind::Mic => self.sources.mic.start(sink),
            SourceKind::DesktopActivity => self.sources.desktop_activity.start(),
        };

        match result {
            Ok(()) => self.mark_source_success(kind),
            Err(error) => self.apply_source_error(kind, error),
        }
    }

    fn stop_source(&mut self, kind: SourceKind) {
        self.increment_generation(kind);
        match kind {
            SourceKind::Screen => self.sources.screen.stop(),
            SourceKind::SystemAudio => self.sources.system_audio.stop(),
            SourceKind::Mic => self.sources.mic.stop(),
            SourceKind::DesktopActivity => {
                self.ensure_segment_for_now();
                let now_ms = self.clock.now_epoch_us() / 1000;
                let closed = self
                    .desktop_activity_scheduler
                    .close_all(self.sources.desktop_activity.as_mut(), now_ms);
                for row in closed {
                    self.append_activity_row(&row);
                }
                self.sources.desktop_activity.stop();
            }
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
            (SourceKind::DesktopActivity, Lifecycle::new(config)),
        ])
    }

    fn storage_backoff_config() -> BackoffConfig {
        BackoffConfig {
            breaker_threshold: u32::MAX,
            ..BackoffConfig::default()
        }
    }

    fn update_audio_preview(
        &mut self,
        kind: SourceKind,
        chunk: &CaptureChunk,
        admitted_proof: u64,
    ) {
        let (gate_source, gen) = self
            .proof
            .lock()
            .map(|p| (p.source(), p.generation()))
            .unwrap_or((None, 0));
        if gate_source != Some(kind) || gen != admitted_proof {
            return;
        }
        let ts_us = chunk.at_us.unwrap_or_else(|| self.clock.now_monotonic_us());
        match kind {
            SourceKind::Mic => {
                if let Some(last) = self.last_mic_preview_us {
                    if ts_us.saturating_sub(last) < 100_000 {
                        return;
                    }
                }
                let level = chunk
                    .format
                    .map(|fmt| compute_audio_level(&chunk.data, fmt))
                    .unwrap_or(0.0);
                self.last_mic_preview_us = Some(ts_us);
                self.preview.consumer_generation = gen;
                self.preview.mic_level = Some(level);
                let _ = self.preview_tx.send(self.preview.clone());
            }
            SourceKind::SystemAudio => {
                if let Some(last) = self.last_sys_preview_us {
                    if ts_us.saturating_sub(last) < 100_000 {
                        return;
                    }
                }
                let level = chunk
                    .format
                    .map(|fmt| compute_audio_level(&chunk.data, fmt))
                    .unwrap_or(0.0);
                self.last_sys_preview_us = Some(ts_us);
                self.preview.consumer_generation = gen;
                self.preview.system_audio_level = Some(level);
                let _ = self.preview_tx.send(self.preview.clone());
            }
            SourceKind::Screen | SourceKind::DesktopActivity => {}
        }
    }

    fn update_screen_preview(&mut self, frame: &ScreenFrame, admitted_proof: u64) {
        let (gate_source, gen) = self
            .proof
            .lock()
            .map(|p| (p.source(), p.generation()))
            .unwrap_or((None, 0));
        if gate_source != Some(SourceKind::Screen) || gen != admitted_proof {
            return;
        }
        let now_100ns = frame.arrival_100ns;
        if let Some(last) = self.last_screen_preview_100ns {
            if now_100ns.saturating_sub(last) < 2_500_000 {
                return;
            }
        }
        self.last_screen_preview_100ns = Some(now_100ns);
        self.preview.consumer_generation = gen;
        self.preview.screen_frame = Some(downscale_screen_frame(frame));
        let _ = self.preview_tx.send(self.preview.clone());
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
            listener_faults: observer_model::ListenerFaults::default(),
            setup: SetupSnapshot::default(),
        }
    }
}

fn compute_audio_level(data: &[u8], format: observer_model::AudioFormat) -> f32 {
    let channels = format.channels as usize;
    let bytes_per_sample = (format.bits_per_sample / 8) as usize;
    let frame_size = channels.saturating_mul(bytes_per_sample);
    if frame_size == 0 || data.is_empty() {
        return 0.0;
    }
    let usable_len = (data.len() / frame_size) * frame_size;
    if usable_len == 0 {
        return 0.0;
    }
    let mut peak = 0.0f32;
    for frame in data[..usable_len].chunks_exact(frame_size) {
        for channel in 0..channels {
            let offset = channel * bytes_per_sample;
            let sample = if format.is_float && format.bits_per_sample == 32 {
                if offset + 4 <= frame.len() {
                    f32::from_le_bytes([
                        frame[offset],
                        frame[offset + 1],
                        frame[offset + 2],
                        frame[offset + 3],
                    ])
                    .abs()
                } else {
                    0.0
                }
            } else if !format.is_float && format.bits_per_sample == 16 {
                if offset + 2 <= frame.len() {
                    (i16::from_le_bytes([frame[offset], frame[offset + 1]]) as f32 / 32768.0).abs()
                } else {
                    0.0
                }
            } else {
                0.0
            };
            if sample > peak {
                peak = sample;
            }
        }
    }
    peak.clamp(0.0, 1.0)
}

fn downscale_screen_frame(frame: &ScreenFrame) -> ScreenPreviewFrame {
    if frame.width == 0 || frame.height == 0 || frame.pixels.is_empty() {
        return ScreenPreviewFrame {
            width: 0,
            height: 0,
            rgba: Vec::new(),
        };
    }
    let max_dim = frame.width.max(frame.height);
    let (target_w, target_h) = if max_dim <= 320 {
        (frame.width, frame.height)
    } else {
        let tw = (frame.width as u64 * 320 / max_dim as u64).max(1) as u32;
        let th = (frame.height as u64 * 320 / max_dim as u64).max(1) as u32;
        (tw, th)
    };

    let src_stride = frame.width as usize * 4;
    let is_bgra = frame.pixel_format == ScreenPixelFormat::Bgra8;
    let mut rgba = Vec::with_capacity((target_w as usize) * (target_h as usize) * 4);

    for ty in 0..target_h {
        let sy = (ty as u64 * frame.height as u64 / target_h as u64) as usize;
        let row_start = sy * src_stride;
        for tx in 0..target_w {
            let sx = (tx as u64 * frame.width as u64 / target_w as u64) as usize;
            let px_start = row_start + sx * 4;
            if px_start + 4 <= frame.pixels.len() {
                if is_bgra {
                    let b = frame.pixels[px_start];
                    let g = frame.pixels[px_start + 1];
                    let r = frame.pixels[px_start + 2];
                    let a = frame.pixels[px_start + 3];
                    rgba.extend_from_slice(&[r, g, b, a]);
                } else {
                    rgba.extend_from_slice(&frame.pixels[px_start..px_start + 4]);
                }
            } else {
                rgba.extend_from_slice(&[0, 0, 0, 255]);
            }
        }
    }

    ScreenPreviewFrame {
        width: target_w,
        height: target_h,
        rgba,
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
        now_monotonic: Arc<AtomicU64>,
    }

    impl FakeClock {
        fn new(now: u64) -> Self {
            Self {
                now: Arc::new(AtomicU64::new(now)),
                now_monotonic: Arc::new(AtomicU64::new(0)),
            }
        }

        fn set(&self, now: u64) {
            self.now.store(now, Ordering::Relaxed);
        }

        fn advance(&self, secs: u64) {
            self.now.fetch_add(secs, Ordering::Relaxed);
        }

        fn set_monotonic_us(&self, us: u64) {
            self.now_monotonic.store(us, Ordering::Relaxed);
        }
    }

    impl Clock for FakeClock {
        fn now_epoch_secs(&self) -> u64 {
            self.now.load(Ordering::Relaxed)
        }

        fn now_monotonic_us(&self) -> u64 {
            self.now_monotonic.load(Ordering::Relaxed)
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
        writes_with_at_us: Vec<(SegmentKey, SourceKind, u64, Option<u64>)>,
        activities: Vec<(SegmentKey, String, String)>,
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

        fn writes_with_at_us(&self) -> Vec<(SegmentKey, SourceKind, u64, Option<u64>)> {
            self.state.lock().unwrap().writes_with_at_us.clone()
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

        fn existing_incomplete(&mut self, _key: SegmentKey) -> Result<Option<String>, Self::Error> {
            Ok(None)
        }

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
            state
                .writes_with_at_us
                .push((key, chunk.source, chunk.seq, chunk.at_us));
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

        fn append_activity(
            &mut self,
            key: SegmentKey,
            file_name: &str,
            line: &str,
            _committed_len: u64,
        ) -> Result<(), Self::Error> {
            let mut state = self.state.lock().unwrap();
            if state.write_failures_remaining > 0 {
                state.write_failures_remaining -= 1;
                return Err("write failed");
            }
            state
                .activities
                .push((key, file_name.to_string(), line.to_string()));
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeRecoveryFs {
        scans: usize,
        finalized: Vec<SegmentKey>,
        quarantined: Vec<SegmentKey>,
        stale: Vec<StaleSegment>,
        fail_finalize: bool,
    }

    impl RecoveryFs for FakeRecoveryFs {
        type Error = ();

        fn scan_incomplete(&mut self) -> Result<Vec<StaleSegment>, ()> {
            self.scans += 1;
            Ok(self.stale.clone())
        }

        fn finalize(&mut self, seg: &StaleSegment) -> Result<(), ()> {
            if self.fail_finalize {
                return Err(());
            }
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
        last_sink: Option<Arc<dyn CaptureSink>>,
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
                    last_sink: None,
                })),
            }
        }

        fn state(&self) -> SourceState {
            self.inner.lock().unwrap().state.clone()
        }

        fn set_state(&self, state: SourceState) {
            self.inner.lock().unwrap().state = state;
        }

        fn starts(&self) -> usize {
            self.inner.lock().unwrap().starts
        }

        fn stops(&self) -> usize {
            self.inner.lock().unwrap().stops
        }

        fn last_sink(&self) -> Option<Arc<dyn CaptureSink>> {
            self.inner.lock().unwrap().last_sink.clone()
        }

        fn display_changes(&self) -> usize {
            self.inner.lock().unwrap().display_changes
        }

        fn start(&self, sink: Arc<dyn CaptureSink>) -> Result<(), SourceError> {
            let mut inner = self.inner.lock().unwrap();
            inner.starts += 1;
            inner.last_sink = Some(sink);
            if let Some(error) = inner.start_errors.pop_front() {
                Err(error)
            } else {
                Ok(())
            }
        }

        fn queue_start_error(&self, error: SourceError) {
            self.inner.lock().unwrap().start_errors.push_back(error);
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
        fn start(&mut self, sink: Arc<dyn CaptureSink>) -> Result<(), SourceError> {
            self.handle.start(sink)
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
        fn start(&mut self, sink: Arc<dyn CaptureSink>) -> Result<(), SourceError> {
            self.handle.start(sink)
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
        fn start(&mut self, sink: Arc<dyn CaptureSink>) -> Result<(), SourceError> {
            self.handle.start(sink)
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

    #[derive(Clone)]
    struct FakeDesktopActivityHandle {
        inner: Arc<Mutex<FakeDesktopActivityState>>,
    }

    struct FakeDesktopActivityState {
        state: SourceState,
        starts: usize,
        stops: usize,
        cancels: usize,
        events: Vec<observer_model::desktop_activity::DesktopActivityProbeEvent>,
        url_results: Vec<observer_model::desktop_activity::DesktopActivityUrlResult>,
        url_requests: Vec<observer_model::desktop_activity::DesktopActivityUrlRequest>,
        snapshot: Option<observer_model::desktop_activity::ForegroundSnapshot>,
    }

    impl FakeDesktopActivityHandle {
        fn new(state: SourceState) -> Self {
            Self {
                inner: Arc::new(Mutex::new(FakeDesktopActivityState {
                    state,
                    starts: 0,
                    stops: 0,
                    cancels: 0,
                    events: Vec::new(),
                    url_results: Vec::new(),
                    url_requests: Vec::new(),
                    snapshot: None,
                })),
            }
        }

        fn state(&self) -> SourceState {
            self.inner.lock().unwrap().state.clone()
        }

        fn starts(&self) -> usize {
            self.inner.lock().unwrap().starts
        }
    }

    struct FakeDesktopActivity {
        handle: FakeDesktopActivityHandle,
    }

    impl DesktopActivitySource for FakeDesktopActivity {
        fn start(&mut self) -> Result<(), SourceError> {
            self.handle.inner.lock().unwrap().starts += 1;
            Ok(())
        }

        fn stop(&mut self) {
            self.handle.inner.lock().unwrap().stops += 1;
        }

        fn state(&self) -> SourceState {
            self.handle.state()
        }

        fn poll_events(
            &mut self,
        ) -> Vec<observer_model::desktop_activity::DesktopActivityProbeEvent> {
            std::mem::take(&mut self.handle.inner.lock().unwrap().events)
        }

        fn request_url(
            &mut self,
            request: observer_model::desktop_activity::DesktopActivityUrlRequest,
        ) {
            self.handle.inner.lock().unwrap().url_requests.push(request);
        }

        fn poll_url_result(
            &mut self,
        ) -> Option<observer_model::desktop_activity::DesktopActivityUrlResult> {
            let mut inner = self.handle.inner.lock().unwrap();
            if inner.url_results.is_empty() {
                None
            } else {
                Some(inner.url_results.remove(0))
            }
        }

        fn cancel_url(&mut self) {
            self.handle.inner.lock().unwrap().cancels += 1;
        }

        fn foreground_snapshot(
            &mut self,
        ) -> Option<observer_model::desktop_activity::ForegroundSnapshot> {
            self.handle.inner.lock().unwrap().snapshot.clone()
        }
    }

    struct Handles {
        screen: FakeSourceHandle,
        screen_encoder: FakeScreenEncoder,
        system_audio: FakeSourceHandle,
        mic: FakeSourceHandle,
        desktop_activity: FakeDesktopActivityHandle,
    }

    fn fake_sources(
        screen_state: SourceState,
        system_audio_state: SourceState,
        mic_state: SourceState,
    ) -> (Sources, Handles) {
        fake_sources_with_activity(
            screen_state,
            system_audio_state,
            mic_state,
            SourceState::Inactive,
        )
    }

    fn fake_sources_with_activity(
        screen_state: SourceState,
        system_audio_state: SourceState,
        mic_state: SourceState,
        desktop_activity_state: SourceState,
    ) -> (Sources, Handles) {
        let handles = Handles {
            screen: FakeSourceHandle::new(screen_state),
            screen_encoder: FakeScreenEncoder::default(),
            system_audio: FakeSourceHandle::new(system_audio_state),
            mic: FakeSourceHandle::new(mic_state),
            desktop_activity: FakeDesktopActivityHandle::new(desktop_activity_state),
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
            desktop_activity: Box::new(FakeDesktopActivity {
                handle: handles.desktop_activity.clone(),
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
        let (mut engine, _) =
            CaptureEngine::new(sources, config, &mut recovery, segment_fs, Box::new(clock))
                .unwrap();
        engine.set_source_choices(SetupSnapshot {
            mic: true,
            system_audio: true,
            screen: true,
            ..SetupSnapshot::default()
        });
        engine
    }

    fn breaker_open_engine() -> (CaptureEngine<FakeSegmentFs>, Handles) {
        let config = EngineConfig {
            lifecycle: BackoffConfig {
                breaker_threshold: 1,
                ..BackoffConfig::default()
            },
            ..EngineConfig::default()
        };
        let (sources, handles) = fake_sources(
            SourceState::Faulted {
                reason: ErrorReason::EndpointLost,
                detail: "gone".into(),
            },
            SourceState::Active,
            SourceState::NoInputDevice,
        );
        let mut engine = engine_with(FakeClock::new(0), FakeSegmentFs::default(), config, sources);
        engine.start();

        let dump = engine.health_dump();
        assert_eq!(dump.app_state, AppPhase::Error);
        assert!(fault_details_contain(&dump, BREAKER_OPEN_MARKER));

        (engine, handles)
    }

    fn fault_details_contain(dump: &HealthDump, needle: &str) -> bool {
        dump.sources.iter().any(|source| {
            matches!(
                &source.state,
                SourceState::Faulted { detail, .. } if detail.contains(needle)
            )
        })
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
                at_us: None,
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

        engine.set_source_choices(SetupSnapshot {
            mic: true,
            system_audio: true,
            screen: true,
            ..SetupSnapshot::default()
        });
        engine.start();
        assert_eq!(handles.screen.starts(), 1);
        assert_eq!(handles.system_audio.starts(), 1);
        assert_eq!(handles.mic.starts(), 1);
        engine.stop();
    }

    #[test]
    fn deferred_recovery_is_visible_then_existing_storage_retry_clears_it() {
        let (sources, _) = active_sources();
        let key = segment_for(0, DEFAULT_SEGMENT_SECS);
        let mut recovery = FakeRecoveryFs {
            stale: vec![StaleSegment {
                key,
                path: "stale.incomplete".into(),
                has_usable_data: true,
            }],
            fail_finalize: true,
            ..Default::default()
        };
        let segment_fs = FakeSegmentFs::default();
        let (mut engine, outcomes) = CaptureEngine::new(
            sources,
            EngineConfig::default(),
            &mut recovery,
            segment_fs.clone(),
            Box::new(FakeClock::new(1000)),
        )
        .unwrap();
        assert_eq!(outcomes, vec![RecoveryOutcome::Deferred(key)]);
        assert!(engine.health_dump().storage.is_some());
        assert!(engine.pending_seals.contains_key(&key));
        engine.pump();
        assert!(!engine.pending_seals.contains_key(&key));
        assert!(engine.health_dump().storage.is_none());
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

    /// An in-memory pause store shared with the test, standing in for the file.
    #[derive(Clone, Default)]
    struct MemoryPauseStore(Arc<Mutex<Option<HeldPause>>>);

    impl MemoryPauseStore {
        fn holding(pause: HeldPause) -> Self {
            Self(Arc::new(Mutex::new(Some(pause))))
        }
        fn held(&self) -> Option<HeldPause> {
            *self.0.lock().unwrap()
        }
    }

    impl PauseStore for MemoryPauseStore {
        fn load(&self) -> Option<HeldPause> {
            self.held()
        }
        fn save(&self, pause: HeldPause) -> io::Result<()> {
            *self.0.lock().unwrap() = Some(pause);
            Ok(())
        }
        fn clear(&self) -> io::Result<()> {
            *self.0.lock().unwrap() = None;
            Ok(())
        }
    }

    fn restarted_with(
        clock: FakeClock,
        store: &MemoryPauseStore,
    ) -> (CaptureEngine<FakeSegmentFs>, Handles) {
        let (sources, handles) = active_sources();
        let mut engine = engine_with(
            clock,
            FakeSegmentFs::default(),
            EngineConfig::default(),
            sources,
        );
        engine.hold_owner_pause_in(Box::new(store.clone()));
        engine.start();
        (engine, handles)
    }

    #[test]
    fn until_resumed_survives_a_restart_and_starts_nothing() {
        let store = MemoryPauseStore::default();
        let (mut engine, _) = restarted_with(FakeClock::new(1_000), &store);
        engine.apply_command(EngineCommand::Pause {
            reason: PauseReason::Operator,
            duration_secs: None,
        });
        assert_eq!(store.held(), Some(HeldPause::UntilResumed));
        engine.stop();

        let clock = FakeClock::new(90_000);
        let (mut engine, handles) = restarted_with(clock.clone(), &store);
        assert_eq!(engine.health_dump().app_state, AppPhase::Paused);
        assert_eq!(handles.screen.starts(), 0);
        assert_eq!(handles.system_audio.starts(), 0);
        assert_eq!(handles.mic.starts(), 0);
        assert!(engine.health_dump().segment_dir.is_none());
        let pause = engine.health_dump().pause.expect("paused");
        assert_eq!(pause.reason, PauseReason::Operator);
        assert_eq!(pause.seconds_remaining, None);
        clock.set(990_000);
        engine.pump();
        assert_eq!(engine.health_dump().app_state, AppPhase::Paused);

        engine.apply_command(EngineCommand::Resume);
        assert_eq!(handles.screen.starts(), 1);
        assert_eq!(engine.health_dump().app_state, AppPhase::Observing);
        assert_eq!(store.held(), None);
        let (engine, handles) = restarted_with(clock, &store);
        assert_eq!(engine.health_dump().app_state, AppPhase::Observing);
        assert_eq!(handles.screen.starts(), 1);
    }

    #[test]
    fn timed_pause_survives_a_restart_and_ends_at_its_deadline() {
        let store = MemoryPauseStore::default();
        let clock = FakeClock::new(1_000);
        let (mut engine, _) = restarted_with(clock.clone(), &store);
        engine.apply_command(EngineCommand::Pause {
            reason: PauseReason::Operator,
            duration_secs: Some(900),
        });
        assert_eq!(store.held(), Some(HeldPause::Until(1_900)));
        engine.stop();

        clock.set(1_600);
        let (mut engine, handles) = restarted_with(clock.clone(), &store);
        assert_eq!(engine.health_dump().app_state, AppPhase::Paused);
        assert_eq!(
            engine.health_dump().pause.unwrap().seconds_remaining,
            Some(300)
        );
        assert_eq!(handles.screen.starts(), 0);
        clock.set(1_899);
        engine.pump();
        assert_eq!(engine.health_dump().app_state, AppPhase::Paused);
        clock.set(1_900);
        engine.pump();
        assert_eq!(engine.health_dump().app_state, AppPhase::Observing);
        assert_eq!(handles.screen.starts(), 1);
        assert_eq!(store.held(), None);
    }

    #[test]
    fn timed_pause_restarted_after_its_deadline_observes() {
        let store = MemoryPauseStore::holding(HeldPause::Until(1_900));
        let (engine, handles) = restarted_with(FakeClock::new(1_900), &store);
        assert_eq!(engine.health_dump().app_state, AppPhase::Observing);
        assert_eq!(handles.screen.starts(), 1);
        assert_eq!(store.held(), None);
    }

    #[test]
    fn unlock_and_wake_do_not_end_the_owners_pause() {
        let store = MemoryPauseStore::default();
        let (mut engine, handles) = restarted_with(FakeClock::new(1_000), &store);
        engine.apply_command(EngineCommand::Pause {
            reason: PauseReason::Operator,
            duration_secs: None,
        });
        for reason in [PauseReason::SessionLocked, PauseReason::SystemSuspending] {
            engine.apply_command(EngineCommand::Pause {
                reason,
                duration_secs: None,
            });
            engine.apply_command(EngineCommand::SystemResume);
            assert_eq!(engine.health_dump().app_state, AppPhase::Paused);
            assert_eq!(
                engine.health_dump().pause.unwrap().reason,
                PauseReason::Operator
            );
        }
        assert_eq!(handles.screen.starts(), 1, "capture never restarted");
        assert_eq!(store.held(), Some(HeldPause::UntilResumed));
    }

    #[test]
    fn a_lock_pause_is_never_recorded_and_unlock_resumes() {
        let store = MemoryPauseStore::default();
        let (mut engine, handles) = restarted_with(FakeClock::new(1_000), &store);
        engine.apply_command(EngineCommand::Pause {
            reason: PauseReason::SessionLocked,
            duration_secs: None,
        });
        assert_eq!(engine.health_dump().app_state, AppPhase::Paused);
        assert_eq!(store.held(), None);
        engine.apply_command(EngineCommand::SystemResume);
        assert_eq!(engine.health_dump().app_state, AppPhase::Observing);
        assert_eq!(handles.screen.starts(), 2);
    }

    #[test]
    fn owner_deadline_while_locked_waits_for_unlock() {
        let store = MemoryPauseStore::default();
        let clock = FakeClock::new(1_000);
        let (mut engine, handles) = restarted_with(clock.clone(), &store);
        engine.apply_command(EngineCommand::Pause {
            reason: PauseReason::Operator,
            duration_secs: Some(900),
        });
        engine.apply_command(EngineCommand::Pause {
            reason: PauseReason::SessionLocked,
            duration_secs: None,
        });
        clock.set(1_900);
        engine.pump();
        assert_eq!(engine.health_dump().app_state, AppPhase::Paused);
        assert_eq!(handles.screen.starts(), 1, "no capture while locked");
        assert_eq!(store.held(), None, "the owner's pause has ended");
        engine.apply_command(EngineCommand::SystemResume);
        assert_eq!(engine.health_dump().app_state, AppPhase::Observing);
        assert_eq!(handles.screen.starts(), 2);
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
    fn refresh_health_carries_listener_faults_forward() {
        let (sources, _) = active_sources();
        let mut engine = engine_with(
            FakeClock::new(0),
            FakeSegmentFs::default(),
            EngineConfig::default(),
            sources,
        );
        engine.start();
        let handle = engine.health_handle();
        handle.lock().unwrap().listener_faults.health = Some("bind: error".into());
        handle.lock().unwrap().listener_faults.control = Some("serve: error".into());

        assert!(engine.health_dump().listener_faults.health.is_none());
        engine.refresh_health();

        assert_eq!(
            handle.lock().unwrap().listener_faults.health.as_deref(),
            Some("bind: error")
        );
        assert_eq!(
            engine
                .health_watch()
                .borrow()
                .listener_faults
                .control
                .as_deref(),
            Some("serve: error")
        );

        engine.apply_command(EngineCommand::Pause {
            reason: PauseReason::Operator,
            duration_secs: None,
        });
        assert_eq!(engine.health_watch().borrow().app_state, AppPhase::Paused);
        assert_eq!(
            engine
                .health_watch()
                .borrow()
                .listener_faults
                .health
                .as_deref(),
            Some("bind: error")
        );
        assert_eq!(
            engine
                .health_watch()
                .borrow()
                .listener_faults
                .control
                .as_deref(),
            Some("serve: error")
        );

        handle.lock().unwrap().listener_faults = observer_model::ListenerFaults::default();
        engine.apply_command(EngineCommand::Resume);
        assert_eq!(
            engine.health_watch().borrow().app_state,
            AppPhase::Observing
        );
        assert!(engine
            .health_watch()
            .borrow()
            .listener_faults
            .health
            .is_none());
        assert!(engine
            .health_watch()
            .borrow()
            .listener_faults
            .control
            .is_none());
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
    fn resume_from_breaker_open_restarts_sources_and_clears_marker() {
        let (mut engine, handles) = breaker_open_engine();
        let starts_before = (
            handles.screen.starts(),
            handles.system_audio.starts(),
            handles.mic.starts(),
        );

        handles.screen.set_state(SourceState::Active);
        engine.apply_command(EngineCommand::Resume);

        assert_eq!(handles.screen.starts(), starts_before.0 + 1);
        assert_eq!(handles.system_audio.starts(), starts_before.1 + 1);
        assert_eq!(handles.mic.starts(), starts_before.2 + 1);
        assert!(!fault_details_contain(
            &engine.health_dump(),
            BREAKER_OPEN_MARKER
        ));
    }

    #[test]
    fn resume_from_cleared_fault_rederives_observing() {
        let (mut engine, handles) = breaker_open_engine();

        handles.screen.set_state(SourceState::Active);
        engine.apply_command(EngineCommand::Resume);

        assert_eq!(engine.health_dump().app_state, AppPhase::Observing);
    }

    #[test]
    fn resume_with_persistent_fault_stays_error() {
        let (mut engine, handles) = breaker_open_engine();
        handles
            .screen
            .queue_start_error(SourceError::new(ErrorReason::EndpointLost, "still gone"));

        engine.apply_command(EngineCommand::Resume);

        let phase = engine.health_dump().app_state;
        assert_eq!(phase, AppPhase::Error);
        assert_ne!(phase, AppPhase::Starting);
        assert_ne!(phase, AppPhase::Observing);
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
            listener_faults: observer_model::ListenerFaults::default(),
            setup: SetupSnapshot::default(),
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

    #[test]
    fn all_off_opens_nothing_for_not_paired_and_paired_sync_snapshot() {
        let (sources, handles) = active_sources();
        let segment_fs = FakeSegmentFs::default();
        let clock = FakeClock::new(0);
        let mut recovery = FakeRecoveryFs::default();
        let (mut engine, _) = CaptureEngine::new(
            sources,
            EngineConfig::default(),
            &mut recovery,
            segment_fs.clone(),
            Box::new(clock),
        )
        .unwrap();

        engine.start();
        assert_eq!(handles.screen.starts(), 0);
        assert_eq!(handles.system_audio.starts(), 0);
        assert_eq!(handles.mic.starts(), 0);
        assert!(segment_fs.events().is_empty());
        assert_eq!(engine.health_dump().app_state, AppPhase::Idle);

        engine.sync_handle().lock().unwrap().pairing.phase = observer_model::PairingPhase::Paired;
        engine.pump();

        assert_eq!(handles.screen.starts(), 0);
        assert_eq!(handles.system_audio.starts(), 0);
        assert_eq!(handles.mic.starts(), 0);
        assert!(segment_fs.events().is_empty());
        assert_eq!(engine.health_dump().app_state, AppPhase::Idle);
    }

    #[test]
    fn one_enabled_source_starts_only_that_source() {
        let (sources, handles) = active_sources();
        let segment_fs = FakeSegmentFs::default();
        let clock = FakeClock::new(0);
        let mut recovery = FakeRecoveryFs::default();
        let (mut engine, _) = CaptureEngine::new(
            sources,
            EngineConfig::default(),
            &mut recovery,
            segment_fs.clone(),
            Box::new(clock),
        )
        .unwrap();

        engine.apply_command(EngineCommand::SetSourceEnabled {
            kind: SourceKind::SystemAudio,
            enabled: true,
        });
        engine.start();

        assert_eq!(handles.system_audio.starts(), 1);
        assert_eq!(handles.screen.starts(), 0);
        assert_eq!(handles.mic.starts(), 0);
        assert_eq!(
            segment_fs.events(),
            vec![FsEvent::Open(observer_segment::segment_for(
                0,
                DEFAULT_SEGMENT_SECS
            ))]
        );
        assert_eq!(engine.health_dump().app_state, AppPhase::Observing);
    }

    #[test]
    fn queued_event_and_late_callback_after_off_are_not_written_and_re_enable_does_not_write_them()
    {
        let (sources, handles) = active_sources();
        let segment_fs = FakeSegmentFs::default();
        let clock = FakeClock::new(0);
        let mut engine = engine_with(clock, segment_fs.clone(), EngineConfig::default(), sources);
        engine.start();

        let source_sink = handles
            .system_audio
            .last_sink()
            .expect("source stored sink at start");

        source_sink.emit(CaptureChunk {
            source: SourceKind::SystemAudio,
            seq: 1,
            data: vec![1, 2],
            format: None,
            at_us: Some(1_000),
        });
        engine.pump();
        assert_eq!(segment_fs.writes().len(), 1);

        engine.apply_command(EngineCommand::SetSourceEnabled {
            kind: SourceKind::SystemAudio,
            enabled: false,
        });

        source_sink.emit(CaptureChunk {
            source: SourceKind::SystemAudio,
            seq: 2,
            data: vec![3, 4],
            format: None,
            at_us: Some(2_000),
        });
        engine.pump();
        assert_eq!(segment_fs.writes().len(), 1);

        engine.apply_command(EngineCommand::SetSourceEnabled {
            kind: SourceKind::SystemAudio,
            enabled: true,
        });

        source_sink.emit(CaptureChunk {
            source: SourceKind::SystemAudio,
            seq: 3,
            data: vec![5, 6],
            format: None,
            at_us: Some(3_000),
        });
        engine.pump();
        assert_eq!(segment_fs.writes().len(), 1);

        let new_sink = handles
            .system_audio
            .last_sink()
            .expect("new sink stored on re-enable");
        new_sink.emit(CaptureChunk {
            source: SourceKind::SystemAudio,
            seq: 4,
            data: vec![7, 8],
            format: None,
            at_us: Some(4_000),
        });
        engine.pump();
        assert_eq!(segment_fs.writes().len(), 2);
    }

    #[test]
    fn an_already_written_chunk_still_seals() {
        let (sources, handles) = active_sources();
        let segment_fs = FakeSegmentFs::default();
        let clock = FakeClock::new(0);
        let mut engine = engine_with(
            clock.clone(),
            segment_fs.clone(),
            EngineConfig::default(),
            sources,
        );
        engine.start();

        let sink = handles.system_audio.last_sink().unwrap();
        sink.emit(CaptureChunk {
            source: SourceKind::SystemAudio,
            seq: 1,
            data: vec![1, 2],
            format: None,
            at_us: Some(500),
        });
        engine.pump();
        assert_eq!(segment_fs.writes().len(), 1);

        engine.apply_command(EngineCommand::SetSourceEnabled {
            kind: SourceKind::Screen,
            enabled: false,
        });
        engine.apply_command(EngineCommand::SetSourceEnabled {
            kind: SourceKind::SystemAudio,
            enabled: false,
        });
        engine.apply_command(EngineCommand::SetSourceEnabled {
            kind: SourceKind::Mic,
            enabled: false,
        });
        engine.pump();

        let key = observer_segment::segment_for(0, DEFAULT_SEGMENT_SECS);
        assert!(!segment_fs.events().contains(&FsEvent::Finalize(key)));
        assert_eq!(segment_fs.writes().len(), 1);

        clock.advance(DEFAULT_SEGMENT_SECS);
        engine.pump();

        assert!(segment_fs.events().contains(&FsEvent::Finalize(key)));
        assert_eq!(segment_fs.writes().len(), 1);
    }

    #[test]
    fn stop_seals_an_open_all_off_segment_without_boundary_cross() {
        let (sources, handles) = active_sources();
        let segment_fs = FakeSegmentFs::default();
        let clock = FakeClock::new(0);
        let mut engine = engine_with(clock, segment_fs.clone(), EngineConfig::default(), sources);
        engine.start();

        let sink = handles.system_audio.last_sink().unwrap();
        sink.emit(CaptureChunk {
            source: SourceKind::SystemAudio,
            seq: 1,
            data: vec![1, 2],
            format: None,
            at_us: Some(500),
        });
        engine.pump();
        assert_eq!(segment_fs.writes().len(), 1);

        engine.apply_command(EngineCommand::SetSourceEnabled {
            kind: SourceKind::Screen,
            enabled: false,
        });
        engine.apply_command(EngineCommand::SetSourceEnabled {
            kind: SourceKind::SystemAudio,
            enabled: false,
        });
        engine.apply_command(EngineCommand::SetSourceEnabled {
            kind: SourceKind::Mic,
            enabled: false,
        });
        engine.pump();

        let key = observer_segment::segment_for(0, DEFAULT_SEGMENT_SECS);
        assert!(!segment_fs.events().contains(&FsEvent::Finalize(key)));

        engine.stop();
        assert!(segment_fs.events().contains(&FsEvent::Finalize(key)));
        assert_eq!(segment_fs.writes().len(), 1);
    }

    #[test]
    fn the_other_source_keeps_running() {
        let (sources, handles) = active_sources();
        let segment_fs = FakeSegmentFs::default();
        let clock = FakeClock::new(0);
        let mut engine = engine_with(clock, segment_fs.clone(), EngineConfig::default(), sources);
        engine.start();

        engine.apply_command(EngineCommand::SetSourceEnabled {
            kind: SourceKind::SystemAudio,
            enabled: false,
        });
        engine.pump();

        let screen_sink = handles.screen.last_sink().unwrap();
        screen_sink.emit_screen_frame(ScreenFrame {
            seq: 1,
            arrival_100ns: 10_000,
            width: 100,
            height: 100,
            pixel_format: ScreenPixelFormat::Rgba8,
            pixels: Arc::from(vec![0u8; 100 * 100 * 4]),
        });
        engine.pump();

        assert_eq!(handles.screen_encoder.frames_consumed(), 1);
        assert_eq!(engine.health_dump().app_state, AppPhase::Observing);
    }

    #[test]
    fn resume_system_resume_display_change_and_retry_obey_choices() {
        let (sources, handles) = active_sources();
        let segment_fs = FakeSegmentFs::default();
        let clock = FakeClock::new(0);
        let mut engine = engine_with(clock.clone(), segment_fs, EngineConfig::default(), sources);
        engine.start();

        engine.apply_command(EngineCommand::SetSourceEnabled {
            kind: SourceKind::Screen,
            enabled: false,
        });
        engine.pump();

        let screen_starts = handles.screen.starts();
        let audio_starts = handles.system_audio.starts();

        engine.apply_command(EngineCommand::Pause {
            reason: PauseReason::Operator,
            duration_secs: None,
        });
        engine.pump();
        assert_eq!(engine.health_dump().app_state, AppPhase::Paused);

        engine.apply_command(EngineCommand::Resume);
        engine.pump();
        assert_eq!(handles.screen.starts(), screen_starts);
        assert_eq!(handles.system_audio.starts(), audio_starts + 1);

        engine.apply_command(EngineCommand::Pause {
            reason: PauseReason::SessionLocked,
            duration_secs: None,
        });
        engine.pump();
        engine.apply_command(EngineCommand::SystemResume);
        engine.pump();
        assert_eq!(handles.screen.starts(), screen_starts);
        assert_eq!(handles.system_audio.starts(), audio_starts + 2);

        let display_changes = handles.screen.display_changes();
        engine.apply_command(EngineCommand::DisplayChanged);
        assert_eq!(handles.screen.display_changes(), display_changes);
    }

    #[test]
    fn set_source_enabled_during_pause_does_not_clear_pause_and_does_not_start() {
        let (sources, handles) = active_sources();
        let segment_fs = FakeSegmentFs::default();
        let clock = FakeClock::new(0);
        let mut engine = engine_with(clock, segment_fs, EngineConfig::default(), sources);
        engine.start();

        engine.apply_command(EngineCommand::SetSourceEnabled {
            kind: SourceKind::Screen,
            enabled: false,
        });
        engine.pump();

        engine.apply_command(EngineCommand::Pause {
            reason: PauseReason::Operator,
            duration_secs: None,
        });
        engine.pump();
        assert_eq!(engine.health_dump().app_state, AppPhase::Paused);

        let screen_starts = handles.screen.starts();

        engine.apply_command(EngineCommand::SetSourceEnabled {
            kind: SourceKind::Screen,
            enabled: true,
        });
        engine.pump();

        assert_eq!(engine.health_dump().app_state, AppPhase::Paused);
        assert_eq!(handles.screen.starts(), screen_starts);
    }

    #[test]
    fn write_chunk_rewrites_at_us_to_segment_relative() {
        let (sources, handles) = active_sources();
        let segment_fs = FakeSegmentFs::default();
        let clock = FakeClock::new(0);
        clock.set_monotonic_us(5_000_000);
        let mut engine = engine_with(clock, segment_fs.clone(), EngineConfig::default(), sources);
        engine.start();

        let sink = handles.system_audio.last_sink().unwrap();
        sink.emit(CaptureChunk {
            source: SourceKind::SystemAudio,
            seq: 1,
            data: vec![1, 2],
            format: None,
            at_us: Some(5_250_000),
        });
        engine.pump();

        let writes = segment_fs.writes_with_at_us();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].3, Some(250_000));
    }

    fn temp_test_dir(label: &str) -> std::path::PathBuf {
        let base = std::path::PathBuf::from("/var/tmp/solstone-test").join(format!(
            "{}-{}-{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn set_source_choices_fresh_and_legacy_preserved_across_pump() {
        let (sources, _handles) = active_sources();
        let segment_fs = FakeSegmentFs::default();
        let clock = FakeClock::new(0);
        let mut engine = engine_with(clock, segment_fs, EngineConfig::default(), sources);

        let fresh = SetupSnapshot {
            screen: false,
            system_audio: false,
            mic: false,
            wizard: observer_model::setup::WizardStep::Mic,
            notice_dismissed: true,
            start_at_login: observer_model::setup::LoginPreference::explicit(false),
            login_apply_failed: true,
            ..Default::default()
        };
        engine.set_source_choices(fresh);
        engine.pump();
        let dump = engine.health_dump();
        assert_eq!(dump.setup.wizard, observer_model::setup::WizardStep::Mic);
        assert!(dump.setup.notice_dismissed);
        assert_eq!(
            dump.setup.start_at_login,
            observer_model::setup::LoginPreference::explicit(false)
        );
        assert!(dump.setup.login_apply_failed);

        let legacy = SetupSnapshot {
            screen: true,
            system_audio: true,
            mic: true,
            wizard: observer_model::setup::WizardStep::Done,
            notice_dismissed: false,
            start_at_login: observer_model::setup::LoginPreference::explicit(true),
            login_apply_failed: true,
            ..Default::default()
        };
        engine.set_source_choices(legacy);
        engine.pump();
        let dump2 = engine.health_dump();
        assert_eq!(dump2.setup.wizard, observer_model::setup::WizardStep::Done);
        assert!(!dump2.setup.notice_dismissed);
        assert_eq!(
            dump2.setup.start_at_login,
            observer_model::setup::LoginPreference::explicit(true)
        );
        assert!(dump2.setup.login_apply_failed);
    }

    #[test]
    fn authority_set_source_off_publishes_projection_and_preserves_in_file() {
        let root = temp_test_dir("authority-temp-root");
        let (sources, _handles) = active_sources();
        let segment_fs = FakeSegmentFs::default();
        let clock = FakeClock::new(0);
        let mut engine = engine_with(clock, segment_fs, EngineConfig::default(), sources);
        engine.set_setup_root(root.clone());

        let initial = observer_model::setup::SetupConfig::legacy();
        observer_model::setup::atomic_write_setup(&root, &initial).unwrap();
        engine.set_source_choices(initial.to_snapshot());

        let auth = engine.setup_authority();
        let running = auth.lock().unwrap().set_source(SourceKind::Mic, false);
        assert!(!running);

        // Pump before set_source_enabled publishes setup.mic == false
        engine.pump();
        assert!(!engine.health_dump().setup.mic);

        // A following set_wizard_step / set_start_at_login / other set_source leaves mic false in file and load_or_classify
        assert!(auth
            .lock()
            .unwrap()
            .set_wizard_step(observer_model::setup::WizardStep::SystemAudio));
        assert!(auth.lock().unwrap().set_start_at_login(false));
        assert!(auth.lock().unwrap().set_source(SourceKind::Screen, true));

        match observer_model::setup::load_or_classify(&root) {
            observer_model::setup::LoadOutcome::Valid(cfg) => {
                assert!(!cfg.mic);
                assert!(cfg.screen);
                assert_eq!(
                    cfg.start_at_login,
                    observer_model::setup::LoginPreference::explicit(false)
                );
                assert_eq!(cfg.wizard, observer_model::setup::WizardStep::SystemAudio);
            }
            other => panic!("expected Valid, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn two_failed_offs_stay_stopped_across_actions_and_retry_writes_without_extra_starts() {
        let root = temp_test_dir("engine-two-failed-offs");
        let (sources, handles) = active_sources();
        let segment_fs = FakeSegmentFs::default();
        let clock = FakeClock::new(0);
        let mut engine = engine_with(clock, segment_fs, EngineConfig::default(), sources);
        engine.set_setup_root(root.clone());

        let initial = observer_model::setup::SetupConfig::legacy();
        observer_model::setup::atomic_write_setup(&root, &initial).unwrap();
        engine.set_source_choices(initial.to_snapshot());
        engine.start();

        let mic_starts = handles.mic.starts();
        let screen_starts = handles.screen.starts();

        std::fs::create_dir(root.join("setup.json.tmp")).unwrap();

        let auth = engine.setup_authority();
        let mic_running = auth.lock().unwrap().set_source(SourceKind::Mic, false);
        assert!(!mic_running);
        engine.apply_command(EngineCommand::SetSourceEnabled {
            kind: SourceKind::Mic,
            enabled: false,
        });

        let screen_running = auth.lock().unwrap().set_source(SourceKind::Screen, false);
        assert!(!screen_running);
        engine.apply_command(EngineCommand::SetSourceEnabled {
            kind: SourceKind::Screen,
            enabled: false,
        });

        engine.pump();
        assert!(!engine.state().is_source_enabled(SourceKind::Mic));
        assert!(!engine.state().is_source_enabled(SourceKind::Screen));

        auth.lock()
            .unwrap()
            .set_wizard_step(observer_model::setup::WizardStep::SystemAudio);
        auth.lock().unwrap().set_start_at_login(false);
        auth.lock().unwrap().dismiss_notice();
        auth.lock()
            .unwrap()
            .set_source(SourceKind::SystemAudio, true);

        engine.pump();
        assert!(!engine.state().is_source_enabled(SourceKind::Mic));
        assert!(!engine.state().is_source_enabled(SourceKind::Screen));

        std::fs::remove_dir(root.join("setup.json.tmp")).unwrap();

        let outcome = auth.lock().unwrap().retry();
        assert!(!outcome.started.contains(&SourceKind::Mic));
        assert!(!outcome.started.contains(&SourceKind::Screen));
        for kind in outcome.started {
            engine.apply_command(EngineCommand::SetSourceEnabled {
                kind,
                enabled: true,
            });
        }
        engine.pump();

        assert_eq!(handles.mic.starts(), mic_starts);
        assert_eq!(handles.screen.starts(), screen_starts);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn proof_consumer_gate_generation_and_clearing() {
        let (sources, handles) = active_sources();
        handles.mic.set_state(SourceState::Active);
        let segment_fs = FakeSegmentFs::default();
        let clock = FakeClock::new(0);
        let mut engine = engine_with(clock, segment_fs.clone(), EngineConfig::default(), sources);
        engine.start();

        let gen1 = engine.open_proof_consumer(SourceKind::Mic);
        assert!(gen1 > 0);

        let sink = handles.mic.last_sink().unwrap();
        sink.emit(CaptureChunk {
            source: SourceKind::Mic,
            seq: 1,
            data: vec![0, 0, 0, 0],
            format: Some(observer_model::AudioFormat {
                sample_rate_hz: 48_000,
                channels: 1,
                bits_per_sample: 16,
                is_float: false,
            }),
            at_us: Some(100_000),
        });
        engine.pump();

        let snap = engine.preview_snapshot();
        assert_eq!(snap.consumer_generation, gen1);
        assert_eq!(snap.mic_level, Some(0.0));

        let gen2 = engine.close_proof_consumer();
        assert!(gen2 > gen1);

        sink.emit(CaptureChunk {
            source: SourceKind::Mic,
            seq: 2,
            data: vec![0, 0, 0, 0],
            format: Some(observer_model::AudioFormat {
                sample_rate_hz: 48_000,
                channels: 1,
                bits_per_sample: 16,
                is_float: false,
            }),
            at_us: Some(300_000),
        });

        let sys_sink = handles.system_audio.last_sink().unwrap();
        sys_sink.emit(CaptureChunk {
            source: SourceKind::SystemAudio,
            seq: 1,
            data: vec![1, 2, 3, 4],
            format: None,
            at_us: Some(350_000),
        });
        engine.pump();

        let snap_closed = engine.preview_snapshot();
        assert_eq!(snap_closed.mic_level, None);
        assert!(segment_fs
            .writes()
            .iter()
            .any(|(_, kind, _)| *kind == SourceKind::SystemAudio));

        let gen3 = engine.open_proof_consumer(SourceKind::Mic);
        assert!(gen3 > gen2);
        let snap_reopened = engine.preview_snapshot();
        assert_eq!(snap_reopened.consumer_generation, gen3);
        assert_eq!(snap_reopened.mic_level, None);

        sink.emit(CaptureChunk {
            source: SourceKind::Mic,
            seq: 3,
            data: vec![0, 0, 0, 0],
            format: Some(observer_model::AudioFormat {
                sample_rate_hz: 48_000,
                channels: 1,
                bits_per_sample: 16,
                is_float: false,
            }),
            at_us: Some(600_000),
        });
        engine.pump();

        let snap3 = engine.preview_snapshot();
        assert_eq!(snap3.consumer_generation, gen3);
        assert_eq!(snap3.mic_level, Some(0.0));
    }

    #[test]
    fn queued_media_is_not_relabelled_as_proof_after_consumer_reopens() {
        let (sources, handles) = active_sources();
        handles.mic.set_state(SourceState::Active);
        let fs = FakeSegmentFs::default();
        let mut engine = engine_with(
            FakeClock::new(0),
            fs.clone(),
            EngineConfig::default(),
            sources,
        );
        engine.start();
        engine.open_proof_consumer(SourceKind::Mic);
        let sink = handles.mic.last_sink().unwrap();
        emit_audio(&sink, SourceKind::Mic, 1..2);
        engine.close_proof_consumer();
        let generation = engine.open_proof_consumer(SourceKind::Mic);
        engine.pump();
        assert_eq!(engine.preview_snapshot().mic_level, None);
        assert_eq!(
            fs.writes().len(),
            1,
            "authorized media still reaches storage"
        );
        emit_audio(&sink, SourceKind::Mic, 2..3);
        engine.pump();
        assert_eq!(fs.writes().len(), 2);
        assert_eq!(engine.preview_snapshot().consumer_generation, generation);
        assert!(engine.preview_snapshot().mic_level.is_some());
    }

    #[test]
    fn device_stream_recovery_before_pump_rejects_old_queue_and_accepts_new_input() {
        let (sources, handles) = active_sources();
        let fs = FakeSegmentFs::default();
        let mut engine = engine_with(
            FakeClock::new(0),
            fs.clone(),
            EngineConfig::default(),
            sources,
        );
        engine.start();
        engine.open_proof_consumer(SourceKind::Mic);
        let sink = handles.mic.last_sink().unwrap();
        emit_audio(&sink, SourceKind::Mic, 1..2);
        handles.mic.set_state(SourceState::NoInputDevice);
        // The producer reselects a device through the same controller and sink.
        sink.begin_stream();
        handles.mic.set_state(SourceState::Active);
        emit_audio(&sink, SourceKind::Mic, 2..3);
        engine.pump();
        let written: Vec<_> = fs.writes().iter().map(|(_, _, seq)| *seq).collect();
        assert_eq!(written, vec![2]);
        assert!(engine.preview_snapshot().mic_level.is_some());
        assert_eq!(handles.mic.starts(), 1);
    }

    #[test]
    fn screen_stream_replacement_rejects_old_frames_and_accepts_new_proof() {
        let (sources, handles) = active_sources();
        let mut engine = engine_with(
            FakeClock::new(0),
            FakeSegmentFs::default(),
            EngineConfig::default(),
            sources,
        );
        engine.start();
        engine.open_proof_consumer(SourceKind::Screen);
        let sink = handles.screen.last_sink().unwrap();
        emit_screen(&sink, 1..2);
        sink.begin_stream();
        engine.pump();
        assert_eq!(handles.screen_encoder.frames_consumed(), 0);
        assert!(engine.preview_snapshot().screen_frame.is_none());
        emit_screen(&sink, 2..3);
        engine.pump();
        assert_eq!(handles.screen_encoder.frames_consumed(), 1);
        assert!(engine.preview_snapshot().screen_frame.is_some());
    }

    #[test]
    fn inactive_or_no_input_device_clears_level_and_drops_chunk_without_gen_bump() {
        let (sources, handles) = active_sources();
        handles.mic.set_state(SourceState::Active);
        let segment_fs = FakeSegmentFs::default();
        let clock = FakeClock::new(0);
        let mut engine = engine_with(clock, segment_fs.clone(), EngineConfig::default(), sources);
        engine.start();

        engine.open_proof_consumer(SourceKind::Mic);
        let sink = handles.mic.last_sink().unwrap();
        sink.emit(CaptureChunk {
            source: SourceKind::Mic,
            seq: 1,
            data: vec![10, 0, 10, 0],
            format: Some(observer_model::AudioFormat {
                sample_rate_hz: 48_000,
                channels: 1,
                bits_per_sample: 16,
                is_float: false,
            }),
            at_us: Some(100_000),
        });
        engine.pump();
        assert!(engine.preview_snapshot().mic_level.is_some());
        let initial_writes = segment_fs.writes().len();
        let initial_starts = handles.mic.starts();

        // Inactive clears level and chunk emitted before pump is not written
        handles.mic.set_state(SourceState::Inactive);
        sink.emit(CaptureChunk {
            source: SourceKind::Mic,
            seq: 2,
            data: vec![20, 0, 20, 0],
            format: Some(observer_model::AudioFormat {
                sample_rate_hz: 48_000,
                channels: 1,
                bits_per_sample: 16,
                is_float: false,
            }),
            at_us: Some(300_000),
        });
        engine.pump();

        assert_eq!(engine.preview_snapshot().mic_level, None);
        assert_eq!(segment_fs.writes().len(), initial_writes);

        // Active restores writing and preview
        handles.mic.set_state(SourceState::Active);
        sink.emit(CaptureChunk {
            source: SourceKind::Mic,
            seq: 3,
            data: vec![30, 0, 30, 0],
            format: Some(observer_model::AudioFormat {
                sample_rate_hz: 48_000,
                channels: 1,
                bits_per_sample: 16,
                is_float: false,
            }),
            at_us: Some(500_000),
        });
        engine.pump();

        assert_eq!(segment_fs.writes().len(), initial_writes + 1);
        assert!(engine.preview_snapshot().mic_level.is_some());
        assert_eq!(handles.mic.starts(), initial_starts);

        // NoInputDevice clears level as well
        handles.mic.set_state(SourceState::NoInputDevice);
        sink.emit(CaptureChunk {
            source: SourceKind::Mic,
            seq: 4,
            data: vec![40, 0, 40, 0],
            format: Some(observer_model::AudioFormat {
                sample_rate_hz: 48_000,
                channels: 1,
                bits_per_sample: 16,
                is_float: false,
            }),
            at_us: Some(700_000),
        });
        engine.pump();

        assert_eq!(engine.preview_snapshot().mic_level, None);
        assert_eq!(segment_fs.writes().len(), initial_writes + 1);
        assert_eq!(handles.mic.starts(), initial_starts);
    }

    #[test]
    fn closing_consumer_does_not_change_source_bits_pause_or_sealed_media() {
        let (sources, handles) = active_sources();
        let segment_fs = FakeSegmentFs::default();
        let clock = FakeClock::new(0);
        let mut engine = engine_with(
            clock.clone(),
            segment_fs.clone(),
            EngineConfig::default(),
            sources,
        );
        engine.start();

        let sink = handles.system_audio.last_sink().unwrap();
        sink.emit(CaptureChunk {
            source: SourceKind::SystemAudio,
            seq: 1,
            data: vec![1, 2],
            format: None,
            at_us: Some(100),
        });
        engine.pump();
        clock.advance(DEFAULT_SEGMENT_SECS);
        engine.pump();

        let key = observer_segment::segment_for(0, DEFAULT_SEGMENT_SECS);
        assert!(segment_fs.events().contains(&FsEvent::Finalize(key)));
        let finalize_count = segment_fs
            .events()
            .iter()
            .filter(|e| matches!(e, FsEvent::Finalize(_)))
            .count();

        engine.apply_command(EngineCommand::Pause {
            reason: PauseReason::Operator,
            duration_secs: None,
        });
        engine.pump();

        assert_eq!(engine.health_dump().app_state, AppPhase::Paused);
        assert!(engine.state().is_source_enabled(SourceKind::Mic));

        engine.open_proof_consumer(SourceKind::Mic);
        engine.close_proof_consumer();

        // Source bits unchanged
        assert!(engine.state().is_source_enabled(SourceKind::Mic));
        assert!(engine.state().is_source_enabled(SourceKind::Screen));
        assert!(engine.state().is_source_enabled(SourceKind::SystemAudio));

        // Held pause unchanged
        assert_eq!(engine.health_dump().app_state, AppPhase::Paused);

        // Sealed media unchanged
        let finalize_count_after = segment_fs
            .events()
            .iter()
            .filter(|e| matches!(e, FsEvent::Finalize(_)))
            .count();
        assert_eq!(finalize_count, finalize_count_after);
    }

    #[test]
    fn activity_only_opens_segment_and_does_not_start_media() {
        let (sources, handles) = fake_sources_with_activity(
            SourceState::Inactive,
            SourceState::Inactive,
            SourceState::Inactive,
            SourceState::Active,
        );
        let segment_fs = FakeSegmentFs::default();
        let clock = FakeClock::new(0);
        let mut recovery = FakeRecoveryFs::default();
        let (mut engine, _) = CaptureEngine::new(
            sources,
            EngineConfig::default(),
            &mut recovery,
            segment_fs.clone(),
            Box::new(clock),
        )
        .unwrap();
        engine.set_source_choices(SetupSnapshot {
            mic: false,
            system_audio: false,
            screen: false,
            desktop_activity: true,
            ..SetupSnapshot::default()
        });
        engine.start();
        engine.pump();

        assert_eq!(handles.screen.starts(), 0);
        assert_eq!(handles.system_audio.starts(), 0);
        assert_eq!(handles.mic.starts(), 0);
        assert_eq!(handles.desktop_activity.starts(), 1);

        let key = observer_segment::segment_for(0, DEFAULT_SEGMENT_SECS);
        assert!(segment_fs.events().contains(&FsEvent::Open(key)));
    }

    #[test]
    fn all_four_off_creates_no_segment() {
        let (sources, handles) = fake_sources_with_activity(
            SourceState::Inactive,
            SourceState::Inactive,
            SourceState::Inactive,
            SourceState::Inactive,
        );
        let segment_fs = FakeSegmentFs::default();
        let clock = FakeClock::new(0);
        let mut recovery = FakeRecoveryFs::default();
        let (mut engine, _) = CaptureEngine::new(
            sources,
            EngineConfig::default(),
            &mut recovery,
            segment_fs.clone(),
            Box::new(clock),
        )
        .unwrap();
        engine.set_source_choices(SetupSnapshot {
            mic: false,
            system_audio: false,
            screen: false,
            desktop_activity: false,
            ..SetupSnapshot::default()
        });
        engine.start();
        engine.pump();

        assert_eq!(handles.screen.starts(), 0);
        assert_eq!(handles.system_audio.starts(), 0);
        assert_eq!(handles.mic.starts(), 0);
        assert_eq!(handles.desktop_activity.starts(), 0);
        assert_eq!(segment_fs.events(), Vec::new());
    }
}
