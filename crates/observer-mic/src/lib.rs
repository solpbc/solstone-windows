// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Microphone control model — pure tier.
//!
//! Owns the owner-configured mic policy ([`MicConfig`], persisted to `mic.json`
//! and edited over IPC): a device **priority** order, a per-device **disable**
//! set, and an input **gain**. It also owns the two pieces of logic worth testing
//! without a sound card:
//!
//! 1. [`MicConfig::select`] — given the live input devices the platform tier
//!    enumerated, pick which one to open: the highest-priority enabled device that
//!    is actually present, falling back to the first enabled device, or `None`
//!    when every device is disabled / none exist.
//! 2. [`apply_gain_f32`] / [`apply_gain_i16`] — multiply captured samples by the
//!    gain, clamped so amplification never wraps or overflows.
//!
//! The platform tier (`capture-wasapi`) enumerates the real WASAPI eCapture
//! endpoints, opens the device this policy selects, and applies the gain to each
//! captured buffer. This crate has no platform dependency and is host-testable.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// The discrete gain factors the owner can pick (matching the macOS observer).
pub const GAIN_LEVELS: [u8; 4] = [1, 2, 4, 8];

/// One input device the platform tier enumerated, offered to the owner and used
/// by [`MicConfig::select`]. `id` is the stable WASAPI endpoint id (the value
/// stored in the priority/disabled lists); `name` is the friendly label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MicDeviceRef {
    pub id: String,
    pub name: String,
}

/// Owner-configured microphone policy. Persisted to `mic.json` and edited over
/// IPC. Default: no priority, nothing disabled, unity gain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MicConfig {
    /// Preferred input device ids, most-preferred first. Devices not listed here
    /// rank below all listed ones (in enumeration order).
    #[serde(default)]
    pub priority: Vec<String>,
    /// Device ids the owner has disabled — never selected, even if present.
    #[serde(default)]
    pub disabled: Vec<String>,
    /// Input gain factor: one of [`GAIN_LEVELS`] (1× / 2× / 4× / 8×).
    #[serde(default = "default_gain")]
    pub gain: u8,
}

fn default_gain() -> u8 {
    1
}

impl Default for MicConfig {
    fn default() -> Self {
        Self {
            priority: Vec::new(),
            disabled: Vec::new(),
            gain: 1,
        }
    }
}

impl MicConfig {
    /// Canonical form: gain snapped to the nearest valid level, lists de-duplicated
    /// (order-stable). Applied when the owner sets config so the stored form is the
    /// effective form.
    pub fn normalized(&self) -> Self {
        Self {
            priority: dedupe(&self.priority),
            disabled: dedupe(&self.disabled),
            gain: snap_gain(self.gain),
        }
    }

    /// The gain as a float multiplier for the DSP helpers.
    pub fn gain_multiplier(&self) -> f32 {
        snap_gain(self.gain) as f32
    }

    /// Whether `id` is disabled.
    pub fn is_disabled(&self, id: &str) -> bool {
        self.disabled.iter().any(|d| d == id)
    }

    /// Pick the device to open from the live device list: the highest-priority
    /// enabled device that is present, else the first enabled device (enumeration
    /// order), else `None` (every device disabled, or none present).
    pub fn select<'a>(&self, available: &'a [MicDeviceRef]) -> Option<&'a MicDeviceRef> {
        let enabled = |d: &&MicDeviceRef| !self.is_disabled(&d.id);
        for id in &self.priority {
            if let Some(dev) = available.iter().find(|d| &d.id == id).filter(enabled) {
                return Some(dev);
            }
        }
        available.iter().find(enabled)
    }
}

/// What Settings renders: the owner's config plus the id of the device the
/// capture loop has *actually* opened (so "active" is earned, not guessed). The
/// live device list is fetched separately (it is enumerated on demand).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MicView {
    pub config: MicConfig,
    pub active_id: Option<String>,
}

/// Snap an arbitrary factor to the nearest valid [`GAIN_LEVELS`] entry.
pub fn snap_gain(gain: u8) -> u8 {
    GAIN_LEVELS
        .iter()
        .copied()
        .min_by_key(|level| (*level as i32 - gain as i32).abs())
        .unwrap_or(1)
}

fn dedupe(values: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in values {
        if !v.is_empty() && !out.contains(v) {
            out.push(v.clone());
        }
    }
    out
}

/// Multiply 32-bit float samples in place by `mult`, clamped to `[-1.0, 1.0]` so
/// amplification never clips past full scale. A `mult` of `1.0` is left exact.
pub fn apply_gain_f32(samples: &mut [f32], mult: f32) {
    if mult == 1.0 {
        return;
    }
    for s in samples {
        *s = (*s * mult).clamp(-1.0, 1.0);
    }
}

