// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Pure seal-time audio transform.

#![forbid(unsafe_code)]

use flacenc::component::BitRepr;
use flacenc::error::Verify;
use flacenc::source::MemSource;
use observer_model::AudioFormat;
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};
use thiserror::Error;

const TARGET_RATE_HZ: u32 = 16_000;
const OUTPUT_CHANNELS: usize = 2;
const MIN_FLAC_BLOCK_FRAMES: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AudioError {
    #[error("unsupported audio format: {channels} channels, {bits} bits, float={is_float}")]
    UnsupportedFormat {
        channels: u16,
        bits: u16,
        is_float: bool,
    },
    #[error("invalid audio format: sample_rate_hz={sample_rate_hz}, channels={channels}")]
    InvalidFormat { sample_rate_hz: u32, channels: u16 },
    #[error("FLAC encode failed: {0}")]
    Encode(String),
    #[error("audio resample failed: {0}")]
    Resample(String),
}

pub fn combine_to_flac(
    mic: Option<(&[u8], AudioFormat)>,
    sys: Option<(&[u8], AudioFormat)>,
) -> Result<Option<Vec<u8>>, AudioError> {
    if mic.is_none() && sys.is_none() {
        return Ok(None);
    }

    let mic = match mic {
        Some((bytes, format)) => decode_source(bytes, format)?,
        None => Vec::new(),
    };
    let sys = match sys {
        Some((bytes, format)) => decode_source(bytes, format)?,
        None => Vec::new(),
    };

    let frames = mic.len().max(sys.len());
    let mut samples = Vec::with_capacity(frames * OUTPUT_CHANNELS);
    for i in 0..frames {
        samples.push(f32_to_i16(mic.get(i).copied().unwrap_or(0.0)) as i32);
        samples.push(f32_to_i16(sys.get(i).copied().unwrap_or(0.0)) as i32);
    }

    encode_flac(&samples).map(Some)
}

fn decode_source(bytes: &[u8], format: AudioFormat) -> Result<Vec<f32>, AudioError> {
    validate_format(format)?;
    let mono = decode_to_mono(bytes, format)?;
    resample_to_16k(mono, format.sample_rate_hz)
}

fn validate_format(format: AudioFormat) -> Result<(), AudioError> {
    if format.sample_rate_hz == 0 || format.channels == 0 {
        return Err(AudioError::InvalidFormat {
            sample_rate_hz: format.sample_rate_hz,
            channels: format.channels,
        });
    }
    match (format.is_float, format.bits_per_sample) {
        (false, 16) | (true, 32) => Ok(()),
        _ => Err(AudioError::UnsupportedFormat {
            channels: format.channels,
            bits: format.bits_per_sample,
            is_float: format.is_float,
        }),
    }
}

fn decode_to_mono(bytes: &[u8], format: AudioFormat) -> Result<Vec<f32>, AudioError> {
    let channels = format.channels as usize;
    let bytes_per_sample = (format.bits_per_sample / 8) as usize;
    let frame_size =
        channels
            .checked_mul(bytes_per_sample)
            .ok_or(AudioError::UnsupportedFormat {
                channels: format.channels,
                bits: format.bits_per_sample,
                is_float: format.is_float,
            })?;
    let usable_len = bytes.len() / frame_size * frame_size;
    let frame_count = usable_len / frame_size;
    let mut mono = Vec::with_capacity(frame_count);

    for frame in bytes[..usable_len].chunks_exact(frame_size) {
        let mut sum = 0.0f32;
        for channel in 0..channels {
            let offset = channel * bytes_per_sample;
            let sample = if format.is_float {
                f32::from_le_bytes([
                    frame[offset],
                    frame[offset + 1],
                    frame[offset + 2],
                    frame[offset + 3],
                ])
            } else {
                i16::from_le_bytes([frame[offset], frame[offset + 1]]) as f32 / 32768.0
            };
            sum += sample;
        }
        mono.push(sum / channels as f32);
    }

    Ok(mono)
}

