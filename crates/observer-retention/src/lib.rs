// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Local-cache retention policy — pure tier.
//!
//! Once a segment is confirmed uploaded to the journal, how long should its local
//! copy stick around? [`RetentionConfig`] is the owner's answer, persisted to
//! `retention.json` and edited over IPC. It mirrors the macOS observer's
//! `cacheRetentionDays` exactly: `0` = don't keep (delete as soon as the upload is
//! confirmed — the Windows default to date), `-1` = keep forever, `N` > 0 = keep N
//! days then prune.
//!
//! This crate owns only the decision logic — [`RetentionConfig::delete_on_confirm`]
//! and [`RetentionConfig::should_prune`]. The transport tier (`pl-transport-win`)
//! marks confirmed segments and removes the ones this policy says are past the
//! window. Pure, host-testable. The covenant guard is the transport tier's, not
//! this crate's: only **confirmed-uploaded** segments are ever pruned — unsynced
//! local data is never deleted.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// `keep_days` value meaning "delete the local copy as soon as the upload is
/// confirmed" (keep nothing locally). The default — Windows' behavior to date.
pub const DONT_KEEP: i32 = 0;
/// `keep_days` value meaning "never prune" — keep every confirmed segment locally.
pub const FOREVER: i32 = -1;

const SECS_PER_DAY: u64 = 86_400;

/// Owner cache-retention policy. See the module docs for the `keep_days` encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionConfig {
    /// `0` = don't keep, `-1` = forever, `N` > 0 = keep N days.
    #[serde(default)]
    pub keep_days: i32,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        // Don't keep — the trust-forward, disk-frugal default (and Windows' current
        // behavior). The owner opts into retaining a local cache.
        Self {
            keep_days: DONT_KEEP,
        }
    }
}

impl RetentionConfig {
    /// Canonical form: any value below `FOREVER` collapses to `FOREVER`.
    pub fn normalized(&self) -> Self {
        Self {
            keep_days: self.keep_days.max(FOREVER),
        }
    }

    /// Whether a confirmed upload's local copy should be deleted immediately
    /// (the don't-keep policy). When true the transport tier never retains or
    /// prunes — it removes on confirmation, as it always has.
    pub fn delete_on_confirm(&self) -> bool {
        self.keep_days == DONT_KEEP
    }

    /// Whether confirmed segments are kept forever (never pruned).
    pub fn is_forever(&self) -> bool {
        self.keep_days < 0
    }

    /// The retention window in seconds, when one applies (`N` > 0 days). `None`
    /// for don't-keep (handled at confirm time) and forever (never pruned).
    pub fn window_secs(&self) -> Option<u64> {
        (self.keep_days > 0).then(|| self.keep_days as u64 * SECS_PER_DAY)
    }

    /// Whether a confirmed segment whose aligned boundary is `boundary_epoch_secs`
    /// should be pruned at `now_epoch_secs`: only when a finite window applies and
    /// the segment is older than it. Forever / don't-keep never prune here.
    pub fn should_prune(&self, boundary_epoch_secs: u64, now_epoch_secs: u64) -> bool {
        match self.window_secs() {
            Some(window) => now_epoch_secs.saturating_sub(boundary_epoch_secs) > window,
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_dont_keep() {
        let c = RetentionConfig::default();
        assert_eq!(c.keep_days, 0);
        assert!(c.delete_on_confirm());
        assert!(!c.is_forever());
        assert_eq!(c.window_secs(), None);
    }

    #[test]
    fn forever_never_prunes_or_deletes_on_confirm() {
        let c = RetentionConfig { keep_days: FOREVER };
        assert!(c.is_forever());
        assert!(!c.delete_on_confirm());
        assert_eq!(c.window_secs(), None);
        assert!(!c.should_prune(0, u64::MAX));
    }

    #[test]
    fn days_window_prunes_past_the_boundary() {
        let c = RetentionConfig { keep_days: 7 };
        assert!(!c.delete_on_confirm());
        assert_eq!(c.window_secs(), Some(7 * 86_400));
        // boundary at t=0; window 7 days = 604800s.
        assert!(!c.should_prune(0, 604_800)); // exactly the window — not yet older
        assert!(c.should_prune(0, 604_801)); // one second past -> prune
                                             // a fresh confirmed segment is kept
        assert!(!c.should_prune(1_000_000, 1_000_100));
    }

    #[test]
    fn normalize_clamps_below_forever() {
        assert_eq!(RetentionConfig { keep_days: -5 }.normalized().keep_days, -1);
        assert_eq!(RetentionConfig { keep_days: 30 }.normalized().keep_days, 30);
        assert_eq!(RetentionConfig { keep_days: 0 }.normalized().keep_days, 0);
    }

    #[test]
    fn round_trips_and_defaults() {
        let c = RetentionConfig { keep_days: 14 };
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<RetentionConfig>(&json).unwrap(), c);
        let partial: RetentionConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(partial, RetentionConfig::default());
    }
}
