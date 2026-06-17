// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Windows.Graphics.Capture screen source.
//!
//! **Platform tier** — this is where the `windows-rs` quarantine and the only
//! permitted `unsafe` live. The crate's whole job is to implement the pure-tier
//! [`ScreenSource`](observer_model::ScreenSource) trait against WGC; the engine
//! is injected the resulting `dyn ScreenSource` and never sees a `windows` type.
//!
//! Off-Windows, this crate exposes the same public source as an honest inert
//! stub so the Linux dev host can compile the workspace.

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use observer_model::{
        CaptureSink, ErrorReason, ScreenFrame, ScreenPixelFormat, ScreenSource, SourceError,
        SourceState,
    };
    use windows_capture::capture::{CaptureControl, Context, GraphicsCaptureApiHandler};
    use windows_capture::frame::Frame;
    use windows_capture::graphics_capture_api::InternalCaptureControl;
    use windows_capture::monitor::Monitor;
    use windows_capture::settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    };

    type HandlerError = String;

    const FRAME_FRESHNESS: Duration = Duration::from_secs(3);
    // ~1 fps cap. At 1080p RGBA8 (~8.3 MB/frame), 1 fps * 300s is ~2.5 GB per
    // five-minute segment and ~15 GB per 30-minute soak; the encoder is deferred.
    const MINIMUM_UPDATE_INTERVAL: Duration = Duration::from_millis(1000);
    const SCREEN_COLOR_FORMAT: ColorFormat = ColorFormat::Rgba8;

    struct SharedState {
        state: SourceState,
        last_frame: Option<Instant>,
    }

    impl Default for SharedState {
        fn default() -> Self {
            Self {
                state: SourceState::Inactive,
                last_frame: None,
            }
        }
    }

    #[derive(Clone)]
    struct HandlerFlags {
        sink: Arc<dyn CaptureSink>,
        state: Arc<Mutex<SharedState>>,
        seq: Arc<AtomicU64>,
        color_format: ColorFormat,
    }

    struct WgcHandler {
        sink: Arc<dyn CaptureSink>,
        state: Arc<Mutex<SharedState>>,
        seq: Arc<AtomicU64>,
        color_format: ColorFormat,
        scratch: Vec<u8>,
    }

    impl WgcHandler {
        fn set_state(&self, state: SourceState, last_frame: Option<Instant>) {
            let mut guard = self.state.lock().unwrap();
            guard.state = state;
            guard.last_frame = last_frame;
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
            let data = frame_buffer.as_nopadding_buffer(&mut self.scratch).to_vec();
            let seq = self.seq.fetch_add(1, Ordering::Relaxed);
            self.sink.emit_screen_frame(ScreenFrame {
                seq,
                width,
                height,
                pixel_format,
                pixels: Arc::from(data),
            });
            self.set_state(SourceState::Active, Some(Instant::now()));
            Ok(())
        }

        fn on_closed(&mut self) -> Result<(), Self::Error> {
            self.set_state(
                SourceState::Faulted {
                    reason: ErrorReason::EndpointLost,
                    detail: "graphics capture item closed".to_string(),
                },
                None,
            );
            Ok(())
        }
    }

    /// WGC-backed screen source.
    pub struct WgcScreenSource {
        control: Option<CaptureControl<WgcHandler, HandlerError>>,
        state: Arc<Mutex<SharedState>>,
        last_sink: Option<Arc<dyn CaptureSink>>,
        seq: Arc<AtomicU64>,
    }

    impl Default for WgcScreenSource {
        fn default() -> Self {
            Self {
                control: None,
                state: Arc::new(Mutex::new(SharedState::default())),
                last_sink: None,
                seq: Arc::new(AtomicU64::new(0)),
            }
        }
    }

    impl WgcScreenSource {
        pub fn new() -> Self {
            Self::default()
        }

        fn set_state(&self, state: SourceState, last_frame: Option<Instant>) {
            let mut guard = self.state.lock().unwrap();
            guard.state = state;
            guard.last_frame = last_frame;
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
                },
            ))
        }
    }

    impl ScreenSource for WgcScreenSource {
        fn start(&mut self, sink: Arc<dyn CaptureSink>) -> Result<(), SourceError> {
            self.stop();
            self.last_sink = Some(Arc::clone(&sink));
            self.set_state(SourceState::Inactive, None);
            let settings = self.settings(sink)?;
            match WgcHandler::start_free_threaded(settings) {
                Ok(control) => {
                    self.control = Some(control);
                    Ok(())
                }
                Err(err) => {
                    let error = Self::source_error(err.to_string());
                    self.set_state(
                        SourceState::Faulted {
                            reason: error.reason,
                            detail: error.detail.clone(),
                        },
                        None,
                    );
                    Err(error)
                }
            }
        }

        fn stop(&mut self) {
            if let Some(control) = self.control.take() {
                let _ = control.stop();
            }
            self.set_state(SourceState::Inactive, None);
        }

        fn state(&self) -> SourceState {
            let guard = self.state.lock().unwrap();
            match &guard.state {
                SourceState::Active => {
                    if guard
                        .last_frame
                        .is_some_and(|last_frame| last_frame.elapsed() <= FRAME_FRESHNESS)
                    {
                        SourceState::Active
                    } else {
                        SourceState::Inactive
                    }
                }
                state => state.clone(),
            }
        }

        fn on_display_changed(&mut self) {
            let Some(sink) = self.last_sink.clone() else {
                self.set_state(SourceState::Inactive, None);
                return;
            };
            if let Err(error) = self.start(sink) {
                self.set_state(
                    SourceState::Faulted {
                        reason: error.reason,
                        detail: error.detail,
                    },
                    None,
                );
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::sync::Arc;

    use observer_model::{CaptureSink, ScreenSource, SourceError, SourceState};

    /// WGC-backed screen source. Non-Windows stub: never produces frames.
    #[derive(Debug, Default)]
    pub struct WgcScreenSource;

    impl WgcScreenSource {
        pub fn new() -> Self {
            Self
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

pub use imp::WgcScreenSource;
