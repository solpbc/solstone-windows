// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Shared vocabulary for the solstone Windows observer.
//!
//! This is the **pure tier**: no platform dependency, no `unsafe`. Everything a
//! source produces or the shell renders is named here once, so there is exactly
//! one definition of "observing", one set of source states, one health payload.
//!
//! The shell is a pure renderer that subscribes to [`HealthDump`]; it never
//! mints status. `app_state` is *computed* by the reducer (see `observer-state`)
//! and `SourceState` is *reported* by the sources — neither is settable by the
//! UI. That is what makes "status earned, never asserted" an architectural fact.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use strum::{EnumIter, IntoStaticStr};

pub mod launch;
pub mod tray_status;

pub use launch::{launch_should_surface, FROM_AUTOSTART_ARG};
pub use tray_status::{classify_tray, TrayVisual};

/// Final screen media filename inside a sealed segment.
pub const SCREEN_FILE_NAME: &str = "display_1_screen.mp4";

/// Final combined-audio filename inside a sealed segment.
pub const AUDIO_FILE_NAME: &str = "audio.flac";

/// Sealed-dir sidecar holding the honest captured-media duration (whole seconds,
/// ASCII decimal) for the segment-key LEN suffix.
///
/// Excluded from upload, mirroring the .uploaded marker.
pub const LEN_FILE_NAME: &str = ".len";

/// The observer's top-level phase. **Computed** from real source state by the
/// reducer in `observer-state` — never set directly by the UI or any command.
///
/// `Observing` exists only as a derived conclusion: it is reachable when the
/// engine is running and the required sources are `Active`. There is no setter.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, EnumIter, IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum AppPhase {
    /// Tray-resident, nothing running.
    Idle,
    /// Engine starting; sources not yet all active.
    Starting,
    /// Computed-true: engine running and required sources active.
    Observing,
    /// Operator-requested pause.
    Paused,
    /// A required source faulted; the observer cannot honestly claim observing.
    Error,
}

/// Which capture source a [`SourceState`] describes.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    EnumIter,
    IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum SourceKind {
    /// Windows.Graphics.Capture screen source.
    Screen,
    /// WASAPI render-loopback system audio.
    SystemAudio,
    /// WASAPI eCapture microphone.
    Mic,
}

/// Why a pause is in effect.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, EnumIter, IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum PauseReason {
    /// The operator paused from the tray or Settings.
    Operator,
    /// The interactive session locked.
    SessionLocked,
    /// The machine is entering sleep / standby.
    SystemSuspending,
}

/// A coarse, owner-meaningful classification of a source fault. The detailed
/// string lives alongside in [`SourceState::Faulted`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, EnumIter, IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ErrorReason {
    /// The OS endpoint or capture handle went away (device unplugged, session change).
    EndpointLost,
    /// Access to the device was denied.
    AccessDenied,
    /// The writer could not persist a segment.
    WriteFailed,
    /// An unclassified platform error.
    Unknown,
}

/// The honest, reported state of a single source. This is what a source tells
/// the engine; the engine never overrides it upward.
///
/// `NoInputDevice` is a **first-class** variant, not an error: a machine with no
/// microphone is a normal, supported configuration. The owner sees "no
/// microphone input device", never a fake "mic active" or a scary fault.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SourceState {
    /// Producing frames/samples right now.
    Active,
    /// Present but not currently producing (e.g. paused, or silent system audio).
    Inactive,
    /// No input device of this kind exists on the machine. First-class, honest.
    NoInputDevice,
    /// The source faulted; carries a coarse reason and a detail string.
    Faulted { reason: ErrorReason, detail: String },
}

/// PCM format of one audio source, captured at WASAPI client-open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioFormat {
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    pub is_float: bool,
}

/// A reported source plus which kind it is and the device label, if any.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceReport {
    pub kind: SourceKind,
    #[serde(flatten)]
    pub state: SourceState,
    /// Human-readable device name, when one is known.
    pub device: Option<String>,
}

/// Owned bytes emitted by a source into the capture engine.
///
/// `seq` is a per-source monotonic index used for diagnostics and no-loss tests.
/// `data` is owned so platform capture threads can copy out non-sendable or
/// short-lived buffers before crossing the pure sink seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureChunk {
    pub source: SourceKind,
    pub seq: u64,
    pub data: Vec<u8>,
    pub format: Option<AudioFormat>,
}

/// CPU pixel format for screen frames crossing the pure capture seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenPixelFormat {
    Rgba8,
    Bgra8,
}

