# Design 1A: capture core

Status: design-only. No production code is implemented by this document.

## Validation notes

- `observer-model` is pure and forbids unsafe at `crates/observer-model/src/lib.rs:15`.
- Current source traits are `start(&mut self)` only at `crates/observer-model/src/lib.rs:157`, `:167`, `:175`; all must change to sink-taking starts.
- `SourceKind` currently lacks `Ord` at `crates/observer-model/src/lib.rs:44`; using `BTreeMap<SourceKind, ...>` requires adding `PartialOrd, Ord` derives. This does not affect serialization.
- `observer-segment` already depends on `observer-model` at `crates/observer-segment/Cargo.toml:10`, so it can name `CaptureChunk` while staying pure.
- `SegmentFs` currently has only `open_incomplete` and `finalize` at `crates/observer-segment/src/lib.rs:54`.
- `CaptureEngine::new` currently returns `Result<(Self, Vec<RecoveryOutcome>), F::Error>` and runs `recover_all` before constructing the engine at `crates/capture-engine/src/lib.rs:61`.
- `SourceFaulted` discards fault detail at `crates/observer-state/src/lib.rs:117`; engine must fold faults through `AppEvent::SourceUpdated(SourceReport { state: SourceState::Faulted { ... } })`.
- `StateMachine` has no `engine_ready()` accessor; health production must either add one or mirror engine readiness. This design adds the accessor.
- Contract generation derives token vocabulary from enum variants and explicit `SourceState` status strings at `crates/observer-contract/src/lib.rs:93`; adding structs/traits and `Ord` derives does not change `automation-contract.json`.
- Current `capture-engine` has no platform edges; `cargo tree -p capture-engine --target all -e normal` lists only observer pure crates. Keep that invariant.

## observer-model seams

Add imports:

```rust
use std::sync::Arc;
```

Add data/sink/clock seams:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureChunk {
    pub source: SourceKind,
    pub seq: u64,
    pub data: Vec<u8>,
}

pub trait CaptureSink: Send + Sync {
    fn emit(&self, chunk: CaptureChunk);
}

pub trait Clock: Send + Sync {
    fn now_epoch_secs(&self) -> u64;
}
```

Update `SourceKind` derives:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, EnumIter, IntoStaticStr)]
```

Update source traits:

```rust
pub trait ScreenSource: Send {
    fn start(&mut self, sink: Arc<dyn CaptureSink>) -> Result<(), SourceError>;
    fn stop(&mut self);
    fn state(&self) -> SourceState;
    fn on_display_changed(&mut self);
}

pub trait SystemAudioSource: Send {
    fn start(&mut self, sink: Arc<dyn CaptureSink>) -> Result<(), SourceError>;
    fn stop(&mut self);
    fn state(&self) -> SourceState;
}

pub trait MicSource: Send {
    fn start(&mut self, sink: Arc<dyn CaptureSink>) -> Result<(), SourceError>;
    fn stop(&mut self);
    fn state(&self) -> SourceState;
}
```

Purity impact: `Arc` is `std`, no `windows` dependency, no unsafe.

## observer-state seam

Add a read-only accessor:

```rust
impl StateMachine {
    pub fn engine_ready(&self) -> bool;
}
```

No reducer behavior changes. `Observing` remains computed by `phase()`.

## observer-segment seam

Extend imports and trait:

```rust
use observer_model::{CaptureChunk, SegmentKey};

pub trait SegmentFs {
    type Error: core::fmt::Debug;

    fn open_incomplete(&mut self, key: SegmentKey) -> Result<String, Self::Error>;
    fn write_chunk(&mut self, key: SegmentKey, chunk: &CaptureChunk) -> Result<(), Self::Error>;
    fn finalize(&mut self, key: SegmentKey) -> Result<(), Self::Error>;
}
```

Real file layout:

- Segment dir: `<segments_root>/<key.index>.incomplete`.
- Final dir: `<segments_root>/<key.index>`.
- Source files:
  - `SourceKind::Screen` -> `display_1_screen.mp4`
  - `SourceKind::SystemAudio` -> `system-audio.pcm`
  - `SourceKind::Mic` -> `mic.pcm`
- `write_chunk` lazily opens the per-source file with append semantics and writes `chunk.data` exactly as received.
- `finalize` flushes/drops open file handles for that key before atomic rename. This matters on Windows.

