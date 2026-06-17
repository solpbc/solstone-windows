// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! WASAPI audio sources: render-loopback system audio and eCapture microphone.
//!
//! **Platform tier** — `windows-rs` quarantine; `unsafe` permitted here only.
//! This crate owns the honest [`SourceState::NoInputDevice`] determination: when
//! the machine has no microphone endpoint, the mic source reports
//! `NoInputDevice` (a first-class, non-error state), never a fake "active".
//!
//! Off-Windows, the same public source types are honest inert stubs so the Linux
//! dev host can compile and test the pure/composition tiers.

#[cfg(windows)]
mod imp {
    use std::ptr;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use observer_model::{
        CaptureChunk, CaptureSink, ErrorReason, MicSource, SourceError, SourceKind, SourceState,
        SystemAudioSource,
    };
    use windows::core::{Error as WindowsError, Result as WindowsResult};
    use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
    use windows::Win32::Media::Audio::{
        eCapture, eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDevice,
        IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT,
        AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, DEVICE_STATE_ACTIVE, WAVEFORMATEX,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
        COINIT_MULTITHREADED,
    };

    const PULL_INTERVAL: Duration = Duration::from_millis(100);
    const WASAPI_BUFFER_DURATION_100NS: i64 = 10_000_000;

    struct ComApartment {
        initialized: bool,
    }

    impl ComApartment {
        fn new() -> WindowsResult<Self> {
            let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            if hr == RPC_E_CHANGED_MODE {
                return Err(WindowsError::from(hr));
            }
            hr.ok()?;
            Ok(Self { initialized: true })
        }
    }

    impl Drop for ComApartment {
        fn drop(&mut self) {
            if self.initialized {
                unsafe { CoUninitialize() };
            }
        }
    }

    #[derive(Debug)]
    struct Worker {
        stop: Arc<AtomicBool>,
        join: Option<JoinHandle<()>>,
    }

    impl Worker {
        fn stop(mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
        }
    }

    fn set_state(state: &Arc<Mutex<SourceState>>, next: SourceState) {
        *state.lock().unwrap() = next;
    }

    fn default_device(
        enumerator: &IMMDeviceEnumerator,
        kind: SourceKind,
    ) -> WindowsResult<IMMDevice> {
        let flow = match kind {
            SourceKind::SystemAudio => eRender,
            SourceKind::Mic => eCapture,
            SourceKind::Screen => unreachable!("screen is not a WASAPI source"),
        };
        unsafe { enumerator.GetDefaultAudioEndpoint(flow, eConsole) }
    }

    fn bytes_per_frame(format: *const WAVEFORMATEX) -> usize {
        let format = unsafe { &*format };
        let channels = format.nChannels as usize;
        let bits = format.wBitsPerSample as usize;
        channels * bits / 8
    }