/// Owned screen frame emitted by the WGC source and consumed by the encoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenFrame {
    pub seq: u64,
    /// Boot-relative SystemRelativeTime (QueryPerformanceCounter-derived), 100ns ticks.
    pub arrival_100ns: i64,
    pub width: u32,
    pub height: u32,
    pub pixel_format: ScreenPixelFormat,
    pub pixels: Arc<[u8]>,
}

/// Crop a screen frame down to even dimensions for NV12 consumers.
pub fn normalize_even(frame: &ScreenFrame) -> ScreenFrame {
    let even_w = frame.width & !1;
    let even_h = frame.height & !1;
    if even_w == frame.width && even_h == frame.height {
        return frame.clone();
    }

    let Some(expected) = (frame.width as usize)
        .checked_mul(frame.height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
    else {
        return frame.clone();
    };
    if frame.pixels.len() != expected {
        return frame.clone();
    }

    let src_stride = frame.width as usize * 4;
    let new_w = even_w as usize;
    let new_h = even_h as usize;
    let row_bytes = new_w * 4;
    let mut out = Vec::with_capacity(new_h * row_bytes);
    for y in 0..new_h {
        let start = y * src_stride;
        out.extend_from_slice(&frame.pixels[start..start + row_bytes]);
    }

    ScreenFrame {
        seq: frame.seq,
        arrival_100ns: frame.arrival_100ns,
        width: even_w,
        height: even_h,
        pixel_format: frame.pixel_format,
        pixels: Arc::from(out),
    }
}

/// Coarse encoder failure category. The engine maps all variants to
/// [`ErrorReason::WriteFailed`] when surfacing a source fault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncoderErrorKind {
    OpenFailed,
    EncodeFailed,
    FinalizeFailed,
    InvalidFrameDimensions,
    DeviceLost,
    Unavailable,
    WorkerStopped,
}

/// Error returned by the screen encoder seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderError {
    pub kind: EncoderErrorKind,
    pub detail: String,
}

impl EncoderError {
    pub fn new(kind: EncoderErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

impl core::fmt::Display for EncoderError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.detail)
    }
}

impl std::error::Error for EncoderError {}

/// Honest screen encoder accounting folded into health.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EncoderHealth {
    pub frames_consumed: u64,
    pub samples_written: u64,
    /// Samples whose time was clamped to stay monotonic / >= 0.
    ///
    /// Systematic clamping surfaces here.
    pub clamp_events: u64,
    pub last_error: Option<String>,
}

/// Capture-exclusion accounting folded into health so exclusion activity is
/// **never silent**: the owner can see that excluded surfaces are being kept out
/// of segments (regions redacted) and that uncertain frames are being dropped
/// whole rather than risk a leak. Reported by the screen source that enforces
/// exclusions; `None` in [`HealthDump`] when no exclusion-aware source is running.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ExclusionHealth {
    /// Whether any exclusion rule is currently configured.
    pub rules_active: bool,
    /// Frames that had one or more regions blacked out before encoding.
    pub frames_redacted: u64,
    /// Frames dropped whole because an excluded surface could not be safely
    /// redacted (unknown geometry, an unreadable window, or enumeration failure).
    pub frames_dropped: u64,
}

/// Inspectable H.264 encoder defaults used to configure Media Foundation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub bitrate: u32,
    pub frame_rate_num: u32,
    pub frame_rate_den: u32,
    pub pixel_aspect_num: u32,
    pub pixel_aspect_den: u32,
    pub progressive: bool,
    pub h264_high_profile: bool,
    pub gop_size: u32,
    pub enable_hardware_transforms: bool,
    pub use_only_hardware_transforms: bool,
    pub use_d3d_manager: bool,
    pub disable_throttling: bool,
}

impl EncoderConfig {
    pub fn for_frame_size(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            bitrate: 1_000_000,
            frame_rate_num: 1,
            frame_rate_den: 1,
            pixel_aspect_num: 1,
            pixel_aspect_den: 1,
            progressive: true,
            h264_high_profile: true,
            gop_size: 90,
            enable_hardware_transforms: true,
            use_only_hardware_transforms: false,
            use_d3d_manager: false,
            disable_throttling: true,
        }
    }
}

/// Identifies one rotation segment: which capture session and which clock-aligned
/// boundary index within it. The math that produces these lives in
/// `observer-segment`; this is just the key shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SegmentKey {
    /// Unix epoch seconds of the segment's aligned start boundary.
    pub boundary_epoch_secs: u64,
    /// Monotonic index of the segment within the running session.
    pub index: u64,
}

