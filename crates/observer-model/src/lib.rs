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

use serde::{Deserialize, Serialize};
use strum::{EnumIter, IntoStaticStr};

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
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, EnumIter, IntoStaticStr,
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
}

// ── Source traits ────────────────────────────────────────────────────────────
// The pure tier defines the *seams*; the platform tier (capture-wgc /
// capture-wasapi) implements them, and `capture-engine` is injected the
// concrete `dyn` impls. So the engine is host-testable with fakes on Linux.

/// A screen capture source (implemented by `capture-wgc` on the build box).
pub trait ScreenSource: Send {
    /// Begin producing screen frames.
    fn start(&mut self) -> Result<(), SourceError>;
    /// Stop producing.
    fn stop(&mut self);
    /// The current honestly-reported state.
    fn state(&self) -> SourceState;
}

/// A system-audio (render loopback) source (implemented by `capture-wasapi`).
pub trait SystemAudioSource: Send {
    fn start(&mut self) -> Result<(), SourceError>;
    fn stop(&mut self);
    fn state(&self) -> SourceState;
}

/// A microphone (eCapture) source (implemented by `capture-wasapi`). Owns the
/// [`SourceState::NoInputDevice`] determination when no mic endpoint exists.
pub trait MicSource: Send {
    fn start(&mut self) -> Result<(), SourceError>;
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