fn resample_to_16k(input: Vec<f32>, sample_rate_hz: u32) -> Result<Vec<f32>, AudioError> {
    if sample_rate_hz == TARGET_RATE_HZ || input.is_empty() {
        return Ok(input);
    }

    let params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        oversampling_factor: 128,
        interpolation: SincInterpolationType::Cubic,
        window: WindowFunction::BlackmanHarris2,
    };
    let mut resampler = Async::<f32>::new_sinc(
        TARGET_RATE_HZ as f64 / sample_rate_hz as f64,
        1.0,
        &params,
        1024,
        1,
        FixedAsync::Input,
    )
    .map_err(|err| AudioError::Resample(err.to_string()))?;

    let output_frames = resampler.process_all_needed_output_len(input.len());
    let input_adapter = InterleavedSlice::new(&input, 1, input.len())
        .map_err(|err| AudioError::Resample(err.to_string()))?;
    let mut output = vec![0.0; output_frames];
    let mut output_adapter = InterleavedSlice::new_mut(&mut output, 1, output_frames)
        .map_err(|err| AudioError::Resample(err.to_string()))?;
    let (_, written) = resampler
        .process_all_into_buffer(&input_adapter, &mut output_adapter, input.len(), None)
        .map_err(|err| AudioError::Resample(err.to_string()))?;
    output.truncate(written);
    Ok(output)
}

fn f32_to_i16(sample: f32) -> i16 {
    if !sample.is_finite() {
        return 0;
    }
    (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

fn encode_flac(samples: &[i32]) -> Result<Vec<u8>, AudioError> {
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|err| AudioError::Encode(format!("{err:?}")))?;
    let frame_count = samples.len() / OUTPUT_CHANNELS;
    let mut padded;
    let encode_samples = if (1..MIN_FLAC_BLOCK_FRAMES).contains(&frame_count) {
        padded = samples.to_vec();
        padded.resize(MIN_FLAC_BLOCK_FRAMES * OUTPUT_CHANNELS, 0);
        padded.as_slice()
    } else {
        samples
    };
    let source =
        MemSource::from_samples(encode_samples, OUTPUT_CHANNELS, 16, TARGET_RATE_HZ as usize);
    let stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|err| AudioError::Encode(err.to_string()))?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|err| AudioError::Encode(err.to_string()))?;
    Ok(sink.as_slice().to_vec())
}