/// The observer's pairing phase with its journal. Like [`AppPhase`], this is a
/// *reported* fact — `Paired` is only ever set after a real pairing handshake
/// and observer registration succeed, never asserted optimistically.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Default,
    Serialize,
    Deserialize,
    EnumIter,
    IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum PairingPhase {
    /// No pairing credential on disk.
    #[default]
    NotPaired,
    /// A pairing handshake is in progress.
    Pairing,
    /// Paired and registered: the observer can upload to the journal.
    Paired,
    /// The last pairing attempt failed; carries a detail in [`PairingState`].
    Failed,
}

/// Render-readiness of one of our webview views. `Rendered` is *earned*: only our
/// own frontend writes it, and only after it has stamped its contract window
/// root. Surfaced on `/healthz` as the value type of `HealthDump::views`. Tokens
/// for this enum flow into the contract via `enum_tokens::<ViewRenderState>()`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, EnumIter, IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ViewRenderState {
    /// Our frontend has not (yet) reported a painted contract root. Represented on
    /// the live map by absence as well; both mean "not proven rendered".
    Pending,
    /// Our frontend painted its contract window root and called the beacon back.
    Rendered,
}

/// The two on-demand webview views. Single source of truth for the `--open-view`
/// startup arg's valid set, the `HealthDump::views` map keys, and the labels of
/// the Tauri webview windows. `ALL` drives `label`/`parse`/`valid_list` so the
/// valid set never drifts into two lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Settings,
    About,
}

impl View {
    /// Every view, in declaration order — the one place the set is enumerated.
    pub const ALL: [View; 2] = [View::Settings, View::About];

    /// The canonical lowercase label: the webview window label, the `--open-view`
    /// value, and the `HealthDump::views` key.
    pub fn label(self) -> &'static str {
        match self {
            View::Settings => "settings",
            View::About => "about",
        }
    }

    /// Parse a view name (an `--open-view` value or a window label). `None` for an
    /// unknown name — callers surface that, never panic.
    pub fn parse(input: &str) -> Option<View> {
        View::ALL.into_iter().find(|view| view.label() == input)
    }

    /// Comma-joined valid names for a user-facing "valid: ..." hint.
    pub fn valid_list() -> String {
        View::ALL
            .iter()
            .map(|view| view.label())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// The honest pairing state surfaced in the health dump.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PairingState {
    pub phase: PairingPhase,
    /// The paired journal's human label, when known.
    pub journal_label: Option<String>,
    /// The registered observer's stream name, when known.
    pub observer_name: Option<String>,
    /// A failure detail when `phase` is `Failed`.
    pub detail: Option<String>,
}

/// The honest upload/sync state surfaced in the health dump. Counts are earned
/// from real ingest outcomes — a segment counts as `uploaded` only after the
/// journal proves it holds the submitted files, never on optimistic send.
pub const RECENT_ERROR_COUNT_MAX: u8 = 99;
pub const LAST_ERROR_REASON_MAX_LEN: usize = 200;

/// The observed transport path used by an upload request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportPath {
    Direct,
    Relay,
}

impl TransportPath {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Relay => "relay",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UploadStatus {
    /// Sealed segments on disk not yet confirmed uploaded.
    pub pending_segments: u64,
    /// Segments confirmed landed in the journal this session.
    pub uploaded_segments: u64,
    /// Segments whose upload last failed (will be retried with backoff).
    pub failed_segments: u64,
    /// The last segment confirmed landed (`HHMMSS_LEN`).
    pub last_uploaded_segment: Option<String>,
    /// The last upload error detail, when one occurred.
    pub last_error: Option<String>,
    /// Epoch milliseconds of the last successful sync tick.
    pub last_successful_sync: Option<u64>,
    /// Consecutive failed sync ticks, bounded for journal diagnostics.
    pub recent_error_count: u8,
    /// Sanitized, single-line sync error code for journal diagnostics.
    pub last_error_reason: Option<String>,
    /// Whether the most recent heartbeat to the journal succeeded.
    pub heartbeat_ok: bool,
    /// Duration in milliseconds of the last confirmed upload.
    pub last_upload_duration_ms: Option<u64>,
    /// Total bytes in the last confirmed upload.
    pub last_upload_bytes: Option<u64>,
    /// Observed transport path for the last confirmed upload.
    pub last_upload_path: Option<TransportPath>,
    /// Dial attempts taken by the winning leg of the last confirmed upload.
    pub last_upload_dial_attempts: Option<u32>,
}

impl UploadStatus {
    pub fn record_failure(&mut self, reason_code: &str) {
        self.recent_error_count = self
            .recent_error_count
            .saturating_add(1)
            .min(RECENT_ERROR_COUNT_MAX);
        self.last_error_reason = Some(bounded_single_line(reason_code, LAST_ERROR_REASON_MAX_LEN));
    }

