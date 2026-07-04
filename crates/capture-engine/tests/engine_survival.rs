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

fn audio_format() -> AudioFormat {
    AudioFormat {
        sample_rate_hz: 16_000,
        channels: 1,
        bits_per_sample: 16,
        is_float: false,
    }
}

fn pcm_i16(samples: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples * 2);
    for _ in 0..samples {
        bytes.extend_from_slice(&1000i16.to_le_bytes());
    }
    bytes
}

fn emit_system_audio(sink: &Arc<dyn CaptureSink>, seq: u64, samples: usize) {
    sink.emit(CaptureChunk {
        source: SourceKind::SystemAudio,
        seq,
        data: pcm_i16(samples),
        format: Some(audio_format()),
    });
}

#[test]
fn pause_resume_same_window_seals_once_and_preserves_audio_duration() {
    let root = temp_root();
    let _ = std::fs::remove_dir_all(&root);
    let clock = FakeClock::new(100);
    let (sources, screen) = sources();
    let mut recovery = platform_win::LocalRecoveryFs::new(root.clone());
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
