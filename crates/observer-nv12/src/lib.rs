// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Pure RGBA8/BGRA8 to NV12 conversion.

#![forbid(unsafe_code)]

use observer_model::{ScreenFrame, ScreenPixelFormat};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nv12Frame {
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Nv12Error {
    #[error("NV12 requires nonzero even dimensions, got {width}x{height}")]
    InvalidDimensions { width: u32, height: u32 },
    #[error("screen frame buffer length mismatch: expected {expected} bytes, got {actual}")]
    BufferLengthMismatch { expected: usize, actual: usize },
}

pub fn rgba_or_bgra_to_nv12(frame: &ScreenFrame) -> Result<Nv12Frame, Nv12Error> {
    let width = frame.width;
    let height = frame.height;
    if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
        return Err(Nv12Error::InvalidDimensions { width, height });
    }

    let pixel_count = (width as usize)
        .checked_mul(height as usize)
        .ok_or(Nv12Error::InvalidDimensions { width, height })?;
    let expected = pixel_count
        .checked_mul(4)
        .ok_or(Nv12Error::InvalidDimensions { width, height })?;
    if frame.pixels.len() != expected {
        return Err(Nv12Error::BufferLengthMismatch {
            expected,
            actual: frame.pixels.len(),
        });
    }

    let y_len = pixel_count;
    let uv_len = pixel_count / 2;
    let mut bytes = vec![0; y_len + uv_len];
    let width_usize = width as usize;
    let height_usize = height as usize;

    for y in 0..height_usize {
        for x in 0..width_usize {
            let (r, g, b) = rgb_at(frame, width_usize, x, y);
            bytes[y * width_usize + x] = luma(r, g, b);
        }
    }

    for y in (0..height_usize).step_by(2) {
        for x in (0..width_usize).step_by(2) {
            let mut r_sum = 0u16;
            let mut g_sum = 0u16;
            let mut b_sum = 0u16;
            for dy in 0..2 {
                for dx in 0..2 {
                    let (r, g, b) = rgb_at(frame, width_usize, x + dx, y + dy);
                    r_sum += r as u16;
                    g_sum += g as u16;
                    b_sum += b as u16;
                }
            }
            let r = (r_sum as f32) / 4.0;
            let g = (g_sum as f32) / 4.0;
            let b = (b_sum as f32) / 4.0;
            let uv_index = y_len + (y / 2) * width_usize + x;
            bytes[uv_index] = chroma_u(r, g, b);
            bytes[uv_index + 1] = chroma_v(r, g, b);
        }
    }

    Ok(Nv12Frame {
        width,
        height,
        bytes,
    })
}

fn rgb_at(frame: &ScreenFrame, width: usize, x: usize, y: usize) -> (u8, u8, u8) {
    let i = (y * width + x) * 4;
    match frame.pixel_format {
        ScreenPixelFormat::Rgba8 => (frame.pixels[i], frame.pixels[i + 1], frame.pixels[i + 2]),
        ScreenPixelFormat::Bgra8 => (frame.pixels[i + 2], frame.pixels[i + 1], frame.pixels[i]),
    }
}

fn luma(r: u8, g: u8, b: u8) -> u8 {
    clamp_round(
        16.0 + 0.257 * r as f32 + 0.504 * g as f32 + 0.098 * b as f32,
        16,
        235,
    )
}

fn chroma_u(r: f32, g: f32, b: f32) -> u8 {
    clamp_round(128.0 - 0.148 * r - 0.291 * g + 0.439 * b, 16, 240)
}

fn chroma_v(r: f32, g: f32, b: f32) -> u8 {
    clamp_round(128.0 + 0.439 * r - 0.368 * g - 0.071 * b, 16, 240)
}

