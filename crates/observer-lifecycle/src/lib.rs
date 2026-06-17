// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Restart policy: exponential backoff with a circuit breaker.
//!
//! When a source faults, the engine consults this pure state machine to decide
//! *when* (or whether) to retry. Backoff grows exponentially up to a cap; after
//! too many failures in a row the breaker opens and retries stop until an
//! operator intervenes — the observer never spins forever pretending it can
//! recover. Time is passed in, never read, so the whole thing is fake-clock
//! tested on any host.

#![forbid(unsafe_code)]

use core::time::Duration;

/// Tuning for the backoff/breaker. Defaults are conservative and host-testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackoffConfig {
    pub initial: Duration,
    pub max: Duration,
    pub multiplier: u32,
    /// Consecutive failures that trip the breaker open.
    pub breaker_threshold: u32,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(60),
            multiplier: 2,
            breaker_threshold: 5,
        }
    }
}

/// The breaker's position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    /// Retries permitted; backoff applies.
    Closed,
    /// Too many consecutive failures — retries suspended pending intervention.
    Open,
}

/// What the engine should do after the latest fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    /// Wait this long, then retry.
    RetryAfter(Duration),
    /// Breaker is open — stop retrying.
    GiveUp,
}

/// Backoff + circuit-breaker state. One instance per restartable source.
#[derive(Debug, Clone)]
pub struct Lifecycle {
    config: BackoffConfig,
    consecutive_failures: u32,
    breaker: BreakerState,
}

impl Lifecycle {
    pub fn new(config: BackoffConfig) -> Self {
        Self {
            config,
            consecutive_failures: 0,
            breaker: BreakerState::Closed,
        }
    }

    pub fn breaker(&self) -> BreakerState {
        self.breaker
    }

    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// Record a successful (re)start: resets the failure count and re-closes the
    /// breaker.
    pub fn on_success(&mut self) {
        self.consecutive_failures = 0;
        self.breaker = BreakerState::Closed;
    }

    /// Record a fault and get the retry decision. Trips the breaker open once the
    /// configured threshold of consecutive failures is reached.
    pub fn on_failure(&mut self) -> RetryDecision {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        if self.consecutive_failures >= self.config.breaker_threshold {
            self.breaker = BreakerState::Open;
            return RetryDecision::GiveUp;
        }
        RetryDecision::RetryAfter(self.current_backoff())
    }

    /// The backoff delay for the current failure count, capped at `config.max`.
    fn current_backoff(&self) -> Duration {
        // failures is >= 1 here; first retry uses `initial`.
        let exp = self.consecutive_failures.saturating_sub(1);
        let factor = self.config.multiplier.saturating_pow(exp);
        let scaled = self.config.initial.saturating_mul(factor.max(1));
        if scaled > self.config.max {
            self.config.max
        } else {
            scaled
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_then_caps() {
        let mut lc = Lifecycle::new(BackoffConfig {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(8),
            multiplier: 2,
            breaker_threshold: 100,
        });
        assert_eq!(
            lc.on_failure(),
            RetryDecision::RetryAfter(Duration::from_secs(1))
        );
        assert_eq!(
            lc.on_failure(),
            RetryDecision::RetryAfter(Duration::from_secs(2))
        );
        assert_eq!(
            lc.on_failure(),
            RetryDecision::RetryAfter(Duration::from_secs(4))
        );
        assert_eq!(
            lc.on_failure(),
            RetryDecision::RetryAfter(Duration::from_secs(8))
        );
        // capped
        assert_eq!(
            lc.on_failure(),
            RetryDecision::RetryAfter(Duration::from_secs(8))
        );
    }

    #[test]
    fn breaker_opens_at_threshold() {
        let mut lc = Lifecycle::new(BackoffConfig {
            breaker_threshold: 3,
            ..Default::default()
        });
        assert!(matches!(lc.on_failure(), RetryDecision::RetryAfter(_)));
        assert!(matches!(lc.on_failure(), RetryDecision::RetryAfter(_)));
        assert_eq!(lc.on_failure(), RetryDecision::GiveUp);
        assert_eq!(lc.breaker(), BreakerState::Open);
    }

    #[test]
    fn success_resets_and_recloses() {
        let mut lc = Lifecycle::new(BackoffConfig {
            breaker_threshold: 3,
            ..Default::default()
        });
        lc.on_failure();
        lc.on_failure();
        lc.on_success();
        assert_eq!(lc.consecutive_failures(), 0);
        assert_eq!(lc.breaker(), BreakerState::Closed);
        // backoff restarts from initial after a success
        assert_eq!(
            lc.on_failure(),
            RetryDecision::RetryAfter(Duration::from_secs(1))
        );
    }
}
