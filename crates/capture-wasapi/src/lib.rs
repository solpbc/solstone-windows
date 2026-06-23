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
    use std::sync::{Arc, Mutex, RwLock};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use observer_mic::{apply_gain_f32, apply_gain_i16, MicConfig, MicDeviceRef};
    use observer_model::{
        CaptureChunk, CaptureSink, ErrorReason, MicSource, SourceError, SourceKind, SourceState,
        SystemAudioSource,
    };
    use windows::core::{Error as WindowsError, Result as WindowsResult, PCWSTR};
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
    use windows::Win32::Media::Audio::{
        eCapture, eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDevice,
        IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT,
        AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, DEVICE_STATE_ACTIVE, WAVEFORMATEX,
    };
    use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
        COINIT_MULTITHREADED, STGM_READ,
    };

    /// How often the mic loop re-enumerates devices + re-applies the owner's
    /// selection policy while running, so a device unplug or a Settings change
    /// takes effect without an app restart. Cheap relative to the pull cadence.
    const MIC_RESELECT_INTERVAL: Duration = Duration::from_millis(1000);

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

    fn set_active(active: &Arc<Mutex<Option<String>>>, id: Option<String>) {
        if let Ok(mut guard) = active.lock() {
            *guard = id;
        }
    }

    /// A device's friendly name, with a generic fallback so enumeration never
    /// fails on a property-read hiccup.
    fn device_friendly_name(device: &IMMDevice) -> String {
        unsafe {
            let Ok(store) = device.OpenPropertyStore(STGM_READ) else {
                return "Microphone".to_string();
            };
            let Ok(prop) = store.GetValue(&PKEY_Device_FriendlyName) else {
                return "Microphone".to_string();
            };
            match PropVariantToStringAlloc(&prop) {
                Ok(pwstr) => {
                    let name = pwstr.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(pwstr.0 as *const _));
                    if name.is_empty() {
                        "Microphone".to_string()
                    } else {
                        name
                    }
                }
                Err(_) => "Microphone".to_string(),
            }
        }
    }

    /// Enumerate the active input (eCapture) endpoints as id + friendly-name refs.
    fn enumerate_mic_devices(enumerator: &IMMDeviceEnumerator) -> WindowsResult<Vec<MicDeviceRef>> {
        let mut out = Vec::new();
        unsafe {
            let collection = enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
            let count = collection.GetCount()?;
            for i in 0..count {
                let Ok(device) = collection.Item(i) else {
                    continue;
                };
                let Ok(id_pw) = device.GetId() else {
                    continue;
                };
                let id = id_pw.to_string().unwrap_or_default();
                CoTaskMemFree(Some(id_pw.0 as *const _));
                if id.is_empty() {
                    continue;
                }
                let name = device_friendly_name(&device);
                out.push(MicDeviceRef { id, name });
            }
        }
        Ok(out)
    }

    /// The input devices the owner can prioritize / disable. COM-initialized per
    /// call (invoked from the IPC thread, not the capture thread).
    pub fn list_mic_devices() -> Vec<MicDeviceRef> {
        (|| -> WindowsResult<Vec<MicDeviceRef>> {
            let _com = ComApartment::new()?;
            let enumerator: IMMDeviceEnumerator =
                unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
            enumerate_mic_devices(&enumerator)
        })()
        .unwrap_or_default()
    }

    /// Copy a captured buffer, applying the owner's gain. Reads the (aligned)
    /// device buffer as typed samples — never casts our own unaligned `Vec<u8>`.
    /// Unknown sample widths pass through ungained (mix format is 32-bit float).
    unsafe fn gained_bytes(data_ptr: *const u8, byte_len: usize, bits: u16, mult: f32) -> Vec<u8> {
        if mult == 1.0 {
            return std::slice::from_raw_parts(data_ptr, byte_len).to_vec();
        }
        match bits {
            32 => {
                let src = std::slice::from_raw_parts(data_ptr as *const f32, byte_len / 4);
                let mut buf = src.to_vec();
                apply_gain_f32(&mut buf, mult);
                buf.iter().flat_map(|s| s.to_le_bytes()).collect()
            }
            16 => {
                let src = std::slice::from_raw_parts(data_ptr as *const i16, byte_len / 2);
                let mut buf = src.to_vec();
                apply_gain_i16(&mut buf, mult);
                buf.iter().flat_map(|s| s.to_le_bytes()).collect()
            }
            _ => std::slice::from_raw_parts(data_ptr, byte_len).to_vec(),
        }
    }

    /// The config-aware microphone capture loop. Selects the owner's preferred
    /// present-and-enabled device, applies input gain to each buffer, and — on a
    /// modest cadence — re-enumerates + re-selects so a device unplug or a Settings
    /// change takes effect without an app restart. Honest state throughout:
    /// `NoInputDevice` when no hardware exists, `Inactive` when every device is
    /// disabled by the owner, `Active` only while truly streaming.
    fn run_mic_loop(
        sink: Arc<dyn CaptureSink>,
        state: Arc<Mutex<SourceState>>,
        stop: Arc<AtomicBool>,
        seq: Arc<AtomicU64>,
        config: Arc<RwLock<MicConfig>>,
        active: Arc<Mutex<Option<String>>>,
    ) -> WindowsResult<()> {
        let _com = ComApartment::new()?;
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };

        while !stop.load(Ordering::Relaxed) {
            let devices = enumerate_mic_devices(&enumerator).unwrap_or_default();
            if devices.is_empty() {
                set_state(&state, SourceState::NoInputDevice);
                set_active(&active, None);
                thread::sleep(MIC_RESELECT_INTERVAL);
                continue;
            }
            let cfg = config.read().map(|c| c.clone()).unwrap_or_default();
            let Some(selected) = cfg.select(&devices).cloned() else {
                // Devices exist but the owner disabled them all -> not producing.
                set_state(&state, SourceState::Inactive);
                set_active(&active, None);
                thread::sleep(MIC_RESELECT_INTERVAL);
                continue;
            };

            let wide_id: Vec<u16> = selected.id.encode_utf16().chain(Some(0)).collect();
            let device = match unsafe { enumerator.GetDevice(PCWSTR(wide_id.as_ptr())) } {
                Ok(d) => d,
                Err(_) => {
                    thread::sleep(MIC_RESELECT_INTERVAL);
                    continue;
                }
            };
            let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None)? };
            let format = unsafe { client.GetMixFormat()? };
            let bpf = bytes_per_frame(format);
            let bits = unsafe { (*format).wBitsPerSample };
            if bpf == 0 {
                unsafe { CoTaskMemFree(Some(format.cast())) };
                thread::sleep(MIC_RESELECT_INTERVAL);
                continue;
            }
            let init = unsafe {
                client.Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    0,
                    WASAPI_BUFFER_DURATION_100NS,
                    0,
                    format,
                    None,
                )
            };
            unsafe { CoTaskMemFree(Some(format.cast())) };
            init?;
            let capture: IAudioCaptureClient = unsafe { client.GetService()? };
            unsafe { client.Start()? };
            set_state(&state, SourceState::Active);
            set_active(&active, Some(selected.id.clone()));

            let mut since_reselect = Duration::ZERO;
            'inner: while !stop.load(Ordering::Relaxed) {
                thread::sleep(PULL_INTERVAL);
                since_reselect += PULL_INTERVAL;
                let mult = config.read().map(|c| c.gain_multiplier()).unwrap_or(1.0);
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
                        let data = if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0
                            || data_ptr.is_null()
                        {
                            vec![0; byte_len]
                        } else {
                            unsafe { gained_bytes(data_ptr, byte_len, bits, mult) }
                        };
                        let seq = seq.fetch_add(1, Ordering::Relaxed);
                        sink.emit(CaptureChunk {
                            source: SourceKind::Mic,
                            seq,
                            data,
                        });
                    }
                    unsafe { capture.ReleaseBuffer(frames)? };
                }
                if since_reselect >= MIC_RESELECT_INTERVAL {
                    since_reselect = Duration::ZERO;
                    let devices = enumerate_mic_devices(&enumerator).unwrap_or_default();
                    let cfg = config.read().map(|c| c.clone()).unwrap_or_default();
                    let now = cfg.select(&devices).map(|d| d.id.clone());
                    if now.as_deref() != Some(selected.id.as_str()) {
                        break 'inner; // selection changed -> reopen
                    }
                }
            }
            unsafe {
                let _ = client.Stop();
            }
        }

        set_state(&state, SourceState::Inactive);
        set_active(&active, None);
        Ok(())
    }

    fn spawn_mic_worker(
        sink: Arc<dyn CaptureSink>,
        state: Arc<Mutex<SourceState>>,
        seq: Arc<AtomicU64>,
        config: Arc<RwLock<MicConfig>>,
        active: Arc<Mutex<Option<String>>>,
    ) -> Worker {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker_state = Arc::clone(&state);
        let join = thread::spawn(move || {
            if let Err(err) = run_mic_loop(
                sink,
                Arc::clone(&worker_state),
                worker_stop,
                seq,
                config,
                active,
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

    /// WASAPI eCapture microphone source. Config-aware: it opens the owner's
    /// selected device (priority + disable) and applies input gain, reconciling
    /// both live. Owns the honest no-mic case via the loop's state reporting.
    #[derive(Debug)]
    pub struct WasapiMicSource {
        worker: Option<Worker>,
        state: Arc<Mutex<SourceState>>,
        seq: Arc<AtomicU64>,
        config: Arc<RwLock<MicConfig>>,
        active: Arc<Mutex<Option<String>>>,
    }

    impl WasapiMicSource {
        /// `config` is the shared owner policy (the IPC controller writes it); the
        /// loop reconciles selection + gain from it live. `active` is where the loop
        /// publishes the id of the device it actually opened (read by Settings).
        pub fn new(config: Arc<RwLock<MicConfig>>, active: Arc<Mutex<Option<String>>>) -> Self {
            Self {
                worker: None,
                state: Arc::new(Mutex::new(SourceState::NoInputDevice)),
                seq: Arc::new(AtomicU64::new(0)),
                config,
                active,
            }
        }

        pub fn has_input_device(&self) -> bool {
            has_input_device()
        }
    }

    impl MicSource for WasapiMicSource {
        fn start(&mut self, sink: Arc<dyn CaptureSink>) -> Result<(), SourceError> {
            self.stop();
            // Always spawn the config-aware loop — it reports NoInputDevice when no
            // hardware exists and picks up a later hot-plug on its own cadence.
            set_state(&self.state, SourceState::Inactive);
            self.worker = Some(spawn_mic_worker(
                sink,
                Arc::clone(&self.state),
                Arc::clone(&self.seq),
                Arc::clone(&self.config),
                Arc::clone(&self.active),
            ));
            Ok(())
        }

        fn stop(&mut self) {
            if let Some(worker) = self.worker.take() {
                worker.stop();
            }
            set_state(&self.state, SourceState::Inactive);
            set_active(&self.active, None);
        }

        fn state(&self) -> SourceState {
            self.state.lock().unwrap().clone()
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::sync::{Arc, Mutex, RwLock};

    use observer_mic::{MicConfig, MicDeviceRef};
    use observer_model::{CaptureSink, MicSource, SourceError, SourceState, SystemAudioSource};

    /// Off-Windows: no input devices to enumerate.
    pub fn list_mic_devices() -> Vec<MicDeviceRef> {
        Vec::new()
    }

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
        pub fn new(_config: Arc<RwLock<MicConfig>>, _active: Arc<Mutex<Option<String>>>) -> Self {
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

pub use imp::{list_mic_devices, WasapiMicSource, WasapiSystemAudioSource};
