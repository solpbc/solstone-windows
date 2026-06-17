// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Clock-boundary segment rotation math.
//!
//! Segments are aligned to fixed wall-clock boundaries (default: every
//! [`DEFAULT_SEGMENT_SECS`] seconds, i.e. 5 minutes on the clock — `:00`, `:05`,
//! `:10`, …), **not** to the moment capture started. Aligning to the clock keeps
//! segments comparable across sources and restarts.
//!
//! This crate is pure decision logic. All filesystem effects go through the
//! [`SegmentFs`] trait so the rotation boundary function is unit/property-tested
//! against a synthetic clock and a fake fs, on any host.

#![forbid(unsafe_code)]

use observer_model::SegmentKey;

/// Default rotation period: five minutes, aligned to the wall clock.
pub const DEFAULT_SEGMENT_SECS: u64 = 5 * 60;

/// The aligned [`SegmentKey`] that a given instant falls into.
///
/// `index` is derived deterministically from the boundary, so the same instant
/// always maps to the same key regardless of when capture started.
///
/// # Panics
/// Never. `period_secs` of 0 is treated as 1 to avoid division by zero.
pub fn segment_for(now_epoch_secs: u64, period_secs: u64) -> SegmentKey {
    let period = period_secs.max(1);
    let boundary = (now_epoch_secs / period) * period;
    SegmentKey {
        boundary_epoch_secs: boundary,
        index: boundary / period,
    }
}

/// Seconds remaining until the next rotation boundary after `now`.
pub fn seconds_until_next_boundary(now_epoch_secs: u64, period_secs: u64) -> u64 {
    let period = period_secs.max(1);
    period - (now_epoch_secs % period)
}

/// True when `now` has crossed out of `current`'s aligned window — i.e. it is
/// time to seal `current` and open the next segment. This is the single
/// rotation-boundary decision the engine consults each tick.
pub fn should_rotate(current: SegmentKey, now_epoch_secs: u64, period_secs: u64) -> bool {
    segment_for(now_epoch_secs, period_secs) != current
}

/// Filesystem seam for segment lifecycle. Real impl lives in `platform-win`
/// (`%LocalAppData%`); tests inject a fake. Open creates an `.incomplete`
/// segment dir; finalize atomically renames it to its sealed name.
pub trait SegmentFs {
    type Error: core::fmt::Debug;

    /// Create and open the `.incomplete` directory for `key`; return its path.
    fn open_incomplete(&mut self, key: SegmentKey) -> Result<String, Self::Error>;

    /// Atomically seal the `.incomplete` segment for `key` to its final name.
    fn finalize(&mut self, key: SegmentKey) -> Result<(), Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_is_clock_aligned_not_start_aligned() {
        // 12:03:30 -> the :00..:05 window (boundary at 12:00:00 of that period).
        let now = 3 * 60 + 30; // 210s past an aligned :00
        let k = segment_for(now, DEFAULT_SEGMENT_SECS);
        assert_eq!(k.boundary_epoch_secs, 0);
        // 12:06:00 lands in the next window.
        let k2 = segment_for(6 * 60, DEFAULT_SEGMENT_SECS);
        assert_eq!(k2.boundary_epoch_secs, 5 * 60);
        assert_eq!(k2.index, 1);
    }

    #[test]
    fn remaining_counts_down_to_boundary() {
        assert_eq!(seconds_until_next_boundary(0, DEFAULT_SEGMENT_SECS), 300);
        assert_eq!(seconds_until_next_boundary(299, DEFAULT_SEGMENT_SECS), 1);
        assert_eq!(seconds_until_next_boundary(300, DEFAULT_SEGMENT_SECS), 300);
    }

    #[test]
    fn rotates_exactly_at_boundary_crossing() {
        let cur = segment_for(10, DEFAULT_SEGMENT_SECS);
        assert!(!should_rotate(cur, 299, DEFAULT_SEGMENT_SECS));
        assert!(should_rotate(cur, 300, DEFAULT_SEGMENT_SECS));
    }

    #[test]
    fn property_index_monotonic_across_boundaries() {
        // Walking forward across many periods, the index never decreases and
        // increments by exactly one per period crossed.
        let mut last = segment_for(0, DEFAULT_SEGMENT_SECS);
        for step in 1..50u64 {
            let k = segment_for(step * DEFAULT_SEGMENT_SECS, DEFAULT_SEGMENT_SECS);
            assert_eq!(k.index, last.index + 1);
            last = k;
        }
    }
}