fn clamp_round(value: f32, min: u8, max: u8) -> u8 {
    (value.round() as i32).clamp(min as i32, max as i32) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use observer_model::normalize_even;
    use std::sync::Arc;

    fn frame(
        width: u32,
        height: u32,
        pixel_format: ScreenPixelFormat,
        pixels: Vec<u8>,
    ) -> ScreenFrame {
        ScreenFrame {
            seq: 0,
            arrival_100ns: 0,
            width,
            height,
            pixel_format,
            pixels: Arc::from(pixels),
        }
    }

    fn solid_rgba(r: u8, g: u8, b: u8) -> ScreenFrame {
        let mut pixels = Vec::new();
        for _ in 0..4 {
            pixels.extend_from_slice(&[r, g, b, 255]);
        }
        frame(2, 2, ScreenPixelFormat::Rgba8, pixels)
    }

    fn rgba_gradient(width: u32, height: u32) -> ScreenFrame {
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height as usize {
            for x in 0..width as usize {
                pixels.extend_from_slice(&[
                    x as u8,
                    y as u8,
                    (x as u8).wrapping_mul(31).wrapping_add(y as u8),
                    255,
                ]);
            }
        }
        frame(width, height, ScreenPixelFormat::Rgba8, pixels)
    }

    #[test]
    fn normalized_odd_height_rgba_converts_to_nv12() {
        let input = rgba_gradient(4, 3);
        let normalized = normalize_even(&input);

        assert!(rgba_or_bgra_to_nv12(&normalized).is_ok());
    }

    #[test]
    fn normalized_odd_width_rgba_converts_to_nv12() {
        let input = rgba_gradient(3, 4);
        let normalized = normalize_even(&input);

        assert!(rgba_or_bgra_to_nv12(&normalized).is_ok());
    }

    #[test]
    fn solid_colors_match_bt601_limited_range() {
        let cases = [
            (solid_rgba(0, 0, 0), 16, 128, 128),
            (solid_rgba(255, 255, 255), 235, 128, 128),
            (solid_rgba(255, 0, 0), 82, 90, 240),
            (solid_rgba(0, 255, 0), 145, 54, 34),
            (solid_rgba(0, 0, 255), 41, 240, 110),
        ];

        for (input, expected_y, expected_u, expected_v) in cases {
            let out = rgba_or_bgra_to_nv12(&input).unwrap();
            assert_eq!(&out.bytes[..4], &[expected_y; 4]);
            assert_eq!(out.bytes[4], expected_u);
            assert_eq!(out.bytes[5], expected_v);
        }
    }

    #[test]
    fn checker_gradient_sets_expected_luma_and_chroma() {
        let input = frame(
            2,
            2,
            ScreenPixelFormat::Rgba8,
            vec![
                0, 0, 0, 255, // black
                255, 255, 255, 255, // white
                255, 0, 0, 255, // red
                0, 0, 255, 255, // blue
            ],
        );
        let out = rgba_or_bgra_to_nv12(&input).unwrap();

        assert_eq!(&out.bytes[..4], &[16, 235, 82, 41]);
        assert!((out.bytes[4] as i16 - 147).abs() <= 1);
        assert!((out.bytes[5] as i16 - 151).abs() <= 1);
    }

    #[test]
    fn rgba_and_bgra_byte_order_guard_changes_uv() {
        let rgba_red = solid_rgba(255, 0, 0);
        let bgra_blue = frame(2, 2, ScreenPixelFormat::Bgra8, [255, 0, 0, 255].repeat(4));

        let rgba = rgba_or_bgra_to_nv12(&rgba_red).unwrap();
        let bgra = rgba_or_bgra_to_nv12(&bgra_blue).unwrap();

        assert_eq!(&rgba.bytes[4..6], &[90, 240]);
        assert_eq!(&bgra.bytes[4..6], &[240, 110]);
    }

    #[test]
    fn uv_interleave_order_guard_rejects_swapped_planes() {
        let out = rgba_or_bgra_to_nv12(&solid_rgba(255, 0, 0)).unwrap();

        assert_eq!(out.bytes[4], 90);
        assert_eq!(out.bytes[5], 240);
        assert_ne!(&out.bytes[4..6], &[240, 90]);
    }
}
