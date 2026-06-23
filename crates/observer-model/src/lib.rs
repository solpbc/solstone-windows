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

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use strum::{EnumIter, IntoStaticStr};

/// Final screen media filename inside a sealed segment.
pub const SCREEN_FILE_NAME: &str = "display_1_screen.mp4";

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
    pub width: u32,
    pub height: u32,
    pub pixel_format: ScreenPixelFormat,
    pub pixels: Arc<[u8]>,
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
/// journal confirms it (reconcile by sha256), never on optimistic send.
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
    /// Whether the most recent heartbeat to the journal succeeded.
    pub heartbeat_ok: bool,
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
}
