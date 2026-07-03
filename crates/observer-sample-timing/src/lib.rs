// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Pure video sample timing.

#![forbid(unsafe_code)]

/// Minimum Media Foundation sample duration: 1 ms in 100 ns ticks.
pub const MIN_SAMPLE_DURATION_100NS: i64 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StampedSample {
    pub sample_time_100ns: i64,
    pub sample_duration_100ns: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SampleTimer {
    anchor: i64,
    min_duration: i64,
    pending: Option<i64>,
    last_gap: i64,
    clamp_events: u64,
    last_end: i64,
}

impl SampleTimer {
    pub fn new(anchor_100ns: i64, min_duration_100ns: i64) -> Self {
        Self {
            anchor: anchor_100ns,
            min_duration: min_duration_100ns,
            pending: None,
            last_gap: min_duration_100ns,
            clamp_events: 0,
            last_end: 0,
        }
    }

    pub fn push(&mut self, arrival_100ns: i64) -> Option<StampedSample> {
        let raw = arrival_100ns - self.anchor;
        let mut time = raw.max(0);
        let mut clamped = raw < 0;

        if let Some(prev) = self.pending {
            let min_time = prev + self.min_duration;
            if time < min_time {
                time = min_time;
                clamped = true;
            }
        }

        if clamped {
            self.clamp_events = self.clamp_events.saturating_add(1);
        }

        let emitted = self.pending.map(|prev| {
            let duration = time - prev;
            self.last_gap = duration;
            StampedSample {
                sample_time_100ns: prev,
                sample_duration_100ns: duration,
            }
        });
        self.pending = Some(time);
        emitted
    }

    pub fn flush(&mut self, window_end_100ns: i64) -> Option<StampedSample> {
        let time = self.pending.take()?;
        let ceiling = (window_end_100ns - time).max(self.min_duration);
        let duration = self.last_gap.min(ceiling).max(self.min_duration);
        self.last_end = time + duration;
        Some(StampedSample {
            sample_time_100ns: time,
            sample_duration_100ns: duration,
        })
    }

    pub fn clamp_events(&self) -> u64 {
        self.clamp_events
    }

    pub fn last_end_100ns(&self) -> i64 {
        self.last_end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEC: i64 = 10_000_000;
    const PERIOD: i64 = 300 * SEC;

    #[test]
    fn real_gaps_become_sample_durations() {
        let mut timer = SampleTimer::new(1_000, MIN_SAMPLE_DURATION_100NS);

        assert_eq!(timer.push(1_000), None);
        assert_eq!(
            timer.push(21_000),
            Some(StampedSample {
                sample_time_100ns: 0,
                sample_duration_100ns: 20_000,
            })
        );
        assert_eq!(
            timer.push(36_000),
            Some(StampedSample {
                sample_time_100ns: 20_000,
                sample_duration_100ns: 15_000,
            })
        );

        assert_eq!(timer.clamp_events(), 0);
    }

    #[test]
    fn equal_arrivals_are_spaced_by_minimum_duration() {
        let mut timer = SampleTimer::new(1_000, MIN_SAMPLE_DURATION_100NS);

        assert_eq!(timer.push(1_000), None);
        assert_eq!(
            timer.push(1_000),
            Some(StampedSample {
                sample_time_100ns: 0,
                sample_duration_100ns: MIN_SAMPLE_DURATION_100NS,
            })
        );

        assert_eq!(timer.clamp_events(), 1);
    }

    #[test]
    fn regressing_arrival_is_clamped_and_counted_once() {
        let mut timer = SampleTimer::new(1_000, MIN_SAMPLE_DURATION_100NS);

        assert_eq!(timer.push(2_000), None);
        assert_eq!(
            timer.push(1_500),
            Some(StampedSample {
                sample_time_100ns: 1_000,
                sample_duration_100ns: MIN_SAMPLE_DURATION_100NS,
            })
        );

        assert_eq!(timer.clamp_events(), 1);
    }

    #[test]
    fn first_arrival_before_anchor_clamps_pending_time_to_zero() {
        let mut timer = SampleTimer::new(1_000, MIN_SAMPLE_DURATION_100NS);

        assert_eq!(timer.push(500), None);
        assert_eq!(timer.clamp_events(), 1);
        assert_eq!(
            timer.flush(PERIOD),
            Some(StampedSample {
                sample_time_100ns: 0,
                sample_duration_100ns: MIN_SAMPLE_DURATION_100NS,
            })
        );
    }

    #[test]
    fn flush_bounds_last_sample_to_full_segment_window() {
        let mut timer = SampleTimer::new(0, MIN_SAMPLE_DURATION_100NS);

        assert_eq!(timer.push(0), None);
        assert_eq!(
            timer.push(PERIOD - 1_000_000),
            Some(StampedSample {
                sample_time_100ns: 0,
                sample_duration_100ns: PERIOD - 1_000_000,
            })
        );
        assert_eq!(
            timer.flush(PERIOD),
            Some(StampedSample {
                sample_time_100ns: PERIOD - 1_000_000,
                sample_duration_100ns: 1_000_000,
            })
        );
        assert_eq!(timer.last_end_100ns(), PERIOD);
    }

    #[test]
    fn flush_keeps_partial_segment_near_last_frame_plus_gap() {
        let mut timer = SampleTimer::new(0, MIN_SAMPLE_DURATION_100NS);

        assert_eq!(timer.push(41 * SEC), None);
        assert_eq!(
            timer.push(42 * SEC),
            Some(StampedSample {
                sample_time_100ns: 41 * SEC,
                sample_duration_100ns: SEC,
            })
        );
        assert_eq!(
            timer.flush(PERIOD),
            Some(StampedSample {
                sample_time_100ns: 42 * SEC,
                sample_duration_100ns: SEC,
            })
        );
        assert_eq!(timer.last_end_100ns(), 43 * SEC);
    }

    #[test]
    fn single_frame_flush_uses_min_duration() {
        let mut timer = SampleTimer::new(25 * SEC, MIN_SAMPLE_DURATION_100NS);

        assert_eq!(timer.push(25 * SEC), None);
        assert_eq!(
            timer.flush(PERIOD),
            Some(StampedSample {
                sample_time_100ns: 0,
                sample_duration_100ns: MIN_SAMPLE_DURATION_100NS,
            })
        );
        assert_eq!(timer.last_end_100ns(), MIN_SAMPLE_DURATION_100NS);
    }
}
