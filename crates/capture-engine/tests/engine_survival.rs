// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use capture_engine::{CaptureEngine, EngineCommand, EngineConfig, Sources};
use observer_model::{
    AudioFormat, CaptureChunk, CaptureSink, Clock, EncoderError, EncoderHealth, MicSource,
    ScreenEncoder, ScreenFrame, ScreenSource, SourceError, SourceKind, SourceState,
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
