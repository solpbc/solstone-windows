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
    AppPhase, CaptureChunk, CaptureSink, Clock, ErrorReason, HealthDump, MicSource, ScreenSource,
    SourceError, SourceKind, SourceReport, SourceState, SystemAudioSource,
};
use observer_recovery::{recover_all, RecoveryFs, RecoveryOutcome};
use observer_segment::{
    seconds_until_next_boundary, segment_for, should_rotate, SegmentFs, DEFAULT_SEGMENT_SECS,
};
use observer_state::{reduce, AppEvent, StateMachine};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};

pub const BREAKER_OPEN_MARKER: &str = "[breaker-open] ";

/// Engine-level errors. Source start failures are folded into honest state and
/// do not abort `start`; segment fs failures are infrastructural and do abort.
#[derive(Debug)]
pub enum EngineError<SegmentError: core::fmt::Debug> {
    Segment(SegmentError),
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

struct EngineSink {
    tx: mpsc::UnboundedSender<CaptureChunk>,
}

impl CaptureSink for EngineSink {
    fn emit(&self, chunk: CaptureChunk) {
        let _ = self.tx.send(chunk);
    }
}

struct OpenSegment {
    key: observer_model::SegmentKey,
    dir: String,
    opened_epoch_secs: u64,
    screen_chunks: u64,
}

/// The concrete platform sources injected into the engine. `capture-engine`
/// never names the platform crates; the binary constructs these and hands them
/// in, keeping the engine Tauri- and Windows-agnostic.
pub struct Sources {
    pub screen: Box<dyn ScreenSource>,
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
    rx: mpsc::UnboundedReceiver<CaptureChunk>,
    sink: Arc<EngineSink>,
    source_reports: BTreeMap<SourceKind, SourceReport>,
    lifecycles: BTreeMap<SourceKind, Lifecycle>,
    retry_at_epoch_secs: BTreeMap<SourceKind, u64>,
    shared_health: Arc<Mutex<HealthDump>>,
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
            shared_health: Arc::new(Mutex::new(Self::empty_health())),
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

