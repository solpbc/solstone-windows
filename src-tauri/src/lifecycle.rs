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
//! (persisted; last_checked_at + outcome + available version) split from a
//! transient `UpdateActivity` (checking/downloading/installing; never restored
//! from disk). Both now live, expanded, in the pure `observer-update` crate and
//! are driven by the Velopack-backed [`crate::update`] controller — earned from
//! the Velopack result, never optimistically pre-set.

/// Acquire the single-instance gate at boot.
pub fn acquire_single_instance() -> platform_win::InstanceLock {
    platform_win::acquire_single_instance("Solstone")
}
