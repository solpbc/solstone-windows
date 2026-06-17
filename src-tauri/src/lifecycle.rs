// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Session / power lifecycle and single-instance.
//!
//! Bridges the platform notification pump (`platform-win`) into engine intents:
//! a session lock pauses with `SessionLocked`, a suspend pauses with
//! `SystemSuspending`, resume/unlock resumes. The per-session named-mutex
//! single-instance gate is acquired here at boot; a second launch exits cleanly
//! for Wave 1.
//!
//! Update status follows the two-layer model: a durable `ReconciledUpdateStatus`
//! (persisted; last-known-available + last-check-outcome) split from a transient
//! `UpdateActivity` (checking/downloading/installing; never restored from disk).
//! The durable layer is what the tray/UI read — earned from the Velopack
//! callback, never optimistically pre-set.

/// Durable update status (persisted). Read by the UI/tray badge.
#[derive(Debug, Clone, Default)]
pub struct ReconciledUpdateStatus {
    pub last_known_available: Option<String>,
    pub last_check_succeeded: bool,
}

/// Transient update activity. Never restored from disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UpdateActivity {
    #[default]
    Idle,
    Checking,
    Downloading,
    Installing,
}

/// Acquire the single-instance gate at boot.
pub fn acquire_single_instance() -> platform_win::InstanceLock {
    platform_win::acquire_single_instance("Solstone")
}
