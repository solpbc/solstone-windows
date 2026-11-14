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

#[cfg_attr(not(windows), allow(dead_code))]
mod reselect {
    /// Pure copy of the Windows `AUDCLNT_E_DEVICE_INVALIDATED` HRESULT,
    /// drift-guarded by a cfg(windows) test.
    pub(crate) const AUDCLNT_E_DEVICE_INVALIDATED_CODE: i32 = 0x8889_0004u32 as i32;

    /// Reopen when the opened render endpoint id differs from the current
    /// default. None-vs-None means no reopen because neither side provided an id.
    pub(crate) fn render_reopen_needed(
        opened_id: Option<&str>,
        current_default_id: Option<&str>,
    ) -> bool {
        opened_id != current_default_id
    }

    /// An `AUDCLNT_E_DEVICE_INVALIDATED`-class HRESULT is a render-endpoint
    /// reselect trigger, not a terminal capture fault.
    pub(crate) fn is_render_reselect_error(code: i32) -> bool {
        code == AUDCLNT_E_DEVICE_INVALIDATED_CODE
    }

    /// Pure copy of the Windows `E_ACCESSDENIED` HRESULT, drift-guarded by a
    /// cfg(windows) test.
    pub(crate) const E_ACCESSDENIED_CODE: i32 = 0x8007_0005u32 as i32;

    /// Windows privacy settings refusing the microphone surface as
    /// `E_ACCESSDENIED` while the stream is set up. Only the owner can lift
    /// that, so the source waits it out instead of faulting.
    pub(crate) fn is_access_denied(code: i32) -> bool {
        code == E_ACCESSDENIED_CODE
    }

    /// Where Windows keeps its microphone privacy choices: the device-wide and
    /// per-user "Microphone access" switches, and "Let desktop apps access your
    /// microphone" (`NonPackaged`).
    pub(crate) const MIC_CONSENT_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone";
    pub(crate) const MIC_CONSENT_DESKTOP_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone\NonPackaged";

    /// A desktop app's stream keeps opening while Windows refuses the
    /// microphone; it just carries silence. So the refusal is read from the
    /// switches themselves: any one set to `Deny` refuses.
    pub(crate) fn consent_refuses(values: &[Option<String>]) -> bool {
        values.iter().any(|value| value.as_deref() == Some("Deny"))
    }

    /// When the first frame of a packet was taken, on the monotonic clock.
    ///
    /// A pull drains every packet queued since the last one, so the moment of
    /// the pull is not when each packet's audio happened. Windows gives each
    /// packet the performance-counter time of its first frame, in 100 ns units;
    /// the packet happened that long before `now`. A position of zero, or one
    /// ahead of `now`, says nothing, and the packet is stamped `now`.
    pub(crate) fn packet_start_us(now_us: u64, now_100ns: u64, packet_100ns: u64) -> u64 {
        if packet_100ns == 0 || packet_100ns > now_100ns {
            return now_us;
        }
        now_us.saturating_sub((now_100ns - packet_100ns) / 10)
    }

    /// The running microphone reads Windows' privacy switches on every fifth
    /// one-second tick, and as soon as a pull is refused.
    pub(crate) const CONSENT_EVERY_TICKS: u32 = 5;