        HealthDump {
            app_state,
            sources: self.source_reports.values().cloned().collect(),
            frame_rate,
            segment_dir: self.current_segment.as_ref().map(|s| s.dir.clone()),
            segment_seconds_remaining,
            engine_ready: self.state.engine_ready(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// Start every source and open the current segment. Source start failures are
    /// folded into state and do not abort the engine.
    pub fn start(&mut self) -> Result<(), EngineError<SFS::Error>> {
        self.reduce(AppEvent::RequestedStart);
        self.open_current_segment()?;
        for kind in [SourceKind::Screen, SourceKind::SystemAudio, SourceKind::Mic] {
            self.start_source(kind);
        }
        self.fold_source_states();
        self.refresh_health();
        Ok(())
    }

    /// Stop every source and seal the current segment, when one is open.
    pub fn stop(&mut self) -> Result<(), EngineError<SFS::Error>> {
        self.sources.screen.stop();
        self.sources.system_audio.stop();
        self.sources.mic.stop();

        if let Some(segment) = self.current_segment.take() {
            self.segment_fs
                .finalize(segment.key)
                .map_err(EngineError::Segment)?;
        }
        self.fold_source_states();
        self.refresh_health();
        Ok(())
    }

    /// Forward display changes to the screen source. Segment rotation stays
    /// clock-driven.
    pub fn on_display_changed(&mut self) {
        self.sources.screen.on_display_changed();
    }

    /// Deterministic, host-testable unit of engine work.
    pub fn pump(&mut self) -> Result<(), EngineError<SFS::Error>> {
        self.drain_chunks()?;
        self.rotate_if_needed()?;
        self.retry_due_sources();
        self.fold_source_states();
        self.refresh_health();
        Ok(())
    }

    /// Production loop. Shutdown exits cleanly; source facts and health are
    /// refreshed on a one-second tick.
    pub async fn run(
        &mut self,
        mut shutdown: oneshot::Receiver<()>,
    ) -> Result<(), EngineError<SFS::Error>> {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = &mut shutdown => {
                    return Ok(());
                }
                chunk = self.rx.recv() => {
                    if let Some(chunk) = chunk {
                        self.write_chunk(chunk)?;
                    } else {
                        return Ok(());
                    }
                }
                _ = interval.tick() => {
                    self.rotate_if_needed()?;
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

    fn open_current_segment(&mut self) -> Result<(), EngineError<SFS::Error>> {
        if self.current_segment.is_some() {
            return Ok(());
        }
        let now = self.clock.now_epoch_secs();
        let key = segment_for(now, self.config.segment_secs);
        let dir = self
            .segment_fs
            .open_incomplete(key)
            .map_err(EngineError::Segment)?;
        self.current_segment = Some(OpenSegment {
            key,
            dir,
            opened_epoch_secs: now,
            screen_chunks: 0,
        });
        Ok(())
    }

    fn drain_chunks(&mut self) -> Result<(), EngineError<SFS::Error>> {
        while let Ok(chunk) = self.rx.try_recv() {
            self.write_chunk(chunk)?;
        }
        Ok(())
    }

    fn write_chunk(&mut self, chunk: CaptureChunk) -> Result<(), EngineError<SFS::Error>> {
        let Some(segment) = self.current_segment.as_mut() else {
            return Ok(());
        };

        if let Err(error) = self.segment_fs.write_chunk(segment.key, &chunk) {
            let detail = format!("{error:?}");
            self.apply_source_report(SourceReport {
                kind: chunk.source,
                state: SourceState::Faulted {
                    reason: ErrorReason::WriteFailed,
                    detail,
                },
                device: None,
            });
            return Err(EngineError::Segment(error));
        }

        if chunk.source == SourceKind::Screen {
            segment.screen_chunks = segment.screen_chunks.saturating_add(1);
        }
        Ok(())
    }

    fn rotate_if_needed(&mut self) -> Result<(), EngineError<SFS::Error>> {
        let now = self.clock.now_epoch_secs();
        let Some(current) = self.current_segment.as_ref() else {
            return Ok(());
        };
        if !should_rotate(current.key, now, self.config.segment_secs) {
            return Ok(());
        }

        let old_key = current.key;
        self.segment_fs
            .finalize(old_key)
            .map_err(EngineError::Segment)?;

        let next_key = segment_for(now, self.config.segment_secs);
        let dir = self
            .segment_fs
            .open_incomplete(next_key)
            .map_err(EngineError::Segment)?;
        self.current_segment = Some(OpenSegment {
            key: next_key,
            dir,
            opened_epoch_secs: now,
            screen_chunks: 0,
        });
        Ok(())
    }

    fn fold_source_states(&mut self) {
        let reports = [
            SourceReport {
                kind: SourceKind::Screen,
                state: self.sources.screen.state(),
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
        let dump = self.health_dump();
        if let Ok(mut shared) = self.shared_health.lock() {
            *shared = dump;
        }
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

    fn empty_health() -> HealthDump {
        HealthDump {
            app_state: AppPhase::Idle,
            sources: Vec::new(),
            frame_rate: None,
            segment_dir: None,
            segment_seconds_remaining: None,
            engine_ready: false,
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

/// Minimal loopback-only HTTP health responder. The caller owns binding and must
/// bind to 127.0.0.1, never a wildcard address.
pub async fn serve_health(listener: TcpListener, dump: Arc<Mutex<HealthDump>>) -> io::Result<()> {
    loop {
        let (mut stream, _) = listener.accept().await?;
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeSet, VecDeque};
    use std::sync::atomic::{AtomicU64, Ordering};

    use observer_model::{ErrorReason, SegmentKey};
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
    }

    #[derive(Default)]
    struct FakeSegmentState {
        events: Vec<FsEvent>,
        writes: Vec<(SegmentKey, SourceKind, u64)>,
        fail_open: bool,
    }

    #[derive(Clone, Default)]
    struct FakeSegmentFs {
        state: Arc<Mutex<FakeSegmentState>>,
    }

    impl FakeSegmentFs {
        fn events(&self) -> Vec<FsEvent> {
            self.state.lock().unwrap().events.clone()
        }

        fn writes(&self) -> Vec<(SegmentKey, SourceKind, u64)> {
            self.state.lock().unwrap().writes.clone()
        }
    }

    impl SegmentFs for FakeSegmentFs {
        type Error = &'static str;

        fn open_incomplete(&mut self, key: SegmentKey) -> Result<String, Self::Error> {
            let mut state = self.state.lock().unwrap();
            if state.fail_open {
                return Err("open failed");
            }
            state.events.push(FsEvent::Open(key));
            Ok(format!("/segments/{}.incomplete", key.index))
        }

        fn write_chunk(
            &mut self,
            key: SegmentKey,
            chunk: &CaptureChunk,
        ) -> Result<(), Self::Error> {
            let mut state = self.state.lock().unwrap();
            state
                .events
                .push(FsEvent::Write(key, chunk.source, chunk.seq));
            state.writes.push((key, chunk.source, chunk.seq));
            Ok(())
        }

        fn finalize(&mut self, key: SegmentKey) -> Result<(), Self::Error> {
            self.state
                .lock()
                .unwrap()
                .events
                .push(FsEvent::Finalize(key));
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

    struct Handles {
        screen: FakeSourceHandle,
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
            system_audio: FakeSourceHandle::new(system_audio_state),
            mic: FakeSourceHandle::new(mic_state),
        };
        let sources = Sources {
            screen: Box::new(FakeScreen {
                handle: handles.screen.clone(),
            }),
            system_audio: Box::new(FakeSystemAudio {
                handle: handles.system_audio.clone(),
            }),
            mic: Box::new(FakeMic {
                handle: handles.mic.clone(),
            }),
        };
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
            sink.emit(CaptureChunk {
                source: SourceKind::Screen,
                seq,
                data: vec![seq as u8],
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

        engine.start().unwrap();
        assert_eq!(handles.screen.starts(), 1);
        assert_eq!(handles.system_audio.starts(), 1);
        assert_eq!(handles.mic.starts(), 1);
        engine.stop().unwrap();
    }

    #[test]
    fn rotation_preserves_every_chunk_once_and_splits_segments() {
        let clock = FakeClock::new(299);
        let segment_fs = FakeSegmentFs::default();
        let segment_view = segment_fs.clone();
        let (sources, _) = active_sources();
        let mut engine = engine_with(clock.clone(), segment_fs, EngineConfig::default(), sources);
        engine.start().unwrap();
        let sink = engine.sink();

        emit_screen(&sink, 0..5);
        engine.pump().unwrap();

        clock.set(300);
        emit_screen(&sink, 5..8);
        engine.pump().unwrap();

        emit_screen(&sink, 8..11);
        engine.pump().unwrap();

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

        engine.start().unwrap();
        engine.pump().unwrap();

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
        engine.start().unwrap();

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
        engine.start().unwrap();

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

        engine.start().unwrap();
        engine.pump().unwrap();

        assert_eq!(engine.health_dump().app_state, AppPhase::Observing);
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
        engine.start().unwrap();
        let sink = engine.sink();

        clock.set(5);
        emit_screen(&sink, 0..10);
        engine.pump().unwrap();

        assert_eq!(engine.health_dump().frame_rate, Some(2));
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
        };
        let expected = observer_health::to_pretty_json(&fed).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(serve_health(listener, Arc::new(Mutex::new(fed))));

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
