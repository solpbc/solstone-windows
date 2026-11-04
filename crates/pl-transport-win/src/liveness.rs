// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The status check that keeps "connected" earned when nothing else is heard.
//!
//! The app sets how often a check may go out (see
//! [`observer_model::journal_link`]); the upload coordinator runs the checks
//! and records every answer from the journal. Each check is one fresh dial.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use observer_model::journal_link::FOREGROUND_CHECK_SECS;
use tokio::sync::Notify;

/// Never more than one status check in this window, whatever asks for it.
pub const MIN_CHECK_SPACING: Duration = Duration::from_secs(FOREGROUND_CHECK_SECS);

/// Shared between the app, which sets the cadence, and the uploader.
#[derive(Debug)]
pub struct Liveness {
    cadence_secs: AtomicU64,
    pub(crate) resumed: Notify,
    pub(crate) recovered: Notify,
}

impl Default for Liveness {
    fn default() -> Self {
        Self {
            cadence_secs: AtomicU64::new(FOREGROUND_CHECK_SECS),
            resumed: Notify::new(),
            recovered: Notify::new(),
        }
    }
}

impl Liveness {
    /// Check once nothing has been heard for `secs`; `0` stops idle checks.
    pub fn set_cadence(&self, secs: u64) {
        self.cadence_secs.store(secs, Ordering::Relaxed);
    }

    pub(crate) fn cadence(&self) -> Option<Duration> {
        match self.cadence_secs.load(Ordering::Relaxed) {
            0 => None,
            secs => Some(Duration::from_secs(secs)),
        }
    }

    /// Taking things in again after a pause, a lock or sleep: check as soon as
    /// the spacing allows, and stop backing off uploads.
    pub fn resumed(&self) {
        self.resumed.notify_one();
        self.recovered.notify_one();
    }
}