    pub fn record_success(&mut self, now_ms: u64) {
        self.recent_error_count = 0;
        self.last_error_reason = None;
        self.last_successful_sync = Some(now_ms);
    }
}

fn bounded_single_line(input: &str, max_chars: usize) -> String {
    input
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect()
}

/// The sync layer's snapshot (pairing + upload), folded into [`HealthDump`] by
/// the engine. `Default` is the honest not-paired, nothing-uploaded state.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SyncSnapshot {
    pub pairing: PairingState,
    pub upload: UploadStatus,
}

/// The honest pause detail surfaced in the health dump while the observer is
/// paused. Like every other field here it is *earned*, never asserted: it is
/// present only when the reducer is actually in [`AppPhase::Paused`].
///
/// `seconds_remaining` counts down to an automatic resume for a duration-bounded
/// operator pause (15m / 30m / 1h); it is `None` for an indefinite pause — the
/// operator's "until I resume", or a system lock/suspend pause that ends on its
/// own OS event rather than a timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PauseSnapshot {
    pub reason: PauseReason,
    pub seconds_remaining: Option<u64>,
}

/// The single honest-state payload, defined once and serialized identically
/// three ways: the `--dump-state` CLI JSON, the localhost `/healthz` body, and
/// the `health://changed` event the shell subscribes to. There is no second
/// representation of "observing" anywhere that could drift.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthDump {
    pub app_state: AppPhase,
    pub sources: Vec<SourceReport>,
    /// Effective capture frame rate, when observing.
    pub frame_rate: Option<u32>,
    /// Absolute path to the active segment directory, when one is open.
    pub segment_dir: Option<String>,
    /// Seconds until the next rotation boundary, when observing.
    pub segment_seconds_remaining: Option<u64>,
    /// Whether the engine has finished construction/recovery and is ready.
    pub engine_ready: bool,
    /// The observer build version.
    pub version: String,
    /// Pairing + upload state from the Wave-2 sync layer. Defaults to the
    /// not-paired/idle snapshot when sync is not running.
    #[serde(default)]
    pub sync: SyncSnapshot,
    /// Screen encoder accounting, present while the engine is running.
    #[serde(default)]
    pub screen_encoder: Option<EncoderHealth>,
    /// Capture-exclusion accounting, present when the screen source enforces it.
    #[serde(default)]
    pub exclusions: Option<ExclusionHealth>,
    /// Honest pause detail (reason + countdown to auto-resume), present only
    /// while the observer is paused. `None` in every non-paused phase.
    #[serde(default)]
    pub pause: Option<PauseSnapshot>,
    /// App-owned per-view render-readiness beacon (label -> render-state). The
    /// engine never computes this; it only carries it forward across its wholesale
    /// dump replace. Meaningful only on the live `/healthz`. Empty until a view
    /// proves it painted.
    #[serde(default)]
    pub views: BTreeMap<String, ViewRenderState>,
}

/// True when `next` differs from `previous` in a way the Settings/About UI
/// renders, so the shell should re-emit `health://changed`. Fail-safe: the
/// ignore-list below is CLOSED — only these five volatile leaves are masked;
/// any other difference (including a field a future arc adds) forces an emit,
/// so the predicate can never swallow a real discrete state change.
///
/// Ignored because they advance ~every 1 s tick during steady observing and are
/// not displayed: `frame_rate`, `segment_seconds_remaining`,
/// `pause.seconds_remaining` (the bounded-pause countdown, advanced UI-side off
/// the 1 s timer instead), and the `screen_encoder` counters
/// `frames_consumed` / `samples_written` / `clamp_events`. The
/// `pause`/`screen_encoder` PRESENCE and `pause.reason` /
/// `screen_encoder.last_error` stay meaningful.
pub fn should_emit(previous: &HealthDump, next: &HealthDump) -> bool {
    canonicalize_for_emit(previous) != canonicalize_for_emit(next)
}

/// Clone `dump` with only the emit-ignored leaves masked to a fixed constant.
/// Everything else is left intact so the derived `PartialEq` compares it.
fn canonicalize_for_emit(dump: &HealthDump) -> HealthDump {
    let mut d = dump.clone();
    d.frame_rate = None;
    d.segment_seconds_remaining = None;
    if let Some(pause) = d.pause.as_mut() {
        pause.seconds_remaining = None;
    }
    if let Some(encoder) = d.screen_encoder.as_mut() {
        encoder.frames_consumed = 0;
        encoder.samples_written = 0;
        encoder.clamp_events = 0;
    }
    d
}

