// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Windows.Graphics.Capture screen source.
//!
//! **Platform tier** — this is where the `windows-rs` quarantine and the only
//! permitted `unsafe` live. The crate's whole job is to implement the pure-tier
//! [`ScreenSource`](observer_model::ScreenSource) trait against WGC; the engine
//! is injected the resulting `dyn ScreenSource` and never sees a `windows` type.
//!
//! **Capture exclusions.** WGC captures the whole primary monitor and exposes no
//! per-window exclude (and `SetWindowDisplayAffinity` only governs a process's
//! *own* windows), so excluded surfaces are removed here in software: at each
//! frame the owner's [`ExclusionRules`] are evaluated against the windows present
//! on the captured monitor, and the frame is passed through, has the excluded
//! regions blacked out, or — when an excluded surface can't be safely redacted —
//! dropped whole. The policy + redaction live in the pure `observer-exclusion`
//! crate; this crate only supplies window facts (Win32 enumeration) and applies
//! the verdict to the owned frame buffer before it crosses the sink seam.
//!
//! Off-Windows, this crate exposes the same public source as an honest inert
//! stub so the Linux dev host can compile the workspace.

use std::sync::atomic::{AtomicU64, Ordering};

/// Per-frame capture-exclusion counters, shared between the WGC capture thread
/// (which increments them) and the health reporter (which snapshots them). It
/// lives in the platform crate because it is incremented at the capture seam;
/// only the pure [`observer_model::ExclusionHealth`] snapshot crosses the
/// `ScreenSource` trait, keeping exclusion activity visible in health (never
/// silent) without leaking a platform type into the engine.
#[derive(Debug, Default)]
pub struct ExclusionStats {
    frames_redacted: AtomicU64,
    frames_dropped: AtomicU64,
}

impl ExclusionStats {
    fn snapshot(&self, rules_active: bool) -> observer_model::ExclusionHealth {
        observer_model::ExclusionHealth {
            rules_active,
            frames_redacted: self.frames_redacted.load(Ordering::Relaxed),
            frames_dropped: self.frames_dropped.load(Ordering::Relaxed),
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, RwLock};
    use std::time::Duration;

    use observer_exclusion::{apply_redaction, evaluate, ExclusionDecision, ExclusionRules};
    use observer_model::{
        normalize_even, CaptureSink, ErrorReason, ExclusionHealth, ScreenFrame, ScreenPixelFormat,
        ScreenSource, SourceError, SourceState,
    };
    use windows_capture::capture::{CaptureControl, Context, GraphicsCaptureApiHandler};
    use windows_capture::frame::Frame;
    use windows_capture::graphics_capture_api::InternalCaptureControl;
    use windows_capture::monitor::Monitor;
    use windows_capture::settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    };

    use super::ExclusionStats;

    mod window_enum;

    pub use window_enum::{dump_primary_monitor_windows, list_running_apps};

    type HandlerError = String;

    // ~1 fps cap. At 1080p RGBA8 (~8.3 MB/frame), 1 fps * 300s is ~2.5 GB per
    // five-minute segment and ~15 GB per 30-minute soak; the encoder is deferred.
    const MINIMUM_UPDATE_INTERVAL: Duration = Duration::from_millis(1000);
    const SCREEN_COLOR_FORMAT: ColorFormat = ColorFormat::Rgba8;

    #[derive(Clone)]
    struct HandlerFlags {
        sink: Arc<dyn CaptureSink>,
        state: Arc<Mutex<SourceState>>,
        seq: Arc<AtomicU64>,
        color_format: ColorFormat,
        rules: Arc<RwLock<ExclusionRules>>,
        stats: Arc<ExclusionStats>,
    }

    struct WgcHandler {
        sink: Arc<dyn CaptureSink>,
        state: Arc<Mutex<SourceState>>,
        seq: Arc<AtomicU64>,
        color_format: ColorFormat,
        rules: Arc<RwLock<ExclusionRules>>,
        stats: Arc<ExclusionStats>,
        scratch: Vec<u8>,
    }

    impl WgcHandler {
        fn set_state(&self, state: SourceState) {
            *self.state.lock().unwrap() = state;
        }
    }

    impl GraphicsCaptureApiHandler for WgcHandler {
        type Flags = HandlerFlags;
        type Error = HandlerError;

        fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
            Ok(Self {
                sink: ctx.flags.sink,
                state: ctx.flags.state,
                seq: ctx.flags.seq,
                color_format: ctx.flags.color_format,
                rules: ctx.flags.rules,
                stats: ctx.flags.stats,
                scratch: Vec::new(),
            })
        }