pub fn flac_duration_secs(flac: &[u8]) -> Option<f64> {
    if flac.len() < 42 || &flac[0..4] != b"fLaC" || (flac[4] & 0x7f) != 0 {
        return None;
    }

    let field = u64::from_be_bytes(flac[18..26].try_into().ok()?);
    let rate = (field >> 44) & 0xF_FFFF;
    let total = field & 0xF_FFFF_FFFF;
    if rate == 0 || total == 0 {
        return None;
    }

    Some(total as f64 / rate as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use claxon::FlacReader;
    use std::f32::consts::TAU;
    use std::io::Cursor;

    fn i16_format(sample_rate_hz: u32, channels: u16) -> AudioFormat {
        AudioFormat {
            sample_rate_hz,
            channels,
            bits_per_sample: 16,
            is_float: false,
        }
    }

    fn f32_format(sample_rate_hz: u32, channels: u16) -> AudioFormat {
        AudioFormat {
            sample_rate_hz,
            channels,
            bits_per_sample: 32,
            is_float: true,
        }
    }

    fn i16_bytes(samples: &[i16]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        bytes
    }

    fn f32_bytes(samples: &[f32]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(samples.len() * 4);
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        bytes
    }

    fn decode(bytes: &[u8]) -> (u32, u32, u32, Vec<i32>) {
        let mut reader = FlacReader::new(Cursor::new(bytes)).unwrap();
        let info = reader.streaminfo();
        let sample_rate = info.sample_rate;
        let channels = info.channels;
        let bits_per_sample = info.bits_per_sample;
        let samples = reader
            .samples()
            .collect::<Result<Vec<i32>, claxon::Error>>()
            .unwrap();
        (sample_rate, channels, bits_per_sample, samples)
    }

    fn left(samples: &[i32]) -> Vec<i32> {
        samples.chunks_exact(2).map(|frame| frame[0]).collect()
    }

    fn right(samples: &[i32]) -> Vec<i32> {
        samples.chunks_exact(2).map(|frame| frame[1]).collect()
    }

    #[test]
    fn flac_duration_reads_streaminfo_total_samples() {
        let pcm = i16_bytes(&[1000; 32_000]);
        let flac = combine_to_flac(Some((&pcm, i16_format(16_000, 1))), None)
            .unwrap()
            .unwrap();

        let duration = flac_duration_secs(&flac).unwrap();

        assert!((duration - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn flac_duration_uses_padded_count_for_sub_minimum_block() {
        let pcm = i16_bytes(&[1000]);
        let flac = combine_to_flac(Some((&pcm, i16_format(16_000, 1))), None)
            .unwrap()
            .unwrap();

        let duration = flac_duration_secs(&flac).unwrap();

        assert!((duration - (16.0 / 16_000.0)).abs() < f64::EPSILON);
    }

    #[test]
    fn flac_duration_rejects_invalid_or_truncated_input() {
        assert_eq!(flac_duration_secs(b"XXXX"), None);
        assert_eq!(flac_duration_secs(&[0; 41]), None);
    }

    #[test]
    fn ac2_encodes_flac_that_decodes_to_16k_stereo_16_bit() {
        let pcm = i16_bytes(&[1000; 16]);
        let flac = combine_to_flac(Some((&pcm, i16_format(16_000, 1))), None)
            .unwrap()
            .unwrap();

        let (rate, channels, bits, samples) = decode(&flac);

        assert_eq!(rate, 16_000);
        assert_eq!(channels, 2);
        assert_eq!(bits, 16);
        assert_eq!(samples.len(), 32);
    }

    #[test]
    fn encodes_sub_minimum_block_audio_as_valid_flac() {
        let mic = i16_bytes(&[1000]);
        let sys = i16_bytes(&[-2000]);
        let flac = combine_to_flac(
            Some((&mic, i16_format(16_000, 1))),
            Some((&sys, i16_format(16_000, 1))),
        )
        .unwrap()
        .unwrap();

        let (_, _, _, samples) = decode(&flac);

        assert_eq!(samples.len(), MIN_FLAC_BLOCK_FRAMES * OUTPUT_CHANNELS);
        assert!((samples[0] - 1000).abs() <= 1);
        assert!((samples[1] + 2000).abs() <= 1);
        assert!(left(&samples)[1..].iter().all(|sample| *sample == 0));
        assert!(right(&samples)[1..].iter().all(|sample| *sample == 0));
    }

    #[test]
    fn ac3_channel_zero_is_mic_and_channel_one_is_system() {
        let mic = i16_bytes(&[16_384; 16]);
        let sys = i16_bytes(&[-16_384; 16]);
        let flac = combine_to_flac(
            Some((&mic, i16_format(16_000, 1))),
            Some((&sys, i16_format(16_000, 1))),
        )
        .unwrap()
        .unwrap();

        let (_, _, _, samples) = decode(&flac);

        for frame in samples.chunks_exact(2) {
            assert!((frame[0] - 16_384).abs() <= 1);
            assert!((frame[1] + 16_384).abs() <= 1);
        }
    }

    #[test]
    fn ac4_resamples_48000_and_44100_to_about_16000_frames() {
        for rate in [48_000, 44_100] {
            let input = vec![0.25; rate as usize];
            let bytes = f32_bytes(&input);
            let flac = combine_to_flac(Some((&bytes, f32_format(rate, 1))), None)
                .unwrap()
                .unwrap();

            let (_, _, _, samples) = decode(&flac);
            let frames = samples.len() / 2;
            assert!(
                (15_900..=16_100).contains(&frames),
                "{rate} Hz produced {frames} frames"
            );
        }
    }

    #[test]
    fn ac4_sinc_resampler_filters_above_target_nyquist() {
        let rate = 48_000;
        let tone = 12_000.0;
        let input: Vec<f32> = (0..rate)
            .map(|n| (TAU * tone * n as f32 / rate as f32).sin())
            .collect();
        let bytes = f32_bytes(&input);
        let flac = combine_to_flac(Some((&bytes, f32_format(rate, 1))), None)
            .unwrap()
            .unwrap();

        let (_, _, _, samples) = decode(&flac);
        let left = left(&samples);
        let rms = (left
            .iter()
            .map(|sample| (*sample as f32) * (*sample as f32))
            .sum::<f32>()
            / left.len() as f32)
            .sqrt();
        assert!(rms < 2500.0, "rms was {rms}");
    }

    #[test]
    fn ac5_downmixes_multichannel_before_resampling() {
        let pcm = i16_bytes(&[10_000, -10_000].repeat(16));
        let flac = combine_to_flac(Some((&pcm, i16_format(16_000, 2))), None)
            .unwrap()
            .unwrap();

        let (_, _, _, samples) = decode(&flac);

        assert!(left(&samples).iter().all(|sample| sample.abs() <= 1));
    }

    #[test]
    fn ac6_rejects_unsupported_integer_formats() {
        for bits in [24, 32] {
            let err = combine_to_flac(
                Some((
                    &[0; 16],
                    AudioFormat {
                        sample_rate_hz: 16_000,
                        channels: 1,
                        bits_per_sample: bits,
                        is_float: false,
                    },
                )),
                None,
            )
            .unwrap_err();

            assert!(matches!(err, AudioError::UnsupportedFormat { bits: b, .. } if b == bits));
        }
    }

    #[test]
    fn ac7_pads_shorter_or_absent_source_with_zeroes() {
        let mic = i16_bytes(&[1000; 20]);
        let sys = i16_bytes(&[-1000; 16]);
        let flac = combine_to_flac(
            Some((&mic, i16_format(16_000, 1))),
            Some((&sys, i16_format(16_000, 1))),
        )
        .unwrap()
        .unwrap();
        let (_, _, _, samples) = decode(&flac);

        assert_eq!(samples.len(), 40);
        assert_eq!(&right(&samples)[16..], &[0, 0, 0, 0]);

        let flac = combine_to_flac(Some((&mic, i16_format(16_000, 1))), None)
            .unwrap()
            .unwrap();
        let (_, _, _, samples) = decode(&flac);
        assert!(right(&samples).iter().all(|sample| *sample == 0));
        assert!(left(&samples).iter().any(|sample| *sample != 0));
    }

    #[test]
    fn ac8_drops_partial_frame_and_clamps_float_samples() {
        let mut partial = i16_bytes(&[1000; 16]);
        partial.push(0x7f);
        let flac = combine_to_flac(Some((&partial, i16_format(16_000, 1))), None)
            .unwrap()
            .unwrap();
        let (_, _, _, samples) = decode(&flac);
        assert_eq!(samples.len(), 32);

        let mut float_samples = vec![0.0; 16];
        float_samples[0] = 2.0;
        float_samples[1] = -2.0;
        let floats = f32_bytes(&float_samples);
        let flac = combine_to_flac(Some((&floats, f32_format(16_000, 1))), None)
            .unwrap()
            .unwrap();
        let (_, _, _, samples) = decode(&flac);
        assert_eq!(&left(&samples)[..2], &[32_767, -32_767]);
    }

    #[test]
    fn ac12_no_sources_returns_none() {
        assert_eq!(combine_to_flac(None, None).unwrap(), None);
    }

    #[test]
    fn deterministic_for_identical_inputs() {
        let mic = i16_bytes(&[1, 2, 3, 4, 5, 6, 7, 8].repeat(2));
        let first = combine_to_flac(Some((&mic, i16_format(16_000, 1))), None)
            .unwrap()
            .unwrap();
        let second = combine_to_flac(Some((&mic, i16_format(16_000, 1))), None)
            .unwrap()
            .unwrap();

        assert_eq!(first, second);
    }
}