// ── Source traits ────────────────────────────────────────────────────────────
// The pure tier defines the *seams*; the platform tier (capture-wgc /
// capture-wasapi) implements them, and `capture-engine` is injected the
// concrete `dyn` impls. So the engine is host-testable with fakes on Linux.

/// Synchronous, non-blocking sink that sources emit owned chunks into.
///
/// The channel-backed implementation lives in `capture-engine`, keeping async
/// runtime types out of the pure tier.
pub trait CaptureSink: Send + Sync {
    fn emit(&self, chunk: CaptureChunk);
    fn emit_screen_frame(&self, frame: ScreenFrame);
}

/// Injected wall-clock seam used by the engine's rotation logic.
///
/// The real implementation lives in `capture-engine`; tests use a deterministic
/// fake clock.
pub trait Clock: Send + Sync {
    fn now_epoch_secs(&self) -> u64;
}

/// Device-local UTC-offset seam. The Windows impl lives in `platform-win`
/// (windows-rs quarantine); off-Windows it is an honest error, never UTC.
pub trait LocalOffset: Send + Sync + std::fmt::Debug {
    /// Signed local-minus-UTC offset in seconds, DST-correct for `epoch_secs`.
    fn local_offset_secs(&self, epoch_secs: u64) -> Result<i64, LocalOffsetError>;
}

/// Why a local-offset lookup failed. Payload-free so no host-specific string
/// (timezone-db path, etc.) can leak into a log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalOffsetError {
    /// The platform local-time lookup call failed.
    Lookup,
    /// No local-offset source on this platform (off-Windows stub).
    Unsupported,
}

/// A screen capture source (implemented by `capture-wgc` on the build box).
pub trait ScreenSource: Send {
    /// Begin producing screen frames into `sink`.
    fn start(&mut self, sink: Arc<dyn CaptureSink>) -> Result<(), SourceError>;
    /// Stop producing.
    fn stop(&mut self);
    /// The current honestly-reported state.
    fn state(&self) -> SourceState;
    /// Re-acquire the screen source after a display topology or resolution change.
    fn on_display_changed(&mut self);
    /// Capture-exclusion accounting, when this source enforces exclusions.
    /// Default `None` for sources that don't (test fakes, the off-Windows stub).
    fn exclusion_health(&self) -> Option<ExclusionHealth> {
        None
    }
}

/// Screen encoder driven by the engine at the segment lifecycle boundary.
pub trait ScreenEncoder: Send {
    fn open(&mut self, dir: &str, width: u32, height: u32) -> Result<(), EncoderError>;
    fn encode_frame(&mut self, frame: &ScreenFrame) -> Result<(), EncoderError>;
    fn finalize(&mut self) -> Result<(), EncoderError>;
    fn frames_consumed(&self) -> u64;
    fn samples_written(&self) -> u64;
    /// The last video sample's end offset in seconds for the just-finalized
    /// segment, or None if the segment produced no video. Feeds the honest LEN
    /// only when audio is absent.
    fn video_end_secs(&self) -> Option<f64>;
    fn last_error(&self) -> Option<String>;
    fn health(&self) -> EncoderHealth;
}

/// A system-audio (render loopback) source (implemented by `capture-wasapi`).
pub trait SystemAudioSource: Send {
    /// Begin producing PCM chunks into `sink`.
    fn start(&mut self, sink: Arc<dyn CaptureSink>) -> Result<(), SourceError>;
    fn stop(&mut self);
    fn state(&self) -> SourceState;
}

/// A microphone (eCapture) source (implemented by `capture-wasapi`). Owns the
/// [`SourceState::NoInputDevice`] determination when no mic endpoint exists.
pub trait MicSource: Send {
    /// Begin producing PCM chunks into `sink`, or report `NoInputDevice`.
    fn start(&mut self, sink: Arc<dyn CaptureSink>) -> Result<(), SourceError>;
    fn stop(&mut self);
    fn state(&self) -> SourceState;
}

/// Error returned by a source operation. Carries the same coarse reason the UI
/// would surface, so a fault round-trips into [`SourceState::Faulted`] without
/// reclassification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceError {
    pub reason: ErrorReason,
    pub detail: String,
}

impl SourceError {
    pub fn new(reason: ErrorReason, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: detail.into(),
        }
    }
}

impl core::fmt::Display for SourceError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let reason: &'static str = self.reason.into();
        write!(f, "{reason}: {}", self.detail)
    }
}