        fn on_frame_arrived(
            &mut self,
            frame: &mut Frame,
            _capture_control: InternalCaptureControl,
        ) -> Result<(), Self::Error> {
            let pixel_format = match self.color_format {
                ColorFormat::Rgba8 => ScreenPixelFormat::Rgba8,
                ColorFormat::Bgra8 => ScreenPixelFormat::Bgra8,
                ColorFormat::Rgba16F => {
                    return Err("unsupported WGC color format Rgba16F".to_string());
                }
            };
            let width = frame.width();
            let height = frame.height();
            let frame_buffer = frame.buffer().map_err(|err| err.to_string())?;
            let mut data = frame_buffer.as_nopadding_buffer(&mut self.scratch).to_vec();

            // Capture exclusions, applied before the frame can become a segment.
            // The owner's rules drive an enumerate -> evaluate -> redact/drop pass;
            // anything that can't be redacted safely drops the whole frame (fail
            // closed). Skipped entirely when no rule is configured (no per-frame
            // cost). A dropped frame still keeps the source `Active` — the observer
            // is healthy, just excluding — and is counted into health.
            let rules = self.rules.read().map(|r| r.clone()).unwrap_or_default();
            if rules.is_active() {
                match window_enum::enumerate_primary_monitor_windows(width, height) {
                    Ok(windows) => match evaluate(&rules, &windows) {
                        ExclusionDecision::Pass => {}
                        ExclusionDecision::Redact(rects) => {
                            apply_redaction(&mut data, width, height, pixel_format, &rects);
                            self.stats.frames_redacted.fetch_add(1, Ordering::Relaxed);
                        }
                        ExclusionDecision::Drop => {
                            self.stats.frames_dropped.fetch_add(1, Ordering::Relaxed);
                            self.set_state(SourceState::Active);
                            return Ok(());
                        }
                    },
                    Err(()) => {
                        // We can't prove the frame is clean — fail closed.
                        self.stats.frames_dropped.fetch_add(1, Ordering::Relaxed);
                        self.set_state(SourceState::Active);
                        return Ok(());
                    }
                }
            }

            let seq = self.seq.fetch_add(1, Ordering::Relaxed);
            let frame = normalize_even(&ScreenFrame {
                seq,
                width,
                height,
                pixel_format,
                pixels: Arc::from(data),
            });
            self.sink.emit_screen_frame(frame);
            self.set_state(SourceState::Active);
            Ok(())
        }

