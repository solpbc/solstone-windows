// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! One operation-scoped observation seam for the production transport.
//!
//! An operator mode attaches a single [`OperationObserver`] to one operation and
//! reads it afterwards. There is deliberately **one** handle rather than a dial
//! counter plus a separate byte-progress channel: a per-operation total that
//! silently omits a class of dials would be exactly the unearned state this
//! codebase forbids.
//!
//! It is threaded explicitly as [`ObserverHandle`] (an `Option`, defaulting to
//! `None`) through [`ObserverClient`](crate::client::ObserverClient), the pairing
//! ceremony, and [`journal_bridge::start_observed`](crate::journal_bridge::start_observed)
//! — the last of which is what makes the bridge's silent `MuxCarrier` redials
//! visible.
//!
//! **Observation only.** Every method is a relaxed `fetch_add`/`fetch_max`/`store`
//! at a seam the transport already passes through. Nothing here awaits, branches
//! on an observed value, or feeds a loop bound, so retry counts, backoff, and
//! ordering are identical whether or not anyone is watching. `observation_is_inert`
//! in `tests/integration_mode.rs` proves it by running the same scenario with
//! `Some(handle)` and with `None` and comparing the emitted dial sequence.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use observer_model::TransportPath;

/// The optional, explicitly-threaded observation handle. `None` in the GUI.
pub type ObserverHandle = Option<Arc<OperationObserver>>;

/// Write-only counters for one operation.
#[derive(Debug, Default)]
pub struct OperationObserver {
    dial_attempts: AtomicU64,
    direct_successes: AtomicU64,
    relay_successes: AtomicU64,
    request_bytes_sent: AtomicU64,
    close_completed: AtomicBool,
}

/// A consistent-enough read of an [`OperationObserver`] for reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DialCounts {
    /// Every physical dial/reconnect begun, including failed and replaced legs.
    pub dial_attempts: u64,
    pub direct_successes: u64,
    pub relay_successes: u64,
    /// Furthest request progress handed to the wire, in bytes.
    pub request_bytes_sent: u64,
    /// Whether a request's `CLOSE` was emitted *and* the peer's terminal
    /// response was observed.
    pub close_completed: bool,
}

impl OperationObserver {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// One physical dial/reconnect attempt is about to be made.
    pub fn record_dial_attempt(&self) {
        self.dial_attempts.fetch_add(1, Ordering::Relaxed);
    }

    /// A dial completed over `path`.
    pub fn record_dial_success(&self, path: TransportPath) {
        let counter = match path {
            TransportPath::Direct => &self.direct_successes,
            TransportPath::Relay => &self.relay_successes,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Request bytes handed to the wire so far. Retained as a maximum, so a
    /// retried leg reports furthest progress rather than a double count.
    pub fn record_request_bytes(&self, bytes: u64) {
        self.request_bytes_sent.fetch_max(bytes, Ordering::Relaxed);
    }

    /// The request's `CLOSE` was emitted and the peer's terminal response was
    /// observed. Emitting `CLOSE` alone is deliberately **not** enough.
    pub fn record_close_completed(&self) {
        self.close_completed.store(true, Ordering::Relaxed);
    }

    pub fn counts(&self) -> DialCounts {
        DialCounts {
            dial_attempts: self.dial_attempts.load(Ordering::Relaxed),
            direct_successes: self.direct_successes.load(Ordering::Relaxed),
            relay_successes: self.relay_successes.load(Ordering::Relaxed),
            request_bytes_sent: self.request_bytes_sent.load(Ordering::Relaxed),
            close_completed: self.close_completed.load(Ordering::Relaxed),
        }
    }
}

pub(crate) fn note_dial_attempt(observer: &ObserverHandle) {
    if let Some(observer) = observer {
        observer.record_dial_attempt();
    }
}

pub(crate) fn note_dial_success(observer: &ObserverHandle, path: TransportPath) {
    if let Some(observer) = observer {
        observer.record_dial_success(path);
    }
}

pub(crate) fn note_request_bytes(observer: &ObserverHandle, bytes: u64) {
    if let Some(observer) = observer {
        observer.record_request_bytes(bytes);
    }
}

pub(crate) fn note_close_completed(observer: &ObserverHandle) {
    if let Some(observer) = observer {
        observer.record_close_completed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_start_at_honest_zero() {
        let observer = OperationObserver::new();
        assert_eq!(
            observer.counts(),
            DialCounts {
                dial_attempts: 0,
                direct_successes: 0,
                relay_successes: 0,
                request_bytes_sent: 0,
                close_completed: false,
            }
        );
    }

    #[test]
    fn attempts_accumulate_and_successes_split_by_path() {
        let observer = OperationObserver::new();
        for _ in 0..4 {
            observer.record_dial_attempt();
        }
        observer.record_dial_success(TransportPath::Relay);
        observer.record_dial_success(TransportPath::Relay);
        observer.record_dial_success(TransportPath::Direct);

        let counts = observer.counts();
        assert_eq!(counts.dial_attempts, 4, "failed legs still count");
        assert_eq!(counts.relay_successes, 2);
        assert_eq!(counts.direct_successes, 1);
    }

    #[test]
    fn request_bytes_keep_the_furthest_progress_not_the_sum() {
        let observer = OperationObserver::new();
        observer.record_request_bytes(4_096);
        observer.record_request_bytes(1_024); // a retried leg restarting
        observer.record_request_bytes(8_192);
        assert_eq!(observer.counts().request_bytes_sent, 8_192);
    }

    #[test]
    fn close_completed_is_false_until_recorded() {
        let observer = OperationObserver::new();
        assert!(!observer.counts().close_completed);
        observer.record_close_completed();
        assert!(observer.counts().close_completed);
    }

    #[test]
    fn helpers_on_an_absent_handle_are_no_ops() {
        let absent: ObserverHandle = None;
        note_dial_attempt(&absent);
        note_dial_success(&absent, TransportPath::Relay);
        note_request_bytes(&absent, 99);
        note_close_completed(&absent);

        let present: ObserverHandle = Some(OperationObserver::new());
        note_dial_attempt(&present);
        note_request_bytes(&present, 99);
        note_close_completed(&present);
        let counts = present.as_ref().unwrap().counts();
        assert_eq!(counts.dial_attempts, 1);
        assert_eq!(counts.request_bytes_sent, 99);
        assert!(counts.close_completed);
    }
}