impl std::error::Error for SourceError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use strum::IntoEnumIterator;

    fn px(x: usize, y: usize) -> [u8; 4] {
        [
            x as u8,
            y as u8,
            (x as u8).wrapping_mul(31).wrapping_add(y as u8),
            255,
        ]
    }

    fn build_frame(w: u32, h: u32) -> ScreenFrame {
        let mut pixels = Vec::with_capacity(w as usize * h as usize * 4);
        for y in 0..h as usize {
            for x in 0..w as usize {
                pixels.extend_from_slice(&px(x, y));
            }
        }
        ScreenFrame {
            seq: 7,
            arrival_100ns: 0,
            width: w,
            height: h,
            pixel_format: ScreenPixelFormat::Rgba8,
            pixels: Arc::from(pixels),
        }
    }

    fn assert_normalizes_to(input: &ScreenFrame, out_w: u32, out_h: u32) -> ScreenFrame {
        let output = normalize_even(input);
        assert_eq!((output.width, output.height), (out_w, out_h));
        assert_eq!(output.pixels.len(), out_w as usize * out_h as usize * 4);
        for y in 0..out_h as usize {
            for x in 0..out_w as usize {
                let i = (y * out_w as usize + x) * 4;
                assert_eq!(&output.pixels[i..i + 4], &px(x, y), "pixel ({x},{y})");
            }
        }
        output
    }

    fn base_dump() -> HealthDump {
        HealthDump {
            app_state: AppPhase::Observing,
            sources: vec![SourceReport {
                kind: SourceKind::Screen,
                state: SourceState::Active,
                device: Some("d".into()),
            }],
            frame_rate: Some(1),
            segment_dir: Some("segments/2026-07-01T12-00-00Z".into()),
            segment_seconds_remaining: Some(120),
            engine_ready: true,
            version: "test".into(),
            sync: SyncSnapshot {
                pairing: PairingState {
                    phase: PairingPhase::Paired,
                    journal_label: Some("journal".into()),
                    observer_name: Some("observer".into()),
                    detail: None,
                },
                upload: UploadStatus {
                    pending_segments: 2,
                    uploaded_segments: 3,
                    failed_segments: 1,
                    last_uploaded_segment: Some("120000".into()),
                    last_error: None,
                    last_successful_sync: Some(1_700_000_000_000),
                    recent_error_count: 1,
                    last_error_reason: Some("retry".into()),
                    heartbeat_ok: true,
                    last_upload_duration_ms: Some(42),
                    last_upload_bytes: Some(12_345),
                    last_upload_path: Some(TransportPath::Direct),
                    last_upload_dial_attempts: Some(2),
                },
            },
            screen_encoder: Some(EncoderHealth {
                frames_consumed: 10,
                samples_written: 20,
                clamp_events: 0,
                last_error: None,
            }),
            exclusions: None,
            pause: Some(PauseSnapshot {
                reason: PauseReason::Operator,
                seconds_remaining: Some(900),
            }),
            views: BTreeMap::new(),
        }
    }

    #[test]
    fn normalize_even_preserves_even_frame_arc() {
        let input = build_frame(4, 4);
        let output = assert_normalizes_to(&input, 4, 4);

        assert_eq!(output.pixels, input.pixels);
        assert!(Arc::ptr_eq(&input.pixels, &output.pixels));
    }

    #[test]
    fn normalize_even_crops_odd_height_only() {
        let input = build_frame(4, 3);

        assert_normalizes_to(&input, 4, 2);
    }

    #[test]
    fn normalize_even_crops_odd_width_only() {
        let input = build_frame(3, 4);

        assert_normalizes_to(&input, 2, 4);
    }

    #[test]
    fn normalize_even_crops_odd_width_and_height() {
        let input = build_frame(3, 3);

        assert_normalizes_to(&input, 2, 2);
    }

    #[test]
    fn normalize_even_returns_malformed_frame_unchanged() {
        let input = ScreenFrame {
            seq: 9,
            arrival_100ns: 0,
            width: 3,
            height: 3,
            pixel_format: ScreenPixelFormat::Rgba8,
            pixels: Arc::from(vec![1u8, 2, 3, 4]),
        };
        let output = normalize_even(&input);

        assert_eq!((output.width, output.height), (3, 3));
        assert_eq!(output.pixels, input.pixels);
        assert!(Arc::ptr_eq(&input.pixels, &output.pixels));
    }

    #[test]
    fn encoder_config_matches_ac1_media_foundation_defaults() {
        let config = EncoderConfig::for_frame_size(1920, 1080);

        assert_eq!(config.width, 1920);
        assert_eq!(config.height, 1080);
        assert_eq!(config.bitrate, 1_000_000);
        assert_eq!((config.frame_rate_num, config.frame_rate_den), (1, 1));
        assert_eq!((config.pixel_aspect_num, config.pixel_aspect_den), (1, 1));
        assert!(config.progressive);
        assert!(config.h264_high_profile);
        assert_eq!(config.gop_size, 90);
        assert!(config.enable_hardware_transforms);
        assert!(!config.use_only_hardware_transforms);
        assert!(!config.use_d3d_manager);
        assert!(config.disable_throttling);
    }

    #[test]
    fn equal_dumps_do_not_emit() {
        assert!(!should_emit(&base_dump(), &base_dump()));
    }

    #[test]
    fn ignored_leaves_do_not_emit() {
        let base = base_dump();
        let mut next = base.clone();
        next.frame_rate = Some(2);
        next.segment_seconds_remaining = Some(119);
        next.pause.as_mut().unwrap().seconds_remaining = Some(899);
        next.screen_encoder.as_mut().unwrap().frames_consumed = 11;
        next.screen_encoder.as_mut().unwrap().samples_written = 21;
        assert!(!should_emit(&base, &next));

        let mut next = base.clone();
        next.frame_rate = None;
        assert!(!should_emit(&base, &next));

        let mut next = base.clone();
        next.pause.as_mut().unwrap().seconds_remaining = Some(1);
        assert!(!should_emit(&base, &next));

        let mut next = base.clone();
        next.screen_encoder.as_mut().unwrap().samples_written = 999;
        assert!(!should_emit(&base, &next));
    }

    #[test]
    fn meaningful_differences_emit() {
        let base = base_dump();

        let mut next = base.clone();
        next.app_state = AppPhase::Paused;
        assert!(should_emit(&base, &next));

        let mut next = base.clone();
        next.sources[0].state = SourceState::Faulted {
            reason: ErrorReason::EndpointLost,
            detail: "x".into(),
        };
        assert!(should_emit(&base, &next));

        let mut next = base.clone();
        next.sync.pairing.phase = PairingPhase::Pairing;
        assert!(should_emit(&base, &next));

        let mut next = base.clone();
        next.pause = None;
        assert!(should_emit(&base, &next));

        let mut next = base.clone();
        next.pause.as_mut().unwrap().reason = PauseReason::SessionLocked;
        assert!(should_emit(&base, &next));

        let mut next = base.clone();
        next.sync.upload.uploaded_segments += 1;
        assert!(should_emit(&base, &next));

        let mut next = base.clone();
        next.segment_dir = Some("segments/2026-07-01T12-05-00Z".into());
        assert!(should_emit(&base, &next));

        let mut next = base.clone();
        next.exclusions = Some(ExclusionHealth {
            rules_active: true,
            frames_redacted: 1,
            frames_dropped: 0,
        });
        assert!(should_emit(&base, &next));

        let mut previous = base.clone();
        previous.exclusions = Some(ExclusionHealth {
            rules_active: true,
            frames_redacted: 1,
            frames_dropped: 0,
        });
        let mut next = previous.clone();
        next.exclusions.as_mut().unwrap().frames_redacted = 2;
        assert!(should_emit(&previous, &next));

        let mut next = base.clone();
        next.screen_encoder = None;
        assert!(should_emit(&base, &next));

        let mut next = base.clone();
        next.screen_encoder.as_mut().unwrap().last_error = Some("encoder failed".into());
        assert!(should_emit(&base, &next));

        let mut next = base.clone();
        next.views
            .insert("settings".into(), ViewRenderState::Rendered);
        assert!(should_emit(&base, &next));
    }

    #[test]
    fn terminal_error_emits() {
        let base = base_dump();
        let mut terminal = base.clone();
        terminal.app_state = AppPhase::Error;

        assert!(should_emit(&base, &terminal));
    }

    #[test]
    fn health_dump_serializes_views() {
        let mut dump = HealthDump {
            app_state: AppPhase::Idle,
            sources: vec![],
            frame_rate: None,
            segment_dir: None,
            segment_seconds_remaining: None,
            engine_ready: false,
            version: "test".into(),
            sync: SyncSnapshot::default(),
            screen_encoder: None,
            exclusions: None,
            pause: None,
            views: BTreeMap::new(),
        };

        let empty_json = serde_json::to_string(&dump).unwrap();
        assert!(empty_json.contains("\"views\":{}"));

        dump.views
            .insert("settings".into(), ViewRenderState::Rendered);
        let json = serde_json::to_string(&dump).unwrap();
        assert!(json.contains("\"views\":{\"settings\":\"rendered\"}"));

        let round_trip: HealthDump = serde_json::from_str(&json).unwrap();
        assert_eq!(
            round_trip.views.get("settings"),
            Some(&ViewRenderState::Rendered)
        );
    }

    #[test]
    fn upload_status_failure_clamps_and_single_lines_reason() {
        let mut upload = UploadStatus::default();
        for _ in 0..100 {
            upload.record_failure("  http_500\nretry\tlater  ");
        }

        assert_eq!(upload.recent_error_count, RECENT_ERROR_COUNT_MAX);
        assert_eq!(
            upload.last_error_reason.as_deref(),
            Some("http_500 retry later")
        );
    }

    #[test]
    fn upload_status_failure_truncates_reason_by_chars() {
        let mut upload = UploadStatus::default();
        let reason = "é".repeat(LAST_ERROR_REASON_MAX_LEN + 5);

        upload.record_failure(&reason);

        let bounded = upload.last_error_reason.unwrap();
        assert_eq!(bounded.chars().count(), LAST_ERROR_REASON_MAX_LEN);
        assert!(bounded.len() > LAST_ERROR_REASON_MAX_LEN);
        assert!(bounded.chars().all(|c| c == 'é'));
    }

    #[test]
    fn upload_status_success_resets_consecutive_failure_signal() {
        let mut upload = UploadStatus::default();
        upload.record_failure("tls");

        upload.record_success(1_700_000_000_000);

        assert_eq!(upload.recent_error_count, 0);
        assert_eq!(upload.last_error_reason, None);
        assert_eq!(upload.last_successful_sync, Some(1_700_000_000_000));
    }

    #[test]
    fn transport_path_tokens_round_trip_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&TransportPath::Direct).unwrap(),
            "\"direct\""
        );
        assert_eq!(
            serde_json::to_string(&TransportPath::Relay).unwrap(),
            "\"relay\""
        );

        let direct: TransportPath = serde_json::from_str("\"direct\"").unwrap();
        let relay: TransportPath = serde_json::from_str("\"relay\"").unwrap();

        assert_eq!(direct, TransportPath::Direct);
        assert_eq!(direct.as_str(), "direct");
        assert_eq!(relay, TransportPath::Relay);
        assert_eq!(relay.as_str(), "relay");
    }

    #[test]
    fn upload_status_serializes_earned_last_upload_fields() {
        let upload = UploadStatus {
            pending_segments: 2,
            uploaded_segments: 3,
            failed_segments: 1,
            last_uploaded_segment: Some("120000_300".into()),
            last_error: None,
            last_successful_sync: Some(1_700_000_000_000),
            recent_error_count: 1,
            last_error_reason: Some("retry".into()),
            heartbeat_ok: true,
            last_upload_duration_ms: Some(42),
            last_upload_bytes: Some(12_345),
            last_upload_path: Some(TransportPath::Direct),
            last_upload_dial_attempts: Some(2),
        };

        let value = serde_json::to_value(&upload).unwrap();

        assert_eq!(value["pending_segments"], 2);
        assert_eq!(value["uploaded_segments"], 3);
        assert_eq!(value["failed_segments"], 1);
        assert_eq!(value["last_uploaded_segment"], "120000_300");
        assert_eq!(value["last_error"], serde_json::Value::Null);
        assert_eq!(value["last_successful_sync"], 1_700_000_000_000u64);
        assert_eq!(value["recent_error_count"], 1);
        assert_eq!(value["last_error_reason"], "retry");
        assert_eq!(value["heartbeat_ok"], true);
        assert_eq!(value["last_upload_duration_ms"], 42);
        assert_eq!(value["last_upload_bytes"], 12_345);
        assert_eq!(value["last_upload_path"], "direct");
        assert_eq!(value["last_upload_dial_attempts"], 2);

        let default_value = serde_json::to_value(UploadStatus::default()).unwrap();
        for key in [
            "last_upload_duration_ms",
            "last_upload_bytes",
            "last_upload_path",
            "last_upload_dial_attempts",
        ] {
            let Some(value) = default_value.get(key) else {
                continue;
            };
            assert!(value.is_null(), "{key} should default to null when present");
        }
    }

    #[test]
    fn view_render_state_tokens_are_snake_case() {
        assert_eq!(<&str>::from(ViewRenderState::Pending), "pending");
        assert_eq!(<&str>::from(ViewRenderState::Rendered), "rendered");
        assert_eq!(ViewRenderState::iter().count(), 2);
        assert_eq!(
            serde_json::to_string(&ViewRenderState::Rendered).unwrap(),
            "\"rendered\""
        );
    }

    #[test]
    fn view_parse_and_labels() {
        assert_eq!(View::parse("settings"), Some(View::Settings));
        assert_eq!(View::parse("about"), Some(View::About));
        assert_eq!(View::parse("bogus"), None);
        assert_eq!(View::parse(""), None);
        assert_eq!(View::Settings.label(), "settings");
        assert_eq!(View::valid_list(), "settings, about");
    }
}