        fn on_closed(&mut self) -> Result<(), Self::Error> {
            self.set_state(SourceState::Faulted {
                reason: ErrorReason::EndpointLost,
                detail: "graphics capture item closed".to_string(),
            });
            Ok(())
        }
    }

    /// WGC-backed screen source.
    pub struct WgcScreenSource {
        control: Option<CaptureControl<WgcHandler, HandlerError>>,
        state: Arc<Mutex<SourceState>>,
        last_sink: Option<Arc<dyn CaptureSink>>,
        seq: Arc<AtomicU64>,
        rules: Arc<RwLock<ExclusionRules>>,
        stats: Arc<ExclusionStats>,
    }

    impl WgcScreenSource {
        /// Construct over the shared exclusion-rules handle. The owner edits the
        /// rules over IPC (writing the same `Arc`), so changes take effect on the
        /// next captured frame without a restart.
        pub fn new(rules: Arc<RwLock<ExclusionRules>>) -> Self {
            Self {
                control: None,
                state: Arc::new(Mutex::new(SourceState::Inactive)),
                last_sink: None,
                seq: Arc::new(AtomicU64::new(0)),
                rules,
                stats: Arc::new(ExclusionStats::default()),
            }
        }

        fn set_state(&self, state: SourceState) {
            *self.state.lock().unwrap() = state;
        }

        fn source_error(detail: impl Into<String>) -> SourceError {
            SourceError::new(ErrorReason::Unknown, detail)
        }

        fn settings(
            &self,
            sink: Arc<dyn CaptureSink>,
        ) -> Result<Settings<HandlerFlags, Monitor>, SourceError> {
            let monitor = Monitor::primary().map_err(|err| Self::source_error(err.to_string()))?;
            Ok(Settings::new(
                monitor,
                CursorCaptureSettings::Default,
                DrawBorderSettings::WithoutBorder,
                SecondaryWindowSettings::Default,
                MinimumUpdateIntervalSettings::Custom(MINIMUM_UPDATE_INTERVAL),
                DirtyRegionSettings::Default,
                SCREEN_COLOR_FORMAT,
                HandlerFlags {
                    sink,
                    state: Arc::clone(&self.state),
                    seq: Arc::clone(&self.seq),
                    color_format: SCREEN_COLOR_FORMAT,
                    rules: Arc::clone(&self.rules),
                    stats: Arc::clone(&self.stats),
                },
            ))
        }
    }

    impl ScreenSource for WgcScreenSource {
        fn start(&mut self, sink: Arc<dyn CaptureSink>) -> Result<(), SourceError> {
            self.stop();
            self.last_sink = Some(Arc::clone(&sink));
            self.set_state(SourceState::Inactive);
            let settings = self.settings(sink)?;
            match WgcHandler::start_free_threaded(settings) {
                Ok(control) => {
                    self.control = Some(control);
                    Ok(())
                }
                Err(err) => {
                    let error = Self::source_error(err.to_string());
                    self.set_state(SourceState::Faulted {
                        reason: error.reason,
                        detail: error.detail.clone(),
                    });
                    Err(error)
                }
            }
        }

        fn stop(&mut self) {
            if let Some(control) = self.control.take() {
                let _ = control.stop();
            }
            self.set_state(SourceState::Inactive);
        }

        /// A running WGC session reports `Active`. The capture API is
        /// change-driven, so a static screen delivering no frames is expected and
        /// healthy; there is no frame-freshness heuristic. Accepted residual: a
        /// silently-wedged session that never fires `on_closed()` reports `Active`
        /// indefinitely. A silent-stall watchdog is deferred (YAGNI), and the old
        /// freshness signal did not usefully cover this because it false-fired on
        /// every healthy static screen.
        fn state(&self) -> SourceState {
            self.state.lock().unwrap().clone()
        }

        fn on_display_changed(&mut self) {
            let Some(sink) = self.last_sink.clone() else {
                self.set_state(SourceState::Inactive);
                return;
            };
            if let Err(error) = self.start(sink) {
                self.set_state(SourceState::Faulted {
                    reason: error.reason,
                    detail: error.detail,
                });
            }
        }

        fn exclusion_health(&self) -> Option<ExclusionHealth> {
            let active = self.rules.read().map(|r| r.is_active()).unwrap_or(false);
            Some(self.stats.snapshot(active))
        }
    }

    #[cfg(all(windows, test))]
    mod tests {
        use super::*;

        struct FakeSink;

        impl CaptureSink for FakeSink {
            fn emit(&self, _chunk: observer_model::CaptureChunk) {}

            fn emit_screen_frame(&self, _frame: ScreenFrame) {}
        }

        #[test]
        fn on_closed_faults_with_endpoint_lost() {
            let state = Arc::new(Mutex::new(SourceState::Inactive));
            let mut handler = WgcHandler {
                sink: Arc::new(FakeSink),
                state: Arc::clone(&state),
                seq: Arc::new(AtomicU64::new(0)),
                color_format: SCREEN_COLOR_FORMAT,
                rules: Arc::new(RwLock::new(ExclusionRules::default())),
                stats: Arc::new(ExclusionStats::default()),
                scratch: Vec::new(),
            };

            handler.on_closed().unwrap();

            let got = state.lock().unwrap().clone();
            assert_eq!(
                got,
                SourceState::Faulted {
                    reason: ErrorReason::EndpointLost,
                    detail: "graphics capture item closed".to_string(),
                }
            );
        }

        #[test]
        fn stop_transitions_active_source_to_inactive() {
            let mut src = WgcScreenSource::new(Arc::new(RwLock::new(ExclusionRules::default())));
            *src.state.lock().unwrap() = SourceState::Active;

            src.stop();

            assert_eq!(src.state(), SourceState::Inactive);
        }

        #[test]
        fn fresh_source_without_frames_reports_inactive() {
            let src = WgcScreenSource::new(Arc::new(RwLock::new(ExclusionRules::default())));

            assert_eq!(src.state(), SourceState::Inactive);
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::sync::{Arc, RwLock};

    use observer_exclusion::{ExclusionRules, RunningApp};
    use observer_model::{CaptureSink, ScreenSource, SourceError, SourceState};

    /// Non-Windows stub: no windows to enumerate.
    pub fn list_running_apps() -> Vec<RunningApp> {
        Vec::new()
    }

    /// Non-Windows stub: no windows to enumerate.
    pub fn dump_primary_monitor_windows() -> Vec<observer_exclusion::WindowInfo> {
        Vec::new()
    }

    /// WGC-backed screen source. Non-Windows stub: never produces frames.
    pub struct WgcScreenSource {
        _rules: Arc<RwLock<ExclusionRules>>,
    }

    impl WgcScreenSource {
        pub fn new(rules: Arc<RwLock<ExclusionRules>>) -> Self {
            Self { _rules: rules }
        }
    }

    impl ScreenSource for WgcScreenSource {
        fn start(&mut self, _sink: Arc<dyn CaptureSink>) -> Result<(), SourceError> {
            Ok(())
        }

        fn stop(&mut self) {}

        fn state(&self) -> SourceState {
            SourceState::Inactive
        }

        fn on_display_changed(&mut self) {}
    }
}

pub use imp::{dump_primary_monitor_windows, list_running_apps, WgcScreenSource};