WGC rate cap: the screen source uses `MinimumUpdateIntervalSettings::Custom(Duration::from_millis(1000))`, approximately 1 fps. At 1080p RGBA8 (~8.3 MB/frame), this is ~2.5 GB per five-minute segment and ~15 GB per 30-minute soak; higher resolutions scale linearly by pixel count. No encoder is added in 1A; encoding/compression stays deferred.

Format risk: 1A raw append has no per-chunk header, dimensions, stride, sample format, or persisted `seq`. This matched the locked KISS shape but made the old screen file harder to inspect across display-size changes. The 1 fps cap bounded raw volume but did not make it storage-efficient. The fake fs owns exact seq/segment assertions.

## capture-engine Cargo and types

Add pure/runtime deps only:

```toml
observer-health.workspace = true
tokio = { workspace = true, features = ["net", "io-util"] }
tracing.workspace = true
```

No `capture-wgc`, `capture-wasapi`, or `platform-win` dependency. Verify with:

```text
cargo tree -p capture-engine --target all -e normal
```

Engine support types:

```rust
pub const BREAKER_OPEN_MARKER: &str = "[breaker-open] ";

#[derive(Debug)]
pub enum EngineExit {
    Shutdown,
    CommandChannelClosed,
    EventChannelClosed,
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_epoch_secs(&self) -> u64;
}

struct EngineSink {
    tx: tokio::sync::mpsc::UnboundedSender<CaptureChunk>,
}

impl CaptureSink for EngineSink {
    fn emit(&self, chunk: CaptureChunk);
}

struct OpenSegment {
    key: SegmentKey,
    dir: String,
    opened_epoch_secs: u64,
    screen_chunks: u64,
}
```

`EngineConfig` extension:

```rust
pub struct EngineConfig {
    pub segment_secs: u64,
    pub lifecycle: observer_lifecycle::BackoffConfig,
}
```

`Default` keeps `DEFAULT_SEGMENT_SECS` and `BackoffConfig::default()`.

`CaptureEngine` becomes generic over the held segment fs:

```rust
pub struct CaptureEngine<SFS: SegmentFs> {
    sources: Sources,
    state: StateMachine,
    config: EngineConfig,
    segment_fs: SFS,
    clock: Box<dyn Clock>,
    current_segment: Option<OpenSegment>,
    tx: tokio::sync::mpsc::UnboundedSender<CaptureChunk>,
    rx: tokio::sync::mpsc::UnboundedReceiver<CaptureChunk>,
    sink: Arc<EngineSink>,
    source_reports: BTreeMap<SourceKind, SourceReport>,
    lifecycles: BTreeMap<SourceKind, observer_lifecycle::Lifecycle>,
    retry_at_epoch_secs: BTreeMap<SourceKind, u64>,
    shared_health: Arc<std::sync::Mutex<HealthDump>>,
}
```

`CaptureEngine::new` keeps recovery-first and does not open a segment, so the error type remains the recovery fs error:

```rust
impl<SFS> CaptureEngine<SFS>
where
    SFS: SegmentFs,
{
    pub fn new<RFS>(
        sources: Sources,
        config: EngineConfig,
        recovery_fs: &mut RFS,
        segment_fs: SFS,
        clock: Box<dyn Clock>,
    ) -> Result<(Self, Vec<RecoveryOutcome>), RFS::Error>
    where
        RFS: RecoveryFs;
}
```

Construction sequence:

1. `recover_all(recovery_fs)?`.
2. Create channel/sink and internal maps.
3. Reduce `AppEvent::EngineReady`.
4. Refresh initial shared health.
5. Return `(engine, outcomes)`.

Public method list:

```rust
pub fn state(&self) -> &StateMachine;
pub fn state_mut(&mut self) -> &mut StateMachine;
pub fn segment_secs(&self) -> u64;
pub fn sink(&self) -> Arc<dyn CaptureSink>;
pub fn health_handle(&self) -> Arc<std::sync::Mutex<HealthDump>>;
pub fn health_dump(&self) -> HealthDump;
pub fn start(&mut self);
pub fn stop(&mut self);
pub fn on_display_changed(&mut self);
pub fn pump(&mut self);
pub fn apply_command(&mut self, command: EngineCommand);
pub async fn run(
    &mut self,
    shutdown: tokio::sync::oneshot::Receiver<()>,
    command_rx: tokio::sync::mpsc::UnboundedReceiver<EngineCommand>,
) -> EngineExit;
```