    /// What the running microphone looks at on one of its one-second ticks.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) struct MicChecks {
        /// List the devices and apply the owner's choice again.
        pub reselect: bool,
        /// Read the privacy switches.
        pub consent: bool,
    }

    /// Counts the running microphone's ticks. Devices are listed again only
    /// when Windows reported a device change or the owner's choice changed.
    #[derive(Debug, Default)]
    pub(crate) struct MicTicks {
        ticks: u32,
    }

    impl MicTicks {
        pub(crate) fn next(&mut self, devices_changed: bool, config_changed: bool) -> MicChecks {
            self.ticks = self.ticks.wrapping_add(1);
            MicChecks {
                reselect: devices_changed || config_changed,
                consent: self.ticks.is_multiple_of(CONSENT_EVERY_TICKS),
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_steady_microphone_lists_devices_only_on_a_change_and_reads_consent_every_five_seconds()
        {
            let mut ticks = MicTicks::default();
            let checks: Vec<MicChecks> = (0..60).map(|_| ticks.next(false, false)).collect();
            assert_eq!(checks.iter().filter(|c| c.reselect).count(), 0);
            assert_eq!(checks.iter().filter(|c| c.consent).count(), 12);
            assert!(ticks.next(true, false).reselect, "a device change");
            assert!(
                ticks.next(false, true).reselect,
                "the owner's choice changed"
            );
        }

        #[test]
        fn packets_drained_in_one_pull_keep_their_own_times() {
            // Ten 10 ms packets drained by one pull at 100 ms: each is stamped
            // when its audio began, so together they cover the whole 100 ms.
            let now_us = 5_000_000;
            let now_100ns = 90_000_000;
            let mut covered = 0;
            let mut end = 0;
            for packet in 0..10u64 {
                let at_100ns = now_100ns - 1_000_000 + packet * 100_000;
                let start = packet_start_us(now_us, now_100ns, at_100ns);
                assert_eq!(start, now_us - 100_000 + packet * 10_000);
                covered += (start + 10_000).saturating_sub(start.max(end));
                end = end.max(start + 10_000);
            }
            assert_eq!(covered, 100_000);
            assert_eq!(packet_start_us(now_us, now_100ns, 0), now_us);
            assert_eq!(packet_start_us(now_us, now_100ns, now_100ns + 1), now_us);
        }

        #[test]
        fn render_reopen_needed_tracks_default_identity() {
            assert!(!render_reopen_needed(Some("a"), Some("a")));
            assert!(render_reopen_needed(Some("a"), Some("b")));
            assert!(render_reopen_needed(Some("a"), None));
            assert!(!render_reopen_needed(None, None));
        }

        #[test]
        fn any_deny_switch_refuses_the_microphone() {
            assert!(!consent_refuses(&[None, None, None]));
            assert!(!consent_refuses(&[
                Some("Allow".into()),
                None,
                Some("Allow".into())
            ]));
            assert!(consent_refuses(&[
                Some("Allow".into()),
                None,
                Some("Deny".into())
            ]));
            assert!(consent_refuses(&[
                Some("Deny".into()),
                Some("Allow".into()),
                None
            ]));
        }

        #[test]
        fn access_denied_only_matches_e_accessdenied() {
            assert!(is_access_denied(E_ACCESSDENIED_CODE));
            assert!(!is_access_denied(AUDCLNT_E_DEVICE_INVALIDATED_CODE));
            assert!(!is_access_denied(0));
        }

        #[test]
        fn render_reselect_error_only_matches_device_invalidated() {
            assert!(is_render_reselect_error(AUDCLNT_E_DEVICE_INVALIDATED_CODE));
            assert!(!is_render_reselect_error(0));
            assert!(!is_render_reselect_error(0x8000_4005u32 as i32));
        }

        #[cfg(windows)]
        mod windows_drift {
            #[test]
            fn device_invalidated_code_matches_windows_constant() {
                assert_eq!(
                    super::super::AUDCLNT_E_DEVICE_INVALIDATED_CODE,
                    windows::Win32::Media::Audio::AUDCLNT_E_DEVICE_INVALIDATED.0
                );
            }

            #[test]
            fn access_denied_code_matches_windows_constant() {
                assert_eq!(
                    super::super::E_ACCESSDENIED_CODE,
                    windows::Win32::Foundation::E_ACCESSDENIED.0
                );
            }
        }
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod imp {
    use std::ptr;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, RwLock};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use crate::reselect::{
        consent_refuses, is_access_denied, is_render_reselect_error, packet_start_us,
        render_reopen_needed, MicTicks, CONSENT_EVERY_TICKS, MIC_CONSENT_DESKTOP_KEY,
        MIC_CONSENT_KEY,
    };
    use observer_mic::{apply_gain_f32, apply_gain_i16, MicConfig, MicDeviceRef};
    use observer_model::{
        monotonic_us, AudioFormat, CaptureChunk, CaptureSink, ErrorReason, MicSource, SourceError,
        SourceKind, SourceState, SystemAudioSource,
    };
    use windows::core::{implement, Error as WindowsError, Result as WindowsResult, PCWSTR};
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Foundation::{ERROR_SUCCESS, RPC_E_CHANGED_MODE};
    use windows::Win32::Media::Audio::{
        eCapture, eConsole, eRender, EDataFlow, ERole, IAudioCaptureClient, IAudioClient,
        IMMDevice, IMMDeviceEnumerator, IMMNotificationClient, IMMNotificationClient_Impl,
        MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_LOOPBACK, DEVICE_STATE, DEVICE_STATE_ACTIVE, WAVEFORMATEX,
        WAVEFORMATEXTENSIBLE, WAVE_FORMAT_PCM,
    };
    use windows::Win32::Media::KernelStreaming::{
        KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE,
    };
    use windows::Win32::Media::Multimedia::{
        KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT,
    };
    use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
        COINIT_MULTITHREADED, STGM_READ,
    };
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
    use windows::Win32::System::Registry::{
        RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ,
    };

    /// The running mic loop's tick. On each one it re-applies the owner's
    /// selection when Windows reported a device change or the owner's choice
    /// changed, so an unplug or a Settings change takes effect without an app
    /// restart; it lists devices only then.
    const MIC_RESELECT_INTERVAL: Duration = Duration::from_millis(1000);

    const PULL_INTERVAL: Duration = Duration::from_millis(100);

    /// The monotonic clock and the performance counter read together, the
    /// counter in the 100 ns units Windows stamps packets with.
    fn clocks_now() -> (u64, u64) {
        let now_us = monotonic_us();
        let mut counter = 0i64;
        let mut frequency = 0i64;
        let read = unsafe {
            QueryPerformanceCounter(&mut counter).is_ok()
                && QueryPerformanceFrequency(&mut frequency).is_ok()
        };
        if !read || counter <= 0 || frequency <= 0 {
            return (now_us, 0);
        }
        let now_100ns = (counter as u128 * 10_000_000 / frequency as u128) as u64;
        (now_us, now_100ns)
    }

    /// When the packet just taken from `GetBuffer` began.
    fn packet_at_us(packet_100ns: u64) -> u64 {
        let (now_us, now_100ns) = clocks_now();
        packet_start_us(now_us, now_100ns, packet_100ns)
    }

    /// Raised by Windows whenever an audio endpoint is added, removed, changes
    /// state or becomes a default, so the microphone lists its devices again
    /// only then.
    #[implement(IMMNotificationClient)]
    struct DeviceChanges {
        changed: Arc<AtomicBool>,
    }

    impl IMMNotificationClient_Impl for DeviceChanges_Impl {
        fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> WindowsResult<()> {
            self.changed.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn OnDeviceAdded(&self, _: &PCWSTR) -> WindowsResult<()> {
            self.changed.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn OnDeviceRemoved(&self, _: &PCWSTR) -> WindowsResult<()> {
            self.changed.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn OnDefaultDeviceChanged(&self, _: EDataFlow, _: ERole, _: &PCWSTR) -> WindowsResult<()> {
            self.changed.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn OnPropertyValueChanged(
            &self,
            _: &PCWSTR,
            _: &windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY,
        ) -> WindowsResult<()> {
            Ok(())
        }
    }

    /// The device-change watch for one capture thread. When Windows will not
    /// take the callback, every tick counts as a change, as before.
    struct DeviceWatch {
        enumerator: IMMDeviceEnumerator,
        client: Option<IMMNotificationClient>,
        changed: Arc<AtomicBool>,
    }

    impl DeviceWatch {
        fn new(enumerator: &IMMDeviceEnumerator) -> Self {
            let changed = Arc::new(AtomicBool::new(false));
            let client: IMMNotificationClient = DeviceChanges {
                changed: Arc::clone(&changed),
            }
            .into();
            let registered =
                unsafe { enumerator.RegisterEndpointNotificationCallback(&client) }.is_ok();
            Self {
                enumerator: enumerator.clone(),
                client: registered.then_some(client),
                changed,
            }
        }

        /// Whether a device changed since the last call.
        fn take(&self) -> bool {
            self.client.is_none() || self.changed.swap(false, Ordering::SeqCst)
        }
    }

    impl Drop for DeviceWatch {
        fn drop(&mut self) {
            if let Some(client) = self.client.take() {
                unsafe {
                    let _ = self
                        .enumerator
                        .UnregisterEndpointNotificationCallback(&client);
                }
            }
        }
    }
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

    fn default_render_device(enumerator: &IMMDeviceEnumerator) -> WindowsResult<IMMDevice> {
        unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
    }

    fn device_id(device: &IMMDevice) -> Option<String> {
        let id_pw = unsafe { device.GetId().ok()? };
        // SAFETY: GetId returned a valid null-terminated PWSTR we free below.
        let id = unsafe { id_pw.to_string() }.unwrap_or_default();
        unsafe { CoTaskMemFree(Some(id_pw.0 as *const _)) };
        if id.is_empty() {
            None
        } else {
            Some(id)
        }
    }

    fn bytes_per_frame(format: *const WAVEFORMATEX) -> usize {
        let channels = unsafe { ptr::addr_of!((*format).nChannels).read_unaligned() } as usize;
        let bits = unsafe { ptr::addr_of!((*format).wBitsPerSample).read_unaligned() } as usize;
        channels * bits / 8
    }

    fn audio_format_from(format: *const WAVEFORMATEX) -> AudioFormat {
        let sample_rate_hz = unsafe { ptr::addr_of!((*format).nSamplesPerSec).read_unaligned() };
        let channels = unsafe { ptr::addr_of!((*format).nChannels).read_unaligned() };
        let bits_per_sample = unsafe { ptr::addr_of!((*format).wBitsPerSample).read_unaligned() };
        let tag = unsafe { ptr::addr_of!((*format).wFormatTag).read_unaligned() } as u32;
        let cb_size = unsafe { ptr::addr_of!((*format).cbSize).read_unaligned() };
        let is_float = match tag {
            WAVE_FORMAT_PCM => false,
            WAVE_FORMAT_IEEE_FLOAT => true,
            WAVE_FORMAT_EXTENSIBLE if cb_size >= 22 => {
                let ext = format as *const WAVEFORMATEXTENSIBLE;
                let sub_format = unsafe { ptr::addr_of!((*ext).SubFormat).read_unaligned() };
                if sub_format == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
                    true
                } else if sub_format == KSDATAFORMAT_SUBTYPE_PCM {
                    false
                } else {
                    bits_per_sample == 32
                }
            }
            _ => bits_per_sample == 32,
        };

        AudioFormat {
            sample_rate_hz,
            channels,
            bits_per_sample,
            is_float,
        }
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
    /// present-and-enabled device, applies input gain to each buffer, and
    /// re-selects when a device or the owner's choice changes, so an unplug or a
    /// Settings change takes effect without an app restart. Honest state throughout:
    /// `NoInputDevice` when no hardware exists, `Inactive` when every device is
    /// disabled by the owner, `Active` only while truly streaming.
    fn consent_value(root: HKEY, subkey: &str) -> Option<String> {
        let key: Vec<u16> = subkey.encode_utf16().chain(Some(0)).collect();
        let name: Vec<u16> = "Value".encode_utf16().chain(Some(0)).collect();
        let mut buf = [0u16; 32];
        let mut bytes = std::mem::size_of_val(&buf) as u32;
        let status = unsafe {
            RegGetValueW(
                root,
                PCWSTR(key.as_ptr()),
                PCWSTR(name.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut bytes),
            )
        };
        if status != ERROR_SUCCESS {
            return None;
        }
        let chars = (bytes as usize / 2).min(buf.len());
        let value = String::from_utf16_lossy(&buf[..chars]);
        Some(value.trim_end_matches('\0').to_string())
    }

    /// Whether Windows privacy settings currently refuse this app the microphone.
    fn microphone_refused() -> bool {
        consent_refuses(&[
            consent_value(HKEY_LOCAL_MACHINE, MIC_CONSENT_KEY),
            consent_value(HKEY_CURRENT_USER, MIC_CONSENT_KEY),
            consent_value(HKEY_CURRENT_USER, MIC_CONSENT_DESKTOP_KEY),
        ])
    }

    fn report_refused(state: &Arc<Mutex<SourceState>>, active: &Arc<Mutex<Option<String>>>) {
        set_state(
            state,
            SourceState::Faulted {
                reason: ErrorReason::AccessDenied,
                detail: String::new(),
            },
        );
        set_active(active, None);
    }

    /// Activate, initialize and start a shared-mode capture stream on `device`.
    /// `Ok(None)` when the endpoint reports an unusable mix format.
    fn open_mic_stream(
        device: &IMMDevice,
    ) -> WindowsResult<Option<(IAudioClient, IAudioCaptureClient, AudioFormat, usize)>> {
        let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None)? };
        let format = unsafe { client.GetMixFormat()? };
        let audio_format = audio_format_from(format);
        let bpf = bytes_per_frame(format);
        if bpf == 0 {
            unsafe { CoTaskMemFree(Some(format.cast())) };
            return Ok(None);
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
        Ok(Some((client, capture, audio_format, bpf)))
    }

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
        let watch = DeviceWatch::new(&enumerator);
        let read_config = || config.read().map(|c| c.clone()).unwrap_or_default();
        // Until a device or the owner's choice changes, listing again finds
        // the same thing.
        let wait_for_change = |cfg: &MicConfig| {
            while !stop.load(Ordering::Relaxed) {
                thread::sleep(MIC_RESELECT_INTERVAL);
                if watch.take() || read_config() != *cfg {
                    return;
                }
            }
        };

        while !stop.load(Ordering::Relaxed) {
            sink.begin_stream();
            let devices = enumerate_mic_devices(&enumerator).unwrap_or_default();
            let mut cfg = read_config();
            if devices.is_empty() {
                set_state(&state, SourceState::NoInputDevice);
                set_active(&active, None);
                wait_for_change(&cfg);
                continue;
            }
            let Some(selected) = cfg.select(&devices).cloned() else {
                // Devices exist but the owner disabled them all -> not producing.
                set_state(&state, SourceState::Inactive);
                set_active(&active, None);
                wait_for_change(&cfg);
                continue;
            };

            if microphone_refused() {
                report_refused(&state, &active);
                thread::sleep(MIC_RESELECT_INTERVAL * CONSENT_EVERY_TICKS);
                continue;
            }

            let wide_id: Vec<u16> = selected.id.encode_utf16().chain(Some(0)).collect();
            let device = match unsafe { enumerator.GetDevice(PCWSTR(wide_id.as_ptr())) } {
                Ok(d) => d,
                Err(_) => {
                    thread::sleep(MIC_RESELECT_INTERVAL);
                    continue;
                }
            };
            let (client, capture, audio_format, bpf) = match open_mic_stream(&device) {
                Ok(Some(opened)) => opened,
                Ok(None) => {
                    thread::sleep(MIC_RESELECT_INTERVAL);
                    continue;
                }
                Err(err) if is_access_denied(err.code().0) => {
                    report_refused(&state, &active);
                    thread::sleep(MIC_RESELECT_INTERVAL);
                    continue;
                }
                Err(err) => return Err(err),
            };
            let bits = audio_format.bits_per_sample;
            set_state(&state, SourceState::Active);
            set_active(&active, Some(selected.id.clone()));

            let mut since_reselect = Duration::ZERO;
            let mut ticks = MicTicks::default();
            'inner: while !stop.load(Ordering::Relaxed) {
                thread::sleep(PULL_INTERVAL);
                since_reselect += PULL_INTERVAL;
                let mult = config.read().map(|c| c.gain_multiplier()).unwrap_or(1.0);
                loop {
                    let packet = match unsafe { capture.GetNextPacketSize() } {
                        Ok(packet) => packet,
                        Err(err) if is_access_denied(err.code().0) => {
                            report_refused(&state, &active);
                            break 'inner;
                        }
                        Err(err) => return Err(err),
                    };
                    if packet == 0 {
                        break;
                    }
                    let mut data_ptr: *mut u8 = ptr::null_mut();
                    let mut frames: u32 = 0;
                    let mut flags: u32 = 0;
                    let mut packet_100ns: u64 = 0;
                    match unsafe {
                        capture.GetBuffer(
                            &mut data_ptr,
                            &mut frames,
                            &mut flags,
                            None,
                            Some(&mut packet_100ns),
                        )
                    } {
                        Ok(()) => {}
                        Err(err) if is_access_denied(err.code().0) => {
                            report_refused(&state, &active);
                            break 'inner;
                        }
                        Err(err) => return Err(err),
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
                            format: Some(audio_format),
                            at_us: Some(packet_at_us(packet_100ns)),
                        });
                    }
                    unsafe { capture.ReleaseBuffer(frames)? };
                }
                if since_reselect >= MIC_RESELECT_INTERVAL {
                    since_reselect = Duration::ZERO;
                    let now_cfg = read_config();
                    let checks = ticks.next(watch.take(), now_cfg != cfg);
                    if checks.reselect {
                        let devices = enumerate_mic_devices(&enumerator).unwrap_or_default();
                        let now = now_cfg.select(&devices).map(|d| d.id.clone());
                        if now.as_deref() != Some(selected.id.as_str()) {
                            break 'inner; // selection changed -> reopen
                        }
                        cfg = now_cfg;
                    }
                    if checks.consent && microphone_refused() {
                        break 'inner; // access refused -> reopen reports it
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

    /// Render-loopback capture loop. It follows the OS default render endpoint,
    /// re-checking on the same 1s cadence as mic reselect and reopening on a
    /// default-device change or `AUDCLNT_E_DEVICE_INVALIDATED`, so a switch costs
    /// <=~1s of system audio. State is honest: `Active` only while streaming,
    /// `Inactive` during a transient reopen, `NoInputDevice` when no render
    /// endpoint exists, and device churn is never `Faulted` because system audio is
    /// a required source and a fault would flip the app to Error. On reopen we
    /// re-read the mix format and keep emitting chunks with the new format; the
    /// per-segment sidecar remains the segment's open-time format, so a mid-segment
    /// render-format switch may mislabel the <=5-minute PCM tail until rotation.
    /// That is deliberate v1: no format-conversion machinery here.
    fn run_loopback_loop(
        sink: Arc<dyn CaptureSink>,
        state: Arc<Mutex<SourceState>>,
        stop: Arc<AtomicBool>,
        seq: Arc<AtomicU64>,
    ) -> WindowsResult<()> {
        let _com = ComApartment::new()?;
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };

        while !stop.load(Ordering::Relaxed) {
            sink.begin_stream();
            let device = match default_render_device(&enumerator) {
                Ok(device) => device,
                Err(_) => {
                    set_state(&state, SourceState::NoInputDevice);
                    thread::sleep(MIC_RESELECT_INTERVAL);
                    continue;
                }
            };
            let opened_id = device_id(&device);

            let client: IAudioClient = match unsafe { device.Activate(CLSCTX_ALL, None) } {
                Ok(client) => client,
                Err(err) if is_render_reselect_error(err.code().0) => {
                    set_state(&state, SourceState::Inactive);
                    thread::sleep(MIC_RESELECT_INTERVAL);
                    continue;
                }
                Err(err) => return Err(err),
            };
            let format = match unsafe { client.GetMixFormat() } {
                Ok(format) => format,
                Err(err) if is_render_reselect_error(err.code().0) => {
                    set_state(&state, SourceState::Inactive);
                    thread::sleep(MIC_RESELECT_INTERVAL);
                    continue;
                }
                Err(err) => return Err(err),
            };
            let audio_format = audio_format_from(format);
            let bpf = bytes_per_frame(format);
            if bpf == 0 {
                unsafe { CoTaskMemFree(Some(format.cast())) };
                set_state(&state, SourceState::Inactive);
                thread::sleep(MIC_RESELECT_INTERVAL);
                continue;
            }

            let initialize = unsafe {
                client.Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_LOOPBACK,
                    WASAPI_BUFFER_DURATION_100NS,
                    0,
                    format,
                    None,
                )
            };
            unsafe { CoTaskMemFree(Some(format.cast())) };
            match initialize {
                Ok(()) => {}
                Err(err) if is_render_reselect_error(err.code().0) => {
                    set_state(&state, SourceState::Inactive);
                    thread::sleep(MIC_RESELECT_INTERVAL);
                    continue;
                }
                Err(err) => return Err(err),
            }

            let capture: IAudioCaptureClient = match unsafe { client.GetService() } {
                Ok(capture) => capture,
                Err(err) if is_render_reselect_error(err.code().0) => {
                    set_state(&state, SourceState::Inactive);
                    thread::sleep(MIC_RESELECT_INTERVAL);
                    continue;
                }
                Err(err) => return Err(err),
            };
            match unsafe { client.Start() } {
                Ok(()) => {}
                Err(err) if is_render_reselect_error(err.code().0) => {
                    set_state(&state, SourceState::Inactive);
                    thread::sleep(MIC_RESELECT_INTERVAL);
                    continue;
                }
                Err(err) => return Err(err),
            }
            set_state(&state, SourceState::Active);

            let mut since_reselect = Duration::ZERO;
            'inner: while !stop.load(Ordering::Relaxed) {
                thread::sleep(PULL_INTERVAL);
                since_reselect += PULL_INTERVAL;
                loop {
                    let packet = match unsafe { capture.GetNextPacketSize() } {
                        Ok(packet) => packet,
                        Err(err) if is_render_reselect_error(err.code().0) => {
                            set_state(&state, SourceState::Inactive);
                            break 'inner;
                        }
                        Err(err) => return Err(err),
                    };
                    if packet == 0 {
                        break;
                    }

                    let mut data_ptr: *mut u8 = ptr::null_mut();
                    let mut frames: u32 = 0;
                    let mut flags: u32 = 0;
                    let mut packet_100ns: u64 = 0;
                    match unsafe {
                        capture.GetBuffer(
                            &mut data_ptr,
                            &mut frames,
                            &mut flags,
                            None,
                            Some(&mut packet_100ns),
                        )
                    } {
                        Ok(()) => {}
                        Err(err) if is_render_reselect_error(err.code().0) => {
                            set_state(&state, SourceState::Inactive);
                            break 'inner;
                        }
                        Err(err) => return Err(err),
                    }

                    if frames > 0 {
                        let byte_len = frames as usize * bpf;
                        let data = if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0
                            || data_ptr.is_null()
                        {
                            vec![0; byte_len]
                        } else {
                            unsafe { std::slice::from_raw_parts(data_ptr, byte_len) }.to_vec()
                        };
                        let seq = seq.fetch_add(1, Ordering::Relaxed);
                        sink.emit(CaptureChunk {
                            source: SourceKind::SystemAudio,
                            seq,
                            data,
                            format: Some(audio_format),
                            at_us: Some(packet_at_us(packet_100ns)),
                        });
                    }
                    match unsafe { capture.ReleaseBuffer(frames) } {
                        Ok(()) => {}
                        Err(err) if is_render_reselect_error(err.code().0) => {
                            set_state(&state, SourceState::Inactive);
                            break 'inner;
                        }
                        Err(err) => return Err(err),
                    }
                }
                if since_reselect >= MIC_RESELECT_INTERVAL {
                    since_reselect = Duration::ZERO;
                    let current = default_render_device(&enumerator)
                        .ok()
                        .and_then(|device| device_id(&device));
                    if render_reopen_needed(opened_id.as_deref(), current.as_deref()) {
                        set_state(&state, SourceState::Inactive);
                        break 'inner;
                    }
                }
            }
            unsafe {
                let _ = client.Stop();
            }
        }

        set_state(&state, SourceState::Inactive);
        Ok(())
    }

    fn spawn_loopback_worker(
        sink: Arc<dyn CaptureSink>,
        state: Arc<Mutex<SourceState>>,
        seq: Arc<AtomicU64>,
    ) -> Worker {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker_state = Arc::clone(&state);
        let join = thread::spawn(move || {
            if let Err(err) = run_loopback_loop(sink, Arc::clone(&worker_state), worker_stop, seq) {
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
            self.worker = Some(spawn_loopback_worker(
                sink,
                Arc::clone(&self.state),
                Arc::clone(&self.seq),
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
                state: Arc::new(Mutex::new(SourceState::Inactive)),
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

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn microphone_is_inactive_until_started() {
            let active = Arc::new(Mutex::new(None));
            let source = WasapiMicSource::new(
                Arc::new(RwLock::new(MicConfig::default())),
                Arc::clone(&active),
            );

            assert_eq!(source.state(), SourceState::Inactive);
            assert!(source.worker.is_none());
            assert!(active.lock().unwrap().is_none());
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
