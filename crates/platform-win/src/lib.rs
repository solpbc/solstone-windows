// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Windows platform glue.
//!
//! **Platform tier** — `windows-rs` quarantine; `unsafe` permitted here only.
//! Holds the OS-bound seams the engine and shell need: the session/power
//! notification pump, the per-session named-mutex single-instance gate, the
//! `%LocalAppData%` path layout, and the real `SegmentFs` / `RecoveryFs`
//! implementations that back the pure rotation and recovery logic.
//!
//! Bootstrap state: the path layout and fs implementations are present as
//! std-only skeletons (so they compile and exercise on any host); the notif pump
//! and named-mutex gate are API-call-free seams that fill in on the build box.
//! The `windows` dependency is target-gated.

use std::path::PathBuf;

use observer_model::SegmentKey;
use observer_recovery::{RecoveryFs, StaleSegment};
use observer_segment::SegmentFs;

/// The per-user data root: `%LocalAppData%\Solstone`. Falls back to a temp path
/// off-Windows so the type is host-constructible for tests.
pub fn local_data_root() -> PathBuf {
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let mut p = PathBuf::from(local);
        p.push("Solstone");
        p
    } else {
        // Host fallback (dev/test). Production always has %LocalAppData%.
        let mut p = std::env::temp_dir();
        p.push("solstone");
        p
    }
}

/// The active segments directory under the data root.
pub fn segments_dir() -> PathBuf {
    local_data_root().join("segments")
}

/// The log directory under the data root (`make run` tails this).
pub fn logs_dir() -> PathBuf {
    local_data_root().join("logs")
}

/// Outcome of the single-instance acquisition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceLock {
    /// This process owns the per-session lock; proceed.
    Acquired,
    /// Another instance already owns it in this interactive session.
    AlreadyRunning,
}

/// Acquire the per-session single-instance lock (a named mutex in the `Local\`
/// namespace = per interactive session = "one observer per session"). On the
/// host this is a no-op that reports `Acquired`; the real `CreateMutexW` +
/// `GetLastError() == ERROR_ALREADY_EXISTS` check lands on the build box.
pub fn acquire_single_instance(_name: &str) -> InstanceLock {
    // TODO(build box): CreateMutexW(Local\Solstone-<session>) + check
    // ERROR_ALREADY_EXISTS; on AlreadyRunning, signal the first instance to
    // surface Settings.
    InstanceLock::Acquired
}

/// A session/power lifecycle notification the pump can deliver to the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemNotification {
    SessionLocked,
    SessionUnlocked,
    DisplayChanged,
    Suspending,
    Resumed,
}

/// The session/power notification pump. Skeleton — the real pump registers a
/// message-only window (`WTSRegisterSessionNotification`, `PBT_*` power
/// broadcasts) on the build box and forwards [`SystemNotification`]s.
#[derive(Debug, Default)]
pub struct NotificationPump;

impl NotificationPump {
    pub fn new() -> Self {
        Self
    }

    /// Drain any pending notifications. Empty on the host skeleton.
    pub fn poll(&mut self) -> Vec<SystemNotification> {
        Vec::new()
    }
}

/// Real `%LocalAppData%`-backed segment filesystem. Skeleton implements the
/// `observer-segment` seam over `std::fs`; the atomic-rename finalize is real.
#[derive(Debug, Default)]
pub struct LocalSegmentFs;

impl SegmentFs for LocalSegmentFs {
    type Error = std::io::Error;

    fn open_incomplete(&mut self, key: SegmentKey) -> Result<String, Self::Error> {
        let dir = segments_dir().join(format!("{}.incomplete", key.index));
        // TODO(build box): create the directory + open per-source writers.
        Ok(dir.to_string_lossy().into_owned())
    }

    fn finalize(&mut self, _key: SegmentKey) -> Result<(), Self::Error> {
        // TODO(build box): atomic rename `<n>.incomplete` -> `<n>`.
        Ok(())
    }
}

/// Real `%LocalAppData%`-backed recovery filesystem (`observer-recovery` seam).
#[derive(Debug, Default)]
pub struct LocalRecoveryFs;

impl RecoveryFs for LocalRecoveryFs {
    type Error = std::io::Error;

    fn scan_incomplete(&mut self) -> Result<Vec<StaleSegment>, Self::Error> {
        // TODO(build box): enumerate `*.incomplete` under segments_dir().
        Ok(Vec::new())
    }

    fn finalize(&mut self, _seg: &StaleSegment) -> Result<(), Self::Error> {
        Ok(())
    }

    fn quarantine(&mut self, _seg: &StaleSegment) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_root_is_under_solstone() {
        assert!(local_data_root().ends_with("Solstone") || local_data_root().ends_with("solstone"));
    }

    #[test]
    fn single_instance_acquires_on_host() {
        assert_eq!(acquire_single_instance("test"), InstanceLock::Acquired);
    }
}