Private helper list:

```rust
fn reduce(&mut self, event: AppEvent) -> AppPhase;
fn open_current_segment(&mut self);
fn drain_chunks(&mut self);
fn write_chunk(&mut self, chunk: CaptureChunk);
fn rotate_if_needed(&mut self);
fn fold_source_states(&mut self);
fn apply_source_report(&mut self, report: SourceReport);
fn handle_fault_transition(&mut self, kind: SourceKind, previous: Option<&SourceState>, current: &SourceState);
fn retry_due_sources(&mut self);
fn refresh_health(&mut self);
```

`start()` sequence:

1. Reduce `RequestedStart`.
2. Open current segment for `segment_for(clock.now_epoch_secs(), config.segment_secs)`.
3. Clone `self.sink()` into each source `start(sink)`.
4. Convert any source `start()` error into a mirrored `SourceState::Faulted { reason, detail }`, reduce through `SourceUpdated`, run the lifecycle/breaker path, and continue.
5. Poll/fold states once and refresh health.

Segment-open and write failures are folded into the storage facet; `start()` does not abort.

`stop()` stops all sources and finalizes the current open segment if present.

## Pump, run loop, and no-loss rotation

`pump()` deterministic order:

1. Drain all currently queued chunks with `rx.try_recv()`.
2. Each drained chunk writes to the current segment key.
3. Then read `now = clock.now_epoch_secs()`.
4. Rotate if `should_rotate(current.key, now, config.segment_secs)`.
5. Retry due sources.
6. Poll/fold all source states.
7. Refresh health.

`run()` uses a `tokio::time::interval(Duration::from_secs(1))` and `tokio::select!` over:

- `shutdown`
- `rx.recv()` for immediate chunk writes
- `interval.tick()` for `pump()`-equivalent rotate/retry/fold/health work

No-loss seam:

- Chunks drained before the rotation check always write to the old key.
- Rotation finalizes old and opens new after the drain.
- Chunks emitted after rotation are received under the new current key.
- Fake `SegmentFs` records `(key, chunk.source, chunk.seq)`, so tests assert exact set membership with no gap/dup.

## State fold and Observing sequence

Source report construction in 1A:

- `device: None` for all sources. Current source traits have no device-label method. Add one later only if needed.

Never use `SourceFaulted`; it overwrites real detail with `Unknown/"source faulted"`.

Observing sequence for screen + system audio active and no mic:

1. `new()` reduces `EngineReady`; phase remains `Idle` until start requested.
2. `start()` reduces `RequestedStart`; phase becomes `Starting`.
3. `fold_source_states()` reduces `Screen Active`; still `Starting`.
4. Reduces `SystemAudio Active`; phase becomes `Observing`.
5. Reduces `Mic NoInputDevice`; phase remains `Observing`.

This matches reducer tests at `crates/observer-state/src/lib.rs:151` and `:171`.

## Fault and lifecycle behavior

Maintain one `Lifecycle` per `SourceKind`.

Fault edge:

- Compare previous mirrored `SourceState` to current reported state.
- On transition into `SourceState::Faulted { reason, detail }`, call `Lifecycle::on_failure()`.
- `RetryAfter(d)`: keep the source's real `reason/detail` in the mirror and set `retry_at_epoch_secs[kind] = now + d.as_secs()`.
- `GiveUp`: mirror `Faulted { reason, detail: format!("{BREAKER_OPEN_MARKER}{detail}") }`.

Retry:

- On each pump/tick, retry due sources by calling `stop()` then `start(self.sink())`.
- On start success, call `Lifecycle::on_success()` and clear retry deadline.
- On start error, mirror that `SourceError` as `Faulted`, then run the same failure decision path.

Health distinction:

- Transient fault: marker absent.
- Breaker open: marker present.
- `automation-contract.json` unchanged because no enum variant or HealthDump field is added.

## HealthDump production

Signature:

```rust
pub fn health_dump(&self) -> HealthDump;
```

Fields:

- `app_state`: `self.state.phase()`
- `sources`: `self.source_reports.values().cloned().collect()`
- `segment_dir`: `self.current_segment.as_ref().map(|s| s.dir.clone())`
- `segment_seconds_remaining`: `Some(seconds_until_next_boundary(now, period))` only when `app_state == Observing`
- `frame_rate`: when observing and a segment is open, `Some(screen_chunks / max(1, now - opened_epoch_secs) as u32)`, else `None`
- `engine_ready`: `self.state.engine_ready()`
- `version`: `env!("CARGO_PKG_VERSION").to_string()`

