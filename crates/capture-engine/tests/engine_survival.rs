// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use capture_engine::{CaptureEngine, EngineCommand, EngineConfig, Sources};
use observer_model::desktop_activity::{
    DesktopActivityProbeEvent, DesktopActivitySource, DesktopActivityUrlRequest,
    DesktopActivityUrlResult, ForegroundSnapshot,
};
use observer_model::{
    AudioFormat, CaptureChunk, CaptureSink, Clock, EncoderError, EncoderHealth, MicSource,
    ScreenEncoder, ScreenFrame, ScreenSource, SourceError, SourceKind, SourceState, StorageHealth,
    SystemAudioSource, AUDIO_FILE_NAME,
};
use observer_pl::civil::segment_key_string_local;
use observer_segment::DEFAULT_SEGMENT_SECS;
use pl_transport_win::sealed::{LocalSealedStore, SealedStore};

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

    fn now_epoch_us(&self) -> u64 {
        self.now.load(Ordering::Relaxed) * 1_000_000
    }

    fn now_monotonic_us(&self) -> u64 {
        self.now.load(Ordering::Relaxed) * 1_000_000
    }
}

#[derive(Clone, Default)]
struct ScreenHandle {
    display_changes: Arc<AtomicU64>,
}

impl ScreenHandle {
    fn display_changes(&self) -> u64 {
        self.display_changes.load(Ordering::Relaxed)
    }
}

struct FakeScreen {
    handle: ScreenHandle,
}

impl ScreenSource for FakeScreen {
    fn start(&mut self, _sink: Arc<dyn CaptureSink>) -> Result<(), SourceError> {
        Ok(())
    }

    fn stop(&mut self) {}

    fn state(&self) -> SourceState {
        SourceState::Active
    }

    fn on_display_changed(&mut self) {
        self.handle.display_changes.fetch_add(1, Ordering::Relaxed);
    }
}

struct FakeSystemAudio;

impl SystemAudioSource for FakeSystemAudio {
    fn start(&mut self, _sink: Arc<dyn CaptureSink>) -> Result<(), SourceError> {
        Ok(())
    }

    fn stop(&mut self) {}

    fn state(&self) -> SourceState {
        SourceState::Active
    }
}

struct FakeMic;

impl MicSource for FakeMic {
    fn start(&mut self, _sink: Arc<dyn CaptureSink>) -> Result<(), SourceError> {
        Ok(())
    }

    fn stop(&mut self) {}

    fn state(&self) -> SourceState {
        SourceState::NoInputDevice
    }
}

struct ActiveMic;

impl MicSource for ActiveMic {
    fn start(&mut self, _sink: Arc<dyn CaptureSink>) -> Result<(), SourceError> {
        Ok(())
    }

    fn stop(&mut self) {}

    fn state(&self) -> SourceState {
        SourceState::Active
    }
}

struct FakeDesktopActivityHandle {
    state: SourceState,
    events: Vec<DesktopActivityProbeEvent>,
    url_results: Vec<DesktopActivityUrlResult>,
    snapshot: Option<ForegroundSnapshot>,
}

impl Default for FakeDesktopActivityHandle {
    fn default() -> Self {
        Self {
            state: SourceState::Active,
            events: Vec::new(),
            url_results: Vec::new(),
            snapshot: None,
        }
    }
}

#[derive(Clone, Default)]
struct FakeDesktopActivity {
    inner: Arc<Mutex<FakeDesktopActivityHandle>>,
}

impl DesktopActivitySource for FakeDesktopActivity {
    fn start(&mut self) -> Result<(), SourceError> {
        self.inner.lock().unwrap().state = SourceState::Active;
        Ok(())
    }

    fn stop(&mut self) {
        self.inner.lock().unwrap().state = SourceState::Inactive;
    }

    fn state(&self) -> SourceState {
        self.inner.lock().unwrap().state.clone()
    }

    fn poll_events(&mut self) -> Vec<DesktopActivityProbeEvent> {
        std::mem::take(&mut self.inner.lock().unwrap().events)
    }

    fn request_url(&mut self, _request: DesktopActivityUrlRequest) {}

    fn poll_url_result(&mut self) -> Option<DesktopActivityUrlResult> {
        let mut lock = self.inner.lock().unwrap();
        if lock.url_results.is_empty() {
            None
        } else {
            Some(lock.url_results.remove(0))
        }
    }

    fn cancel_url(&mut self) {}

    fn foreground_snapshot(&mut self) -> Option<ForegroundSnapshot> {
        self.inner.lock().unwrap().snapshot.clone()
    }
}

#[derive(Default)]
struct FakeEncoder;

impl ScreenEncoder for FakeEncoder {
    fn open(&mut self, _dir: &str, _width: u32, _height: u32) -> Result<(), EncoderError> {
        Ok(())
    }

    fn encode_frame(&mut self, _frame: &ScreenFrame) -> Result<(), EncoderError> {
        Ok(())
    }

    fn finalize(&mut self) -> Result<(), EncoderError> {
        Ok(())
    }

    fn frames_consumed(&self) -> u64 {
        0
    }

    fn samples_written(&self) -> u64 {
        0
    }

    fn video_end_secs(&self) -> Option<f64> {
        None
    }

    fn last_error(&self) -> Option<String> {
        None
    }

    fn health(&self) -> EncoderHealth {
        EncoderHealth {
            frames_consumed: 0,
            frames_dropped: 0,
            samples_written: 0,
            clamp_events: 0,
            last_error: None,
        }
    }
}

fn temp_root() -> PathBuf {
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "solstone-engine-survival-{}-{id}",
        std::process::id()
    ))
}