/// Multiply 16-bit PCM samples in place by `mult`, clamped to the `i16` range so
/// amplification saturates instead of wrapping. A `mult` of `1.0` is a no-op.
pub fn apply_gain_i16(samples: &mut [i16], mult: f32) {
    if mult == 1.0 {
        return;
    }
    for s in samples {
        let scaled = (*s as f32 * mult).clamp(i16::MIN as f32, i16::MAX as f32);
        *s = scaled as i16;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(id: &str, name: &str) -> MicDeviceRef {
        MicDeviceRef {
            id: id.to_string(),
            name: name.to_string(),
        }
    }

    #[test]
    fn default_is_empty_unity_gain() {
        let c = MicConfig::default();
        assert!(c.priority.is_empty());
        assert!(c.disabled.is_empty());
        assert_eq!(c.gain, 1);
        assert_eq!(c.gain_multiplier(), 1.0);
    }

    #[test]
    fn select_prefers_highest_priority_present_enabled() {
        let devs = [dev("a", "Mic A"), dev("b", "Mic B"), dev("c", "Mic C")];
        // priority [c, b] -> c wins (present + enabled)
        let cfg = MicConfig {
            priority: vec!["c".into(), "b".into()],
            ..Default::default()
        };
        assert_eq!(cfg.select(&devs).unwrap().id, "c");

        // c disabled -> next priority b
        let cfg = MicConfig {
            priority: vec!["c".into(), "b".into()],
            disabled: vec!["c".into()],
            ..Default::default()
        };
        assert_eq!(cfg.select(&devs).unwrap().id, "b");

        // priority device absent -> fall back to first enabled in enumeration order
        let cfg = MicConfig {
            priority: vec!["z".into()],
            ..Default::default()
        };
        assert_eq!(cfg.select(&devs).unwrap().id, "a");

        // first enumerated disabled -> first *enabled*
        let cfg = MicConfig {
            disabled: vec!["a".into()],
            ..Default::default()
        };
        assert_eq!(cfg.select(&devs).unwrap().id, "b");
    }

    #[test]
    fn select_none_when_all_disabled_or_empty() {
        let devs = [dev("a", "Mic A")];
        let cfg = MicConfig {
            disabled: vec!["a".into()],
            ..Default::default()
        };
        assert!(cfg.select(&devs).is_none());
        assert!(MicConfig::default().select(&[]).is_none());
    }

    #[test]
    fn gain_snaps_to_valid_levels() {
        assert_eq!(snap_gain(1), 1);
        assert_eq!(snap_gain(2), 2);
        assert_eq!(snap_gain(3), 2); // ties/near -> nearest (3 is equidistant 2/4 -> first min = 2)
        assert_eq!(snap_gain(4), 4);
        assert_eq!(snap_gain(7), 8);
        assert_eq!(snap_gain(100), 8);
        assert_eq!(snap_gain(0), 1);
    }

    #[test]
    fn normalize_dedupes_and_snaps() {
        let c = MicConfig {
            priority: vec!["a".into(), "a".into(), "b".into(), "".into()],
            disabled: vec!["c".into(), "c".into()],
            gain: 5,
        };
        let n = c.normalized();
        assert_eq!(n.priority, vec!["a", "b"]);
        assert_eq!(n.disabled, vec!["c"]);
        assert_eq!(n.gain, 4);
    }

    #[test]
    fn gain_f32_clamps_and_unity_is_noop() {
        let mut s = vec![0.5f32, -0.5, 0.9, -0.9];
        apply_gain_f32(&mut s, 1.0);
        assert_eq!(s, vec![0.5, -0.5, 0.9, -0.9]); // unity untouched

        let mut s = vec![0.5f32, -0.5, 0.9, -0.9];
        apply_gain_f32(&mut s, 2.0);
        assert_eq!(s, vec![1.0, -1.0, 1.0, -1.0]); // clamped at full scale
    }

    #[test]
    fn gain_i16_saturates() {
        let mut s = vec![10_000i16, -10_000, 100];
        apply_gain_i16(&mut s, 4.0);
        assert_eq!(s, vec![i16::MAX, i16::MIN, 400]); // saturates, no wrap
    }

    #[test]
    fn config_round_trips_and_defaults() {
        let c = MicConfig {
            priority: vec!["a".into()],
            disabled: vec!["b".into()],
            gain: 4,
        };
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<MicConfig>(&json).unwrap(), c);

        let partial: MicConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(partial, MicConfig::default());
    }
}