`refresh_health()` writes `health_dump()` into `shared_health`.

## Display change

Signature:

```rust
pub fn on_display_changed(&mut self);
```

Behavior:

- Forward only to `self.sources.screen.on_display_changed()`.
- Do not rotate the segment.
- Platform shell/pump wiring is 1B: `platform-win::SystemNotification::DisplayChanged` already exists at `crates/platform-win/src/lib.rs:70`.
- Host test uses a fake screen source that records this call.
- No engine-to-platform edge is introduced.

## Loopback health server

Signature:

```rust
pub async fn serve_health(
    listener: tokio::net::TcpListener,
    dump: Arc<std::sync::Mutex<HealthDump>>,
) -> std::io::Result<()>;
```

Behavior:

- Loopback-only caller responsibility: production and tests bind `127.0.0.1`, never `0.0.0.0`.
- For each accepted connection:
  - read and discard request bytes with Tokio I/O extension traits
  - clone `HealthDump` under a short std mutex lock
  - serialize with `observer_health::to_pretty_json`
  - write minimal HTTP/1.1 response with `Content-Type: application/json` and exact `Content-Length`
- Independently testable: caller can pass a fixed `Arc<Mutex<HealthDump>>` without launching the binary.

Use `std::sync::Mutex`, not `tokio::sync::Mutex`; the critical section is tiny and sync.

## Platform-tier implementation shape

### capture-wgc

`WgcScreenSource` fields:

```rust
control: Option<windows_capture::capture::CaptureControl<Handler, HandlerError>>,
state: Arc<Mutex<SourceState>>,
last_sink: Option<Arc<dyn CaptureSink>>,
seq: Arc<AtomicU64>,
```

`start(sink)`:

- Build `Settings::new(Monitor::primary()?, ..., ColorFormat::Rgba8, flags)`.
- `flags` carries sink, shared state, and seq counter.
- Call `Handler::start_free_threaded(settings)`.
- Store `CaptureControl`.

Handler:

- `new(ctx)` receives flags through `ctx.flags`.
- `on_frame_arrived` copies `frame.buffer()?.as_nopadding_buffer(&mut scratch)` inside the handler thread.
- Emit `CaptureChunk { source: SourceKind::Screen, seq, data }`.
- Set state Active on successful fresh frames.
- `on_closed` sets `Faulted { reason: EndpointLost, detail }` without panic.

`on_display_changed()`:

- Stop current control.
- Restart using last sink and fresh `Monitor::primary()`.
- If no last sink, only update state as inactive/faulted as appropriate.

Correctness constraint: `Frame` is not Send/Sync; copy bytes inside the WGC handler thread only.

### capture-wasapi

`WasapiSystemAudioSource`:

- `start(sink)` spawns a `std::thread`.
- Thread initializes COM, opens `eRender/eConsole`, initializes `AUDCLNT_STREAMFLAGS_LOOPBACK`, starts client.
- Pull loop: sleep, `GetNextPacketSize`, `GetBuffer`, copy packet bytes into owned `Vec<u8>`, emit `CaptureChunk { source: SystemAudio, seq, data }`, `ReleaseBuffer`.
- Stop flag is `AtomicBool`; state is `Arc<Mutex<SourceState>>`.

`WasapiMicSource`:

- `state()`/`start()` enumerate `EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)`.
- `GetCount() == 0` -> `SourceState::NoInputDevice`; `start()` returns `Ok(())` without spawning.
- If endpoint exists, spawn the same pull-loop shape without loopback flag.
- This crate owns the no-mic determination.

Correctness constraint: WASAPI `GetBuffer` pointers are valid only until `ReleaseBuffer`; copy before release.

### platform-win

`LocalSegmentFs`:

- std-only, host-testable.
- Holds root dir and lazy per-source append handles for the current key.
- `open_incomplete` creates dir and returns absolute path.
- `write_chunk` appends to source file.
- `finalize` flushes/drops handles then renames `<n>.incomplete` -> `<n>`.

`LocalRecoveryFs`:

- std-only, host-testable.
- `scan_incomplete` enumerates `*.incomplete` under segments dir.
- `has_usable_data` is true when at least one media file is non-empty.
- Staleness guard skips dirs whose mtime is newer than `segment_secs + RECOVERY_STALENESS_MARGIN_SECS`.
- Fresh current segment must never be swept.

`acquire_single_instance`:

- Windows-only box test.
- `CreateMutexW` in `Local\...`; `ERROR_ALREADY_EXISTS` -> `AlreadyRunning`.
- Hold mutex handle for process life.

`NotificationPump`:

- Windows-only box test.
- Message-only window.
- `WTSRegisterSessionNotification`, power suspend/resume, `WM_DISPLAYCHANGE -> SystemNotification::DisplayChanged`.

## windows-family duplication decision

Decision: drop the direct `windows = "0.58"` dependency from `capture-wgc` if the implementation can stay entirely on `windows-capture` exported types.

Rationale:

- Prep showed `windows-capture 2.0.0` brings `windows 0.62.2`.
- The planned WGC implementation uses `windows_capture::{capture, frame, monitor, settings}` types.
- Removing direct `windows 0.58` reduces the duplicate-version surface.
- If implementation later needs raw `windows::` APIs in `capture-wgc`, keep a direct dependency but align it to the `windows-capture` family where possible.

## Required host/box tests

1. Rotation and no-loss, host:
   - Use `FakeClock { now: Arc<AtomicU64> }`, `FakeSegmentFs`, and fake sources.
   - Construct engine, `start()`, get `let sink = engine.sink()`.
   - Emit seq `0..5`, pump at old time.
   - Advance clock across boundary, emit seq `5..8`, pump. These still land in old segment because drain precedes rotation.
   - Emit seq `8..11`, pump. These land in new segment.
   - Assert fake observed `finalize(old)` before `open_incomplete(new)`.
   - Assert union of recorded seqs is exactly `0..11`, once each.

2. Recovery-first, host:
   - Fake `RecoveryFs` records scan/finalize/quarantine.
   - Fake sources record starts.
   - Call `CaptureEngine::new(...)`.
   - Assert recovery ran and source start counts are zero.
   - Then call `start()` and assert starts happen after recovery.

3. Staleness guard, host and box:
   - Use a unique temp dir under `std::env::temp_dir()`.
   - Create one stale non-empty `.incomplete` and one fresh non-empty `.incomplete`.
   - Set mtimes with std APIs; no new dependency.
   - `LocalRecoveryFs::scan_incomplete()` returns only the stale segment.

4. Fault provenance, host:
   - Fake required source reports `Faulted { reason: EndpointLost, detail: "x" }`.
   - `pump()`.
   - `health_dump().sources` contains the exact reason/detail.
   - `health_dump().app_state == AppPhase::Error`.

5. Breaker visibility, host:
   - Use `EngineConfig { lifecycle: BackoffConfig { breaker_threshold: 1, .. } }`.
   - Fake required source faults.
   - `pump()`.
   - Health fault detail contains `BREAKER_OPEN_MARKER`.
   - Repeat with threshold above 1 and assert transient detail does not contain the marker.

6. No-mic, host:
   - Fake screen and system audio report `Active`.
   - Fake mic reports `NoInputDevice`.
   - `start()` then `pump()`.
   - `health_dump().app_state == AppPhase::Observing`.

7. Loopback health, host:
   - Bind `tokio::net::TcpListener::bind("127.0.0.1:0")`.
   - Spawn `serve_health(listener, Arc<Mutex<fed_dump>>)`.
   - Connect with `tokio::net::TcpStream`.
   - Assert response body equals `observer_health::to_pretty_json(&fed_dump).unwrap()`.
   - No binary launch.

## Purity, DAG, and contract impact

- Pure tier remains windows-free: `observer-model`, `observer-segment`, `observer-state`, `observer-health`, `observer-recovery`, `observer-lifecycle`, and `observer-contract` add no platform dependency and keep `#![forbid(unsafe_code)]`.
- `capture-engine` remains composition tier only: pure crates plus Tokio runtime utilities. No `capture-wgc`, `capture-wasapi`, `platform-win`, `windows`, or `windows-capture` edge.
- Platform crates keep Windows APIs quarantined and target-gated.
- `automation-contract.json` is unchanged: no enum variants or AutomationId constants are added. Breaker visibility uses a detail-string marker, not a schema change.