    fn has_input_device_result() -> WindowsResult<bool> {
        let _com = ComApartment::new()?;
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
        let endpoints = unsafe { enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)? };
        let count = unsafe { endpoints.GetCount()? };
        Ok(count > 0)
    }

    fn has_input_device() -> bool {
        has_input_device_result().unwrap_or(false)
    }

    fn run_pull_loop(
        kind: SourceKind,
        sink: Arc<dyn CaptureSink>,
        state: Arc<Mutex<SourceState>>,
        stop: Arc<AtomicBool>,
        seq: Arc<AtomicU64>,
        loopback: bool,
    ) -> WindowsResult<()> {
        let _com = ComApartment::new()?;
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
        if kind == SourceKind::Mic && !has_input_device_result()? {
            set_state(&state, SourceState::NoInputDevice);
            return Ok(());
        }

        let device = default_device(&enumerator, kind)?;
        let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None)? };
        let format = unsafe { client.GetMixFormat()? };
        let bpf = bytes_per_frame(format);
        if bpf == 0 {
            unsafe { CoTaskMemFree(Some(format.cast())) };
            return Err(WindowsError::from_win32());
        }

        let stream_flags = if loopback {
            AUDCLNT_STREAMFLAGS_LOOPBACK
        } else {
            0
        };
        let initialize = unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                stream_flags,
                WASAPI_BUFFER_DURATION_100NS,
                0,
                format,
                None,
            )
        };
        unsafe { CoTaskMemFree(Some(format.cast())) };
        initialize?;

        let capture: IAudioCaptureClient = unsafe { client.GetService()? };
        unsafe { client.Start()? };
        set_state(&state, SourceState::Active);

        while !stop.load(Ordering::Relaxed) {
            thread::sleep(PULL_INTERVAL);
            loop {
                let packet = unsafe { capture.GetNextPacketSize()? };
                if packet == 0 {
                    break;
                }

                let mut data_ptr: *mut u8 = ptr::null_mut();
                let mut frames: u32 = 0;
                let mut flags: u32 = 0;
                unsafe {
                    capture.GetBuffer(&mut data_ptr, &mut frames, &mut flags, None, None)?;
                }

                if frames > 0 {
                    let byte_len = frames as usize * bpf;
                    let data =
                        if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data_ptr.is_null() {
                            vec![0; byte_len]
                        } else {
                            unsafe { std::slice::from_raw_parts(data_ptr, byte_len) }.to_vec()
                        };
                    let seq = seq.fetch_add(1, Ordering::Relaxed);
                    sink.emit(CaptureChunk {
                        source: kind,
                        seq,
                        data,
                    });
                }
                unsafe { capture.ReleaseBuffer(frames)? };
            }
        }

        unsafe { client.Stop()? };
        set_state(&state, SourceState::Inactive);
        Ok(())
    }

    fn spawn_worker(
        kind: SourceKind,
        sink: Arc<dyn CaptureSink>,
        state: Arc<Mutex<SourceState>>,
        seq: Arc<AtomicU64>,
        loopback: bool,
    ) -> Worker {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker_state = Arc::clone(&state);
        let join = thread::spawn(move || {
            if let Err(err) = run_pull_loop(
                kind,
                sink,
                Arc::clone(&worker_state),
                worker_stop,
                seq,
                loopback,
            ) {
                set_state(
                    &worker_state,
                    SourceState::Faulted {
                        reason: ErrorReason::Unknown,
                        detail: err.to_string(),
                    },
                );
            }
        });
        Worker {
            stop,
            join: Some(join),
        }
    }

    /// WASAPI render-loopback system-audio source.
    #[derive(Debug)]
    pub struct WasapiSystemAudioSource {
        worker: Option<Worker>,
        state: Arc<Mutex<SourceState>>,
        seq: Arc<AtomicU64>,
    }

    impl Default for WasapiSystemAudioSource {
        fn default() -> Self {
            Self {
                worker: None,
                state: Arc::new(Mutex::new(SourceState::Inactive)),
                seq: Arc::new(AtomicU64::new(0)),
            }
        }
    }

    impl WasapiSystemAudioSource {
        pub fn new() -> Self {
            Self::default()
        }
    }

    impl SystemAudioSource for WasapiSystemAudioSource {
        fn start(&mut self, sink: Arc<dyn CaptureSink>) -> Result<(), SourceError> {
            self.stop();
            set_state(&self.state, SourceState::Inactive);
            self.worker = Some(spawn_worker(
                SourceKind::SystemAudio,
                sink,
                Arc::clone(&self.state),
                Arc::clone(&self.seq),
                true,
            ));
            Ok(())
        }

        fn stop(&mut self) {
            if let Some(worker) = self.worker.take() {
                worker.stop();
            }
            set_state(&self.state, SourceState::Inactive);
        }

        fn state(&self) -> SourceState {
            self.state.lock().unwrap().clone()
        }
    }

    /// WASAPI eCapture microphone source. Owns the no-mic case.
    #[derive(Debug)]
    pub struct WasapiMicSource {
        worker: Option<Worker>,
        state: Arc<Mutex<SourceState>>,
        seq: Arc<AtomicU64>,
    }

    impl Default for WasapiMicSource {
        fn default() -> Self {
            Self {
                worker: None,
                state: Arc::new(Mutex::new(SourceState::NoInputDevice)),
                seq: Arc::new(AtomicU64::new(0)),
            }
        }
    }

    impl WasapiMicSource {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn has_input_device(&self) -> bool {
            has_input_device()
        }
    }

    impl MicSource for WasapiMicSource {
        fn start(&mut self, sink: Arc<dyn CaptureSink>) -> Result<(), SourceError> {
            self.stop();
            if !self.has_input_device() {
                set_state(&self.state, SourceState::NoInputDevice);
                return Ok(());
            }
            set_state(&self.state, SourceState::Inactive);
            self.worker = Some(spawn_worker(
                SourceKind::Mic,
                sink,
                Arc::clone(&self.state),
                Arc::clone(&self.seq),
                false,
            ));
            Ok(())
        }

        fn stop(&mut self) {
            if let Some(worker) = self.worker.take() {
                worker.stop();
            }
            if self.has_input_device() {
                set_state(&self.state, SourceState::Inactive);
            } else {
                set_state(&self.state, SourceState::NoInputDevice);
            }
        }

        fn state(&self) -> SourceState {
            if !self.has_input_device() {
                SourceState::NoInputDevice
            } else {
                self.state.lock().unwrap().clone()
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::sync::Arc;

    use observer_model::{CaptureSink, MicSource, SourceError, SourceState, SystemAudioSource};

    /// WASAPI render-loopback system-audio source.
    #[derive(Debug, Default)]
    pub struct WasapiSystemAudioSource;

    impl WasapiSystemAudioSource {
        pub fn new() -> Self {
            Self
        }
    }

    impl SystemAudioSource for WasapiSystemAudioSource {
        fn start(&mut self, _sink: Arc<dyn CaptureSink>) -> Result<(), SourceError> {
            Ok(())
        }

        fn stop(&mut self) {}

        fn state(&self) -> SourceState {
            SourceState::Inactive
        }
    }

    /// WASAPI eCapture microphone source. Non-Windows stub reports no mic.
    #[derive(Debug, Default)]
    pub struct WasapiMicSource;

    impl WasapiMicSource {
        pub fn new() -> Self {
            Self
        }

        pub fn has_input_device(&self) -> bool {
            false
        }
    }

    impl MicSource for WasapiMicSource {
        fn start(&mut self, _sink: Arc<dyn CaptureSink>) -> Result<(), SourceError> {
            Ok(())
        }

        fn stop(&mut self) {}

        fn state(&self) -> SourceState {
            SourceState::NoInputDevice
        }
    }
}

pub use imp::{WasapiMicSource, WasapiSystemAudioSource};