fn sources() -> (Sources, ScreenHandle) {
    let screen = ScreenHandle::default();
    (
        Sources {
            screen: Box::new(FakeScreen {
                handle: screen.clone(),
            }),
            screen_encoder: Box::new(FakeEncoder),
            system_audio: Box::new(FakeSystemAudio),
            mic: Box::new(FakeMic),
            desktop_activity: Box::new(FakeDesktopActivity::default()),
        },
        screen,
    )
}

fn active_mic_sources() -> Sources {
    Sources {
        screen: Box::new(FakeScreen {
            handle: ScreenHandle::default(),
        }),
        screen_encoder: Box::new(FakeEncoder),
        system_audio: Box::new(FakeSystemAudio),
        mic: Box::new(ActiveMic),
        desktop_activity: Box::new(FakeDesktopActivity::default()),
    }
}

fn audio_format() -> AudioFormat {
    AudioFormat {
        sample_rate_hz: 16_000,
        channels: 1,
        bits_per_sample: 16,
        is_float: false,
    }
}

fn pcm_i16(samples: usize) -> Vec<u8> {
    pcm_i16_amplitude(samples, 1000)
}

fn pcm_i16_amplitude(samples: usize, amp: i16) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples * 2);
    for _ in 0..samples {
        bytes.extend_from_slice(&amp.to_le_bytes());
    }
    bytes
}

fn emit_system_audio(sink: &Arc<dyn CaptureSink>, seq: u64, samples: usize) {
    sink.emit(CaptureChunk {
        source: SourceKind::SystemAudio,
        seq,
        data: pcm_i16(samples),
        format: Some(audio_format()),
        at_us: None,
    });
}

fn emit_mic(sink: &Arc<dyn CaptureSink>, seq: u64, samples: usize, amp: i16, at_us: u64) {
    sink.emit(CaptureChunk {
        source: SourceKind::Mic,
        seq,
        data: pcm_i16_amplitude(samples, amp),
        format: Some(audio_format()),
        at_us: Some(at_us),
    });
}

fn decode_flac_stereo_mic(flac_bytes: &[u8]) -> Vec<i32> {
    let mut reader = claxon::FlacReader::new(std::io::Cursor::new(flac_bytes)).unwrap();
    let all_samples: Vec<i32> = reader.samples().map(|s| s.unwrap()).collect();
    all_samples
        .chunks_exact(2)
        .map(|frame| {
            assert_eq!(frame[1], 0, "right channel must be silence");
            frame[0]
        })
        .collect()
}

