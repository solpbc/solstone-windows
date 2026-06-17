// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Incomplete-segment recovery.
//!
//! On engine construction — before any source starts — stale `.incomplete`
//! segment directories left by a crash or hard power-off are scanned and either
//! finalized (if they hold usable, sealed-able data) or quarantined. The
//! decision is pure logic over the [`RecoveryFs`] seam; `platform-win` supplies
//! the real `%LocalAppData%`-backed implementation and tests supply a fake.

#![forbid(unsafe_code)]

use observer_model::SegmentKey;

/// A stale segment found during the recovery scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleSegment {
    pub key: SegmentKey,
    pub path: String,
    /// True when the segment dir holds at least one usable media chunk and can
    /// be sealed; false when it is empty/corrupt and should be quarantined.
    pub has_usable_data: bool,
}

/// What recovery did with one stale segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryOutcome {
    Finalized(SegmentKey),
    Quarantined(SegmentKey),
}

/// Filesystem seam for recovery. Real impl in `platform-win`; fake in tests.
pub trait RecoveryFs {
    type Error: core::fmt::Debug;

    /// Enumerate stale `.incomplete` segment directories from a prior run.
    fn scan_incomplete(&mut self) -> Result<Vec<StaleSegment>, Self::Error>;

    /// Seal a usable stale segment to its final name (atomic rename).
    fn finalize(&mut self, seg: &StaleSegment) -> Result<(), Self::Error>;

    /// Move an unusable stale segment aside into quarantine.
    fn quarantine(&mut self, seg: &StaleSegment) -> Result<(), Self::Error>;
}

/// Scan and resolve every stale segment. Runs once at construction; returns the
/// per-segment outcomes (recovery is best-effort: a per-segment error is mapped
/// to quarantine rather than aborting the whole sweep).
pub fn recover_all<F: RecoveryFs>(fs: &mut F) -> Result<Vec<RecoveryOutcome>, F::Error> {
    let stale = fs.scan_incomplete()?;
    let mut outcomes = Vec::with_capacity(stale.len());
    for seg in stale {
        if seg.has_usable_data {
            match fs.finalize(&seg) {
                Ok(()) => outcomes.push(RecoveryOutcome::Finalized(seg.key)),
                Err(_) => {
                    fs.quarantine(&seg)?;
                    outcomes.push(RecoveryOutcome::Quarantined(seg.key));
                }
            }
        } else {
            fs.quarantine(&seg)?;
            outcomes.push(RecoveryOutcome::Quarantined(seg.key));
        }
    }
    Ok(outcomes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeFs {
        stale: Vec<StaleSegment>,
        finalized: Vec<SegmentKey>,
        quarantined: Vec<SegmentKey>,
    }

    impl RecoveryFs for FakeFs {
        type Error = ();
        fn scan_incomplete(&mut self) -> Result<Vec<StaleSegment>, ()> {
            Ok(self.stale.clone())
        }
        fn finalize(&mut self, seg: &StaleSegment) -> Result<(), ()> {
            self.finalized.push(seg.key);
            Ok(())
        }
        fn quarantine(&mut self, seg: &StaleSegment) -> Result<(), ()> {
            self.quarantined.push(seg.key);
            Ok(())
        }
    }

    fn seg(index: u64, usable: bool) -> StaleSegment {
        StaleSegment {
            key: SegmentKey { boundary_epoch_secs: index * 300, index },
            path: format!("/seg/{index}.incomplete"),
            has_usable_data: usable,
        }
    }

    #[test]
    fn usable_finalized_unusable_quarantined() {
        let mut fs = FakeFs {
            stale: vec![seg(1, true), seg(2, false)],
            ..Default::default()
        };
        let out = recover_all(&mut fs).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(fs.finalized, vec![SegmentKey { boundary_epoch_secs: 300, index: 1 }]);
        assert_eq!(fs.quarantined, vec![SegmentKey { boundary_epoch_secs: 600, index: 2 }]);
    }

    #[test]
    fn empty_scan_is_a_noop() {
        let mut fs = FakeFs::default();
        assert!(recover_all(&mut fs).unwrap().is_empty());
    }
}