#[test]
fn pause_resume_same_window_seals_once_and_preserves_audio_duration() {
    let root = temp_root();
    let _ = std::fs::remove_dir_all(&root);
    let clock = FakeClock::new(100);
    let (sources, screen) = sources();
    let mut recovery = platform_win::LocalRecoveryFs::new(root.clone(), DEFAULT_SEGMENT_SECS);
    let segment_fs = platform_win::LocalSegmentFs::new(root.clone());
    let (mut engine, outcomes) = CaptureEngine::new(
        sources,
        EngineConfig::default(),
        &mut recovery,
        segment_fs,
        Box::new(clock.clone()),
    )
    .unwrap();
    assert!(outcomes.is_empty());

    engine.set_source_choices(observer_model::setup::SetupSnapshot {
        mic: true,
        system_audio: true,
        screen: true,
        ..observer_model::setup::SetupSnapshot::default()
    });
    engine.start();
    let sink = engine.sink();
    let pre = 16_000usize;
    let mid = 8_000usize;
    let post = 4_000usize;

    emit_system_audio(&sink, 1, pre);
    engine.pump();
    engine.apply_command(EngineCommand::Pause {
        reason: observer_model::PauseReason::Operator,
        duration_secs: None,
    });
    assert!(engine.health_dump().segment_dir.is_some());
    assert!(LocalSealedStore::new(&root, DEFAULT_SEGMENT_SECS)
        .scan()
        .unwrap()
        .is_empty());

    engine.apply_command(EngineCommand::Resume);
    emit_system_audio(&sink, 2, mid);
    engine.pump();
    engine.apply_command(EngineCommand::Pause {
        reason: observer_model::PauseReason::Operator,
        duration_secs: None,
    });
    engine.apply_command(EngineCommand::Resume);
    emit_system_audio(&sink, 3, post);
    engine.pump();

    clock.set(300);
    engine.pump();

    let before_display = screen.display_changes();
    engine.apply_command(EngineCommand::DisplayChanged);
    assert_eq!(screen.display_changes(), before_display + 1);

    let store = LocalSealedStore::new(&root, DEFAULT_SEGMENT_SECS);
    let sealed = store.scan().unwrap();
    assert_eq!(sealed.len(), 1);
    assert_eq!(sealed[0].index, 0);
    let flac = store.read_file(sealed[0].index, AUDIO_FILE_NAME).unwrap();
    let duration = observer_audio::flac_duration_secs(&flac).unwrap();
    let expected = (pre + mid + post) as f64 / 16_000.0;
    assert!(
        (duration - expected).abs() < 0.001,
        "duration {duration} != expected {expected}"
    );

    emit_system_audio(&sink, 4, 16_000);
    engine.pump();
    clock.set(600);
    engine.pump();

    let sealed = store.scan().unwrap();
    assert_eq!(sealed.len(), 2);
    let keys: BTreeSet<_> = sealed
        .iter()
        .map(|segment| {
            segment_key_string_local(
                segment.boundary_epoch_secs,
                0,
                segment.len_secs.unwrap_or(DEFAULT_SEGMENT_SECS),
            )
        })
        .collect();
    assert_eq!(keys.len(), sealed.len());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn same_window_mic_gap_preservation() {
    let root = temp_root();
    let _ = std::fs::remove_dir_all(&root);
    let clock = FakeClock::new(100);
    let sources = active_mic_sources();
    let mut recovery = platform_win::LocalRecoveryFs::new(root.clone(), DEFAULT_SEGMENT_SECS);
    let segment_fs = platform_win::LocalSegmentFs::new(root.clone());
    let (mut engine, outcomes) = CaptureEngine::new(
        sources,
        EngineConfig::default(),
        &mut recovery,
        segment_fs,
        Box::new(clock.clone()),
    )
    .unwrap();
    assert!(outcomes.is_empty());

    engine.set_source_choices(observer_model::setup::SetupSnapshot {
        mic: true,
        system_audio: false,
        screen: false,
        ..observer_model::setup::SetupSnapshot::default()
    });
    engine.start();
    let sink = engine.sink();

    // Take A: amplitude 1200 at monotonic us = 100_000_000 + 10_000
    emit_mic(&sink, 1, 1600, 1200, 100_000_000 + 10_000);
    engine.pump();

    // OFF
    engine.apply_command(EngineCommand::SetSourceEnabled {
        kind: SourceKind::Mic,
        enabled: false,
    });
    engine.pump();

    // 1 second later
    clock.set(101);

    // ON
    engine.apply_command(EngineCommand::SetSourceEnabled {
        kind: SourceKind::Mic,
        enabled: true,
    });

    // Take B: amplitude 2400 at monotonic us = 101_000_000 + 10_000
    emit_mic(&sink, 2, 1600, 2400, 101_000_000 + 10_000);
    engine.pump();

    // OFF
    engine.apply_command(EngineCommand::SetSourceEnabled {
        kind: SourceKind::Mic,
        enabled: false,
    });
    engine.pump();

    // Advance past boundary and pump
    clock.set(300);
    engine.pump();

    let store = LocalSealedStore::new(&root, DEFAULT_SEGMENT_SECS);
    let sealed = store.scan().unwrap();
    assert_eq!(sealed.len(), 1);
    assert_eq!(sealed[0].boundary_epoch_secs, 0);

    let flac = store.read_file(sealed[0].index, AUDIO_FILE_NAME).unwrap();
    let mic_samples = decode_flac_stereo_mic(&flac);

    assert!(mic_samples.contains(&1200));
    assert!(mic_samples.contains(&2400));

    let first_b = mic_samples.iter().position(|&s| s == 2400).unwrap();
    let last_a = mic_samples[..first_b]
        .iter()
        .rposition(|&s| s == 1200)
        .unwrap();
    assert!(first_b > last_a + 1);
    assert!(mic_samples[last_a + 1..first_b].iter().all(|&s| s == 0));

    assert!(!root.join("quarantine").exists());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cross_boundary_mic_seals_earlier_key_at_on_and_places_new_samples_in_current_key() {
    let root = temp_root();
    let _ = std::fs::remove_dir_all(&root);
    let clock = FakeClock::new(100);
    let sources = active_mic_sources();
    let mut recovery = platform_win::LocalRecoveryFs::new(root.clone(), DEFAULT_SEGMENT_SECS);
    let segment_fs = platform_win::LocalSegmentFs::new(root.clone());
    let (mut engine, outcomes) = CaptureEngine::new(
        sources,
        EngineConfig::default(),
        &mut recovery,
        segment_fs,
        Box::new(clock.clone()),
    )
    .unwrap();
    assert!(outcomes.is_empty());

    engine.set_source_choices(observer_model::setup::SetupSnapshot {
        mic: true,
        system_audio: false,
        screen: false,
        ..observer_model::setup::SetupSnapshot::default()
    });
    engine.start();
    let sink = engine.sink();

    // Take 1: amplitude 3500 at monotonic us = 100_000_000 + 20_000
    emit_mic(&sink, 1, 1600, 3500, 100_000_000 + 20_000);
    engine.pump();

    // OFF
    engine.apply_command(EngineCommand::SetSourceEnabled {
        kind: SourceKind::Mic,
        enabled: false,
    });

    // Advance clock past boundary with no pump between advance and ON
    clock.set(350);

    // ON
    engine.apply_command(EngineCommand::SetSourceEnabled {
        kind: SourceKind::Mic,
        enabled: true,
    });

    // The earlier key is sealed once at ON and contains the first take
    let store = LocalSealedStore::new(&root, DEFAULT_SEGMENT_SECS);
    let sealed_at_on = store.scan().unwrap();
    assert_eq!(sealed_at_on.len(), 1);
    assert_eq!(sealed_at_on[0].boundary_epoch_secs, 0);

    let flac_0 = store
        .read_file(sealed_at_on[0].index, AUDIO_FILE_NAME)
        .unwrap();
    let mic_samples_0 = decode_flac_stereo_mic(&flac_0);
    assert!(mic_samples_0.contains(&3500));

    // New samples at 351s
    clock.set(351);
    emit_mic(&sink, 2, 1600, 4500, 351_000_000 + 20_000);
    engine.pump();

    // Seal current segment
    clock.set(600);
    engine.pump();

    let sealed = store.scan().unwrap();
    assert_eq!(sealed.len(), 2);
    assert_eq!(sealed[1].boundary_epoch_secs, 300);

    let flac_1 = store.read_file(sealed[1].index, AUDIO_FILE_NAME).unwrap();
    let mic_samples_1 = decode_flac_stereo_mic(&flac_1);
    assert!(mic_samples_1.contains(&4500));

    // Placement is relative to the 300s boundary, including the initial OFF interval.
    let first_non_zero = mic_samples_1.iter().position(|&s| s != 0).unwrap();
    assert_eq!(first_non_zero, 816_320);
    assert!(!root.join("quarantine").exists());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn fresh_all_off_start_creates_no_segment() {
    let root = temp_root();
    let _ = std::fs::remove_dir_all(&root);
    let clock = FakeClock::new(100);
    let mut recovery = platform_win::LocalRecoveryFs::new(root.clone(), DEFAULT_SEGMENT_SECS);
    let segment_fs = platform_win::LocalSegmentFs::new(root.clone());
    let (mut engine, outcomes) = CaptureEngine::new(
        active_mic_sources(),
        EngineConfig::default(),
        &mut recovery,
        segment_fs,
        Box::new(clock),
    )
    .unwrap();
    assert!(outcomes.is_empty());

    engine.set_source_choices(observer_model::setup::SetupSnapshot {
        mic: false,
        system_audio: false,
        screen: false,
        ..observer_model::setup::SetupSnapshot::default()
    });
    engine.start();

    assert!(engine.health_dump().segment_dir.is_none());
    let store = LocalSealedStore::new(&root, DEFAULT_SEGMENT_SECS);
    assert!(store.scan().unwrap().is_empty());
    assert!(!root.exists() || root.read_dir().unwrap().next().is_none());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn all_off_restart_seals_existing_current_window_audio() {
    use observer_segment::SegmentFs;
    let root = temp_root();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let key = observer_segment::segment_for(now, DEFAULT_SEGMENT_SECS);
    let mut previous = platform_win::LocalSegmentFs::new(root.clone());
    previous.open_incomplete(key).unwrap();
    previous
        .write_chunk(
            key,
            &CaptureChunk {
                source: SourceKind::Mic,
                seq: 1,
                data: pcm_i16_amplitude(1600, 1200),
                format: Some(audio_format()),
                at_us: Some(10_000),
            },
        )
        .unwrap();
    drop(previous); // process ends without finalize
    let clock = FakeClock::new(now);
    let mut recovery = platform_win::LocalRecoveryFs::new(root.clone(), DEFAULT_SEGMENT_SECS);
    let (mut engine, _) = CaptureEngine::new(
        active_mic_sources(),
        EngineConfig::default(),
        &mut recovery,
        platform_win::LocalSegmentFs::new(root.clone()),
        Box::new(clock.clone()),
    )
    .unwrap();
    engine.set_source_choices(observer_model::setup::SetupSnapshot::default());
    engine.start();
    clock.set(key.boundary_epoch_secs + DEFAULT_SEGMENT_SECS);
    engine.pump();
    let store = LocalSealedStore::new(&root, DEFAULT_SEGMENT_SECS);
    let sealed = store.scan().unwrap();
    assert_eq!(sealed.len(), 1);
    assert_eq!(sealed[0].boundary_epoch_secs, key.boundary_epoch_secs);
    let samples =
        decode_flac_stereo_mic(&store.read_file(sealed[0].index, AUDIO_FILE_NAME).unwrap());
    assert_eq!(
        samples.iter().filter(|&&sample| sample == 1200).count(),
        1600
    );
    assert!(!root.join("quarantine").exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn restarted_process_preserves_gap_with_reset_monotonic_clock() {
    use observer_segment::SegmentFs;
    struct RestartClock(u64);
    impl Clock for RestartClock {
        fn now_epoch_secs(&self) -> u64 {
            self.0
        }
        fn now_epoch_us(&self) -> u64 {
            self.0 * 1_000_000 + 500_000
        }
        fn now_monotonic_us(&self) -> u64 {
            0
        }
    }
    let root = temp_root();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let key = observer_segment::segment_for(now, DEFAULT_SEGMENT_SECS);
    let mut previous = platform_win::LocalSegmentFs::new(root.clone());
    previous.open_incomplete(key).unwrap();
    previous
        .write_chunk(
            key,
            &CaptureChunk {
                source: SourceKind::Mic,
                seq: 1,
                data: pcm_i16_amplitude(1600, 1200),
                format: Some(audio_format()),
                at_us: Some(10_000),
            },
        )
        .unwrap();
    drop(previous);
    let mut recovery = platform_win::LocalRecoveryFs::new(root.clone(), DEFAULT_SEGMENT_SECS);
    let (mut engine, _) = CaptureEngine::new(
        active_mic_sources(),
        EngineConfig::default(),
        &mut recovery,
        platform_win::LocalSegmentFs::new(root.clone()),
        Box::new(RestartClock(now)),
    )
    .unwrap();
    engine.set_source_choices(observer_model::setup::SetupSnapshot::default());
    engine.start();
    engine.apply_command(EngineCommand::SetSourceEnabled {
        kind: SourceKind::Mic,
        enabled: true,
    });
    emit_mic(&engine.sink(), 2, 1600, 2400, 10_000);
    engine.pump();
    engine.stop();
    let store = LocalSealedStore::new(&root, DEFAULT_SEGMENT_SECS);
    let sealed = store.scan().unwrap();
    assert_eq!(sealed.len(), 1);
    let samples =
        decode_flac_stereo_mic(&store.read_file(sealed[0].index, AUDIO_FILE_NAME).unwrap());
    let first_b = samples.iter().position(|&sample| sample == 2400).unwrap();
    assert_eq!(
        first_b,
        ((now - key.boundary_epoch_secs) * 16_000 + 8_160) as usize
    );
    assert_eq!(
        samples.iter().filter(|&&sample| sample == 1200).count(),
        1600
    );
    assert_eq!(
        samples.iter().filter(|&&sample| sample == 2400).count(),
        1600
    );
    assert!(samples[1_760..first_b].iter().all(|&sample| sample == 0));
    assert!(!root.join("quarantine").exists());
    std::fs::remove_dir_all(root).unwrap();
}

struct FaultyAppendSegmentFs {
    inner: platform_win::LocalSegmentFs,
    fail_append: Arc<std::sync::atomic::AtomicBool>,
}

impl observer_segment::SegmentFs for FaultyAppendSegmentFs {
    type Error = std::io::Error;

    fn existing_incomplete(
        &mut self,
        key: observer_model::SegmentKey,
    ) -> Result<Option<String>, Self::Error> {
        self.inner.existing_incomplete(key)
    }

    fn open_incomplete(&mut self, key: observer_model::SegmentKey) -> Result<String, Self::Error> {
        self.inner.open_incomplete(key)
    }

    fn write_chunk(
        &mut self,
        key: observer_model::SegmentKey,
        chunk: &CaptureChunk,
    ) -> Result<(), Self::Error> {
        self.inner.write_chunk(key, chunk)
    }

    fn append_activity(
        &mut self,
        key: observer_model::SegmentKey,
        file_name: &str,
        line: &str,
        committed_len: u64,
    ) -> Result<(), Self::Error> {
        if self.fail_append.load(Ordering::SeqCst) {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "simulated append failure",
            ))
        } else {
            self.inner
                .append_activity(key, file_name, line, committed_len)
        }
    }

    fn finalize(
        &mut self,
        key: observer_model::SegmentKey,
        video_end_secs: Option<f64>,
    ) -> Result<(), Self::Error> {
        self.inner.finalize(key, video_end_secs)
    }
}

#[test]
fn activity_only_seals_segment_and_doesnt_start_media_four_off_creates_none() {
    let root = temp_root();
    let _ = std::fs::remove_dir_all(&root);
    let clock = FakeClock::new(100);
    let (mut sources1, _screen) = sources();
    let activity_handle = FakeDesktopActivityHandle::default();
    let fake_activity = FakeDesktopActivity {
        inner: Arc::new(Mutex::new(activity_handle)),
    };
    sources1.desktop_activity = Box::new(fake_activity.clone());

    let mut recovery = platform_win::LocalRecoveryFs::new(root.clone(), DEFAULT_SEGMENT_SECS);
    let segment_fs = platform_win::LocalSegmentFs::new(root.clone());
    let (mut engine, _) = CaptureEngine::new(
        sources1,
        EngineConfig::default(),
        &mut recovery,
        segment_fs,
        Box::new(clock.clone()),
    )
    .unwrap();

    // Part A: All 4 off creates none
    engine.set_source_choices(observer_model::setup::SetupSnapshot {
        mic: false,
        system_audio: false,
        screen: false,
        desktop_activity: false,
        ..Default::default()
    });
    engine.start();
    clock.set(450); // Cross 300 boundary
    engine.pump();
    engine.stop();

    let store = LocalSealedStore::new(&root, DEFAULT_SEGMENT_SECS);
    assert!(
        store.scan().unwrap().is_empty(),
        "all 4 off must create no segments"
    );

    // Part B: Desktop activity enabled only
    let (mut sources2, _screen2) = sources();
    let mut activity_handle2 = FakeDesktopActivityHandle::default();
    activity_handle2
        .events
        .push(DesktopActivityProbeEvent::Foreground {
            window_id: 1,
            app: "TestApp".into(),
            exe_name: "test.exe".into(),
            title: "Test Title".into(),
        });
    let fake_activity2 = FakeDesktopActivity {
        inner: Arc::new(Mutex::new(activity_handle2)),
    };
    sources2.desktop_activity = Box::new(fake_activity2);

    let mut recovery2 = platform_win::LocalRecoveryFs::new(root.clone(), DEFAULT_SEGMENT_SECS);
    let segment_fs2 = platform_win::LocalSegmentFs::new(root.clone());
    let (mut engine2, _) = CaptureEngine::new(
        sources2,
        EngineConfig::default(),
        &mut recovery2,
        segment_fs2,
        Box::new(clock.clone()),
    )
    .unwrap();

    engine2.set_source_choices(observer_model::setup::SetupSnapshot {
        mic: false,
        system_audio: false,
        screen: false,
        desktop_activity: true,
        ..Default::default()
    });
    clock.set(500);
    engine2.start();
    engine2.pump();
    clock.set(550);
    engine2.pump();
    engine2.stop();

    let sealed = store.scan().unwrap();
    assert_eq!(sealed.len(), 1, "activity only must seal a segment");
    let seg_idx = sealed[0].index;
    assert!(
        store.read_file(seg_idx, AUDIO_FILE_NAME).is_err(),
        "media (audio.flac) must not be started"
    );
    assert!(
        store.read_file(seg_idx, "screen.mp4").is_err(),
        "media (screen.mp4) must not be started"
    );
    let files = &sealed[0].files;
    assert!(
        files.iter().any(|f| f.ends_with("_desktop_activity.jsonl")),
        "activity file must be present"
    );
    assert_eq!(sealed[0].len_secs, Some(300), "len must be 300");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn two_runs_in_one_boundary_before_ack_and_after_ack() {
    let root = temp_root();
    let _ = std::fs::remove_dir_all(&root);
    let clock = FakeClock::new(100);

    // Run 1:
    let (mut s1, _) = sources();
    let mut ah1 = FakeDesktopActivityHandle::default();
    ah1.events.push(DesktopActivityProbeEvent::Foreground {
        window_id: 1,
        app: "Run1App".into(),
        exe_name: "app1.exe".into(),
        title: "Title 1".into(),
    });
    s1.desktop_activity = Box::new(FakeDesktopActivity {
        inner: Arc::new(Mutex::new(ah1)),
    });
    let mut r1 = platform_win::LocalRecoveryFs::new(root.clone(), DEFAULT_SEGMENT_SECS);
    let (mut engine1, _) = CaptureEngine::new(
        s1,
        EngineConfig::default(),
        &mut r1,
        platform_win::LocalSegmentFs::new(root.clone()),
        Box::new(clock.clone()),
    )
    .unwrap();
    engine1.set_source_choices(observer_model::setup::SetupSnapshot {
        desktop_activity: true,
        ..Default::default()
    });
    engine1.start();
    engine1.pump();
    engine1.stop(); // Seals segment at key for t=100 (boundary 0..300)

    let store = LocalSealedStore::new(&root, DEFAULT_SEGMENT_SECS);
    let sealed = store.scan().unwrap();
    assert_eq!(sealed.len(), 1);
    let files1 = &sealed[0].files;
    let act_file1 = files1
        .iter()
        .find(|f| f.ends_with("_desktop_activity.jsonl"))
        .unwrap()
        .clone();
    let content1 = store.read_file(sealed[0].index, &act_file1).unwrap();

    // Run 2: in same boundary (t=200) before ack
    clock.set(200);
    let (mut s2, _) = sources();
    let mut ah2 = FakeDesktopActivityHandle::default();
    ah2.events.push(DesktopActivityProbeEvent::Foreground {
        window_id: 2,
        app: "Run2App".into(),
        exe_name: "app2.exe".into(),
        title: "Title 2".into(),
    });
    s2.desktop_activity = Box::new(FakeDesktopActivity {
        inner: Arc::new(Mutex::new(ah2)),
    });
    let mut r2 = platform_win::LocalRecoveryFs::new(root.clone(), DEFAULT_SEGMENT_SECS);
    let (mut engine2, _) = CaptureEngine::new(
        s2,
        EngineConfig::default(),
        &mut r2,
        platform_win::LocalSegmentFs::new(root.clone()),
        Box::new(clock.clone()),
    )
    .unwrap();
    engine2.set_source_choices(observer_model::setup::SetupSnapshot {
        desktop_activity: true,
        ..Default::default()
    });
    engine2.start();
    engine2.pump();
    engine2.stop(); // Seals again into same boundary

    let sealed2 = store.scan().unwrap();
    let files2 = &sealed2[0].files;
    let act_files2: Vec<_> = files2
        .iter()
        .filter(|f| f.ends_with("_desktop_activity.jsonl"))
        .collect();
    assert_eq!(
        act_files2.len(),
        2,
        "two activity files must exist in same sealed segment"
    );
    assert_ne!(act_files2[0], act_files2[1], "names must differ");
    // Prior bytes remain:
    assert_eq!(
        store.read_file(sealed2[0].index, &act_file1).unwrap(),
        content1
    );

    // Stale .uploaded simulation:
    // Mark act_file1 as uploaded:
    let seg_dir = root.join(sealed2[0].index.to_string());
    std::fs::write(seg_dir.join(".uploaded"), b"").unwrap();

    // Run 3: after earlier ack, still in same boundary (t=250)
    clock.set(250);
    let (mut s3, _) = sources();
    let mut ah3 = FakeDesktopActivityHandle::default();
    ah3.events.push(DesktopActivityProbeEvent::Foreground {
        window_id: 3,
        app: "Run3App".into(),
        exe_name: "app3.exe".into(),
        title: "Title 3".into(),
    });
    s3.desktop_activity = Box::new(FakeDesktopActivity {
        inner: Arc::new(Mutex::new(ah3)),
    });
    let mut r3 = platform_win::LocalRecoveryFs::new(root.clone(), DEFAULT_SEGMENT_SECS);
    let (mut engine3, _) = CaptureEngine::new(
        s3,
        EngineConfig::default(),
        &mut r3,
        platform_win::LocalSegmentFs::new(root.clone()),
        Box::new(clock.clone()),
    )
    .unwrap();
    engine3.set_source_choices(observer_model::setup::SetupSnapshot {
        desktop_activity: true,
        ..Default::default()
    });
    engine3.start();
    engine3.pump();
    engine3.stop();

    // Stale .uploaded was removed by seal_or_merge, so segment is scanned and pending
    let sealed3 = store.scan().unwrap();
    assert_eq!(sealed3.len(), 1, "stale .uploaded must not hide segment");
    let files3 = &sealed3[0].files;
    let act_files3: Vec<_> = files3
        .iter()
        .filter(|f| f.ends_with("_desktop_activity.jsonl"))
        .collect();
    assert_eq!(act_files3.len(), 3, "three activity files must exist");
    // Check .len file is 300
    assert_eq!(sealed3[0].len_secs, Some(300), "len must be 300");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn multi_boundary_step_keeps_emitted_intervals_inside_delivered_segment_range() {
    let root = temp_root();
    let _ = std::fs::remove_dir_all(&root);
    let clock = FakeClock::new(100);

    let (mut s, _) = sources();
    let mut ah = FakeDesktopActivityHandle::default();
    ah.events.push(DesktopActivityProbeEvent::Foreground {
        window_id: 1,
        app: "LongApp".into(),
        exe_name: "long.exe".into(),
        title: "Long Running App".into(),
    });
    s.desktop_activity = Box::new(FakeDesktopActivity {
        inner: Arc::new(Mutex::new(ah)),
    });
    let mut r = platform_win::LocalRecoveryFs::new(root.clone(), DEFAULT_SEGMENT_SECS);
    let (mut engine, _) = CaptureEngine::new(
        s,
        EngineConfig::default(),
        &mut r,
        platform_win::LocalSegmentFs::new(root.clone()),
        Box::new(clock.clone()),
    )
    .unwrap();
    engine.set_source_choices(observer_model::setup::SetupSnapshot {
        desktop_activity: true,
        ..Default::default()
    });
    engine.start();
    engine.pump(); // Starts in segment 0..300 at t=100

    // Step across multiple boundaries sequentially
    clock.set(350);
    engine.pump();
    clock.set(650);
    engine.pump();
    clock.set(750);
    engine.stop();

    let store = LocalSealedStore::new(&root, DEFAULT_SEGMENT_SECS);
    let sealed = store.scan().unwrap();
    assert_eq!(
        sealed.len(),
        3,
        "must have sealed 3 segments for 0..300, 300..600, 600..900"
    );

    for seg in &sealed {
        let seg_start_ms = seg.boundary_epoch_secs * 1000;
        let seg_end_ms = (seg.boundary_epoch_secs + DEFAULT_SEGMENT_SECS) * 1000;
        let files = &seg.files;
        let act_file = files
            .iter()
            .find(|f| f.ends_with("_desktop_activity.jsonl"))
            .unwrap();
        let content = String::from_utf8(store.read_file(seg.index, act_file).unwrap()).unwrap();
        for line in content.lines() {
            let row: serde_json::Value = serde_json::from_str(line).unwrap();
            let row_kind = row.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            if row_kind == "foreground" || row_kind == "activity" {
                let ts = row.get("ts").and_then(|v| v.as_u64()).unwrap();
                let end_ts = row.get("end_ts").and_then(|v| v.as_u64()).unwrap();
                assert!(
                    ts >= seg_start_ms,
                    "row ts {ts} must be >= seg_start_ms {seg_start_ms}"
                );
                assert!(
                    end_ts <= seg_end_ms,
                    "row end_ts {end_ts} must be <= seg_end_ms {seg_end_ms}"
                );
            }
        }
    }

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn clock_jump_then_stop_delivers_intervals_for_all_crossed_boundaries() {
    let root = temp_root();
    let _ = std::fs::remove_dir_all(&root);
    let clock = FakeClock::new(100);

    let (mut s, _) = sources();
    let mut ah = FakeDesktopActivityHandle::default();
    ah.events.push(DesktopActivityProbeEvent::Foreground {
        window_id: 1,
        app: "LongApp".into(),
        exe_name: "long.exe".into(),
        title: "Long Running App".into(),
    });
    s.desktop_activity = Box::new(FakeDesktopActivity {
        inner: Arc::new(Mutex::new(ah)),
    });
    let mut r = platform_win::LocalRecoveryFs::new(root.clone(), DEFAULT_SEGMENT_SECS);
    let (mut engine, _) = CaptureEngine::new(
        s,
        EngineConfig::default(),
        &mut r,
        platform_win::LocalSegmentFs::new(root.clone()),
        Box::new(clock.clone()),
    )
    .unwrap();
    engine.set_source_choices(observer_model::setup::SetupSnapshot {
        desktop_activity: true,
        ..Default::default()
    });
    engine.start();
    engine.pump(); // Clock at 100

    clock.set(950);
    engine.stop();

    let store = LocalSealedStore::new(&root, DEFAULT_SEGMENT_SECS);
    let sealed = store.scan().unwrap();
    let boundaries: Vec<u64> = sealed.iter().map(|s| s.boundary_epoch_secs).collect();
    assert_eq!(
        boundaries,
        vec![0, 300, 600, 900],
        "must have sealed 4 segments for boundaries 0, 300, 600, 900"
    );

    for seg in &sealed {
        let seg_start_ms = seg.boundary_epoch_secs * 1000;
        let seg_end_ms = (seg.boundary_epoch_secs + DEFAULT_SEGMENT_SECS) * 1000;
        let files = &seg.files;
        let act_file = files
            .iter()
            .find(|f| f.ends_with("_desktop_activity.jsonl"))
            .unwrap();
        let content = String::from_utf8(store.read_file(seg.index, act_file).unwrap()).unwrap();
        for line in content.lines() {
            let row: serde_json::Value = serde_json::from_str(line).unwrap();
            let row_kind = row.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            if row_kind == "foreground" || row_kind == "activity" {
                let ts = row.get("ts").and_then(|v| v.as_u64()).unwrap();
                let end_ts = row.get("end_ts").and_then(|v| v.as_u64()).unwrap();
                assert!(
                    ts >= seg_start_ms,
                    "row ts {ts} must be >= seg_start_ms {seg_start_ms} for segment {}",
                    seg.boundary_epoch_secs
                );
                assert!(
                    end_ts <= seg_end_ms,
                    "row end_ts {end_ts} must be <= seg_end_ms {seg_end_ms} for segment {}",
                    seg.boundary_epoch_secs
                );
            }
        }
    }

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn failed_append_stays_queued_fault_remains_until_retry_writes_line_once_detail_has_no_row_text() {
    let root = temp_root();
    let _ = std::fs::remove_dir_all(&root);
    let clock = FakeClock::new(100);

    let (mut s, _) = sources();
    let mut ah = FakeDesktopActivityHandle::default();
    ah.events.push(DesktopActivityProbeEvent::Foreground {
        window_id: 42,
        app: "SecretSuperSensitiveApp".into(),
        exe_name: "sensitive.exe".into(),
        title: "SuperSecretConfidentialWindowTitle".into(),
    });
    ah.events
        .push(DesktopActivityProbeEvent::ForegroundUnavailable);
    s.desktop_activity = Box::new(FakeDesktopActivity {
        inner: Arc::new(Mutex::new(ah)),
    });

    let fail_flag = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let faulty_fs = FaultyAppendSegmentFs {
        inner: platform_win::LocalSegmentFs::new(root.clone()),
        fail_append: fail_flag.clone(),
    };

    let mut r = platform_win::LocalRecoveryFs::new(root.clone(), DEFAULT_SEGMENT_SECS);
    let (mut engine, _) = CaptureEngine::new(
        s,
        EngineConfig::default(),
        &mut r,
        faulty_fs,
        Box::new(clock.clone()),
    )
    .unwrap();
    engine.set_source_choices(observer_model::setup::SetupSnapshot {
        desktop_activity: true,
        desktop_activity_window_titles: true,
        ..Default::default()
    });
    engine.start();

    // Pump when fail_flag is true: append_activity fails!
    engine.pump();

    // 1. Fault remains:
    let dump = engine.health_dump();
    match dump.storage {
        Some(StorageHealth { ref detail }) => {
            // 2. Detail has NO row text (no app name, no window title)
            assert!(!detail.contains("SecretSuperSensitiveApp"));
            assert!(!detail.contains("SuperSecretConfidentialWindowTitle"));
        }
        None => panic!("expected storage fault, got None"),
    }

    // 3. Clear the failure condition and retry
    fail_flag.store(false, Ordering::SeqCst);
    engine.pump();

    // Fault cleared
    assert_eq!(engine.health_dump().storage, None);

    // Stop and inspect sealed segment
    engine.stop();

    let store = LocalSealedStore::new(&root, DEFAULT_SEGMENT_SECS);
    let sealed = store.scan().unwrap();
    assert_eq!(sealed.len(), 1);
    let files = &sealed[0].files;
    let act_file = files
        .iter()
        .find(|f| f.ends_with("_desktop_activity.jsonl"))
        .unwrap();
    let content = String::from_utf8(store.read_file(sealed[0].index, act_file).unwrap()).unwrap();

    // 4. Line written ONCE
    let fg_lines: Vec<_> = content
        .lines()
        .filter(|l| l.contains("SuperSecretConfidentialWindowTitle"))
        .collect();
    assert_eq!(
        fg_lines.len(),
        1,
        "failed row must be written exactly once upon retry"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn append_activity_local_fs_no_nul_prefix_and_line_two_adjacent() {
    let root = temp_root();
    let _ = std::fs::remove_dir_all(&root);
    let clock = FakeClock::new(100);

    let (mut s, _) = sources();
    let mut ah = FakeDesktopActivityHandle::default();
    ah.events.push(DesktopActivityProbeEvent::Foreground {
        window_id: 1,
        app: "AppOne".into(),
        exe_name: "one.exe".into(),
        title: "Title One".into(),
    });
    ah.events.push(DesktopActivityProbeEvent::Foreground {
        window_id: 2,
        app: "AppTwo".into(),
        exe_name: "two.exe".into(),
        title: "Title Two".into(),
    });
    s.desktop_activity = Box::new(FakeDesktopActivity {
        inner: Arc::new(Mutex::new(ah)),
    });

    let mut r = platform_win::LocalRecoveryFs::new(root.clone(), DEFAULT_SEGMENT_SECS);
    let (mut engine, _) = CaptureEngine::new(
        s,
        EngineConfig::default(),
        &mut r,
        platform_win::LocalSegmentFs::new(root.clone()),
        Box::new(clock.clone()),
    )
    .unwrap();
    engine.set_source_choices(observer_model::setup::SetupSnapshot {
        desktop_activity: true,
        desktop_activity_window_titles: true,
        ..Default::default()
    });
    engine.start();
    engine.pump();
    clock.set(150);
    engine.stop();

    let store = LocalSealedStore::new(&root, DEFAULT_SEGMENT_SECS);
    let sealed = store.scan().unwrap();
    assert_eq!(sealed.len(), 1);
    let files = &sealed[0].files;
    let act_file = files
        .iter()
        .find(|f| f.ends_with("_desktop_activity.jsonl"))
        .unwrap()
        .clone();
    let bytes = store.read_file(sealed[0].index, &act_file).unwrap();

    // 1. the activity file's first byte is `{`
    assert_eq!(bytes[0], b'{', "the activity file's first byte must be {{");
    // 2. the file contains no NUL
    assert!(!bytes.contains(&0), "the file must contain no NUL");

    let content = String::from_utf8(bytes.clone()).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert!(lines.len() >= 2, "must have at least two lines");
    let fg_lines: Vec<_> = lines
        .iter()
        .filter(|l| l.contains("\"foreground\""))
        .collect();
    assert_eq!(fg_lines.len(), 2, "must have two closed foreground rows");

    // 3. line 2 begins immediately after line 1
    assert_eq!(
        &bytes[lines[0].len()..lines[0].len() + 2],
        b"\n{",
        "line 2 must begin immediately after line 1"
    );

    // Drop that engine. Construct a new engine on the same directory so recovery runs.
    drop(engine);

    let clock2 = FakeClock::new(200);
    let (mut s2, _) = sources();
    let mut ah2 = FakeDesktopActivityHandle::default();
    ah2.events.push(DesktopActivityProbeEvent::Foreground {
        window_id: 3,
        app: "AppThree".into(),
        exe_name: "three.exe".into(),
        title: "Title Three".into(),
    });
    s2.desktop_activity = Box::new(FakeDesktopActivity {
        inner: Arc::new(Mutex::new(ah2)),
    });
    let mut r2 = platform_win::LocalRecoveryFs::new(root.clone(), DEFAULT_SEGMENT_SECS);
    let (mut engine2, _) = CaptureEngine::new(
        s2,
        EngineConfig::default(),
        &mut r2,
        platform_win::LocalSegmentFs::new(root.clone()),
        Box::new(clock2.clone()),
    )
    .unwrap();
    engine2.set_source_choices(observer_model::setup::SetupSnapshot {
        desktop_activity: true,
        desktop_activity_window_titles: true,
        ..Default::default()
    });
    engine2.start();

    // The sealed file is still there with those same bytes.
    let bytes_after_recovery = store.read_file(sealed[0].index, &act_file).unwrap();
    assert_eq!(
        bytes_after_recovery, bytes,
        "the sealed file is still there with those same bytes"
    );

    // A later batch uses a different file name and does not rewrite the first file.
    engine2.pump();
    clock2.set(250);
    engine2.stop();

    let sealed2 = store.scan().unwrap();
    assert_eq!(sealed2.len(), 1);
    let files2 = &sealed2[0].files;
    let act_files: Vec<_> = files2
        .iter()
        .filter(|f| f.ends_with("_desktop_activity.jsonl"))
        .collect();
    assert_eq!(act_files.len(), 2, "a later batch uses a different file name");
    assert!(act_files.contains(&&act_file));
    let original_bytes = store.read_file(sealed2[0].index, &act_file).unwrap();
    assert_eq!(
        original_bytes, bytes,
        "first file is not rewritten by later batch"
    );

    let _ = std::fs::remove_dir_all(&root);
}

