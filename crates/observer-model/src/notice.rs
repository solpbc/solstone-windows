// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Which state changes earn a desktop notification.
//!
//! The tray shows every state as it happens; a notification interrupts, so it
//! is earned only by a problem that holds and is worth the owner's attention.
//! A problem must persist for its dwell time before it is announced, each
//! problem is announced once, a pause (owner or system) is never announced, and
//! a recovery is announced only after its problem was.

use std::time::Duration;

use crate::{source_owner_name, SourceKind, TrayVisual};

/// Offline only means the journal is out of reach: everything is still saved
/// on this PC, so it is worth a notification only once it has lasted.
pub const OFFLINE_DWELL: Duration = Duration::from_secs(30 * 60);
/// A stopped source is losing material, but the engine retries on its own first.
pub const STOPPED_DWELL: Duration = Duration::from_secs(2 * 60);
/// A refused permission does not clear without the owner; the dwell only rides
/// out a momentary reading.
pub const PERMISSION_DWELL: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// Paired, but the journal is out of reach.
    Offline,
    /// A source or storage faulted.
    Stopped,
    /// Windows is refusing a source.
    NeedsPermission(SourceKind),
}

impl Problem {
    fn from_tray(visual: TrayVisual, blocked: Option<SourceKind>) -> Option<Self> {
        match visual {
            TrayVisual::Offline => Some(Self::Offline),
            TrayVisual::Error => Some(Self::Stopped),
            TrayVisual::NeedsPermission => blocked.map(Self::NeedsPermission),
            TrayVisual::Healthy | TrayVisual::Connecting | TrayVisual::Paused | TrayVisual::Off => {
                None
            }
        }
    }

    fn dwell(self) -> Duration {
        match self {
            Self::Offline => OFFLINE_DWELL,
            Self::Stopped => STOPPED_DWELL,
            Self::NeedsPermission(_) => PERMISSION_DWELL,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    Problem(Problem),
    Recovered(Problem),
}

impl Notice {
    pub fn title(self) -> String {
        match self {
            Self::Problem(Problem::Offline) => "your journal is out of reach".to_string(),
            Self::Problem(Problem::Stopped) => "intake ran into a problem".to_string(),
            Self::Problem(Problem::NeedsPermission(kind)) => {
                format!("the {} needs your permission", source_owner_name(kind))
            }
            Self::Recovered(Problem::Offline) => "your journal is back in reach".to_string(),
            Self::Recovered(Problem::Stopped) => "intake is running again".to_string(),
            Self::Recovered(Problem::NeedsPermission(kind)) => {
                format!("the {} is back", source_owner_name(kind))
            }
        }
    }

    pub fn body(self) -> String {
        match self {
            Self::Problem(Problem::Offline) => "solstone is still on. everything is saved on this PC and goes into your journal once it's back in reach.".to_string(),
            Self::Problem(Problem::Stopped) => {
                "the solstone icon in the taskbar says what's wrong.".to_string()
            }
            Self::Problem(Problem::NeedsPermission(kind)) => format!(
                "windows privacy settings are blocking it. turn on {} access for desktop apps, and it goes into your journal again.",
                source_owner_name(kind)
            ),
            Self::Recovered(Problem::Offline) => {
                "everything saved on this PC is going into your journal now.".to_string()
            }
            Self::Recovered(Problem::Stopped) => {
                "everything you turned on is going into your journal again.".to_string()
            }
            Self::Recovered(Problem::NeedsPermission(_)) => {
                "it's going into your journal again.".to_string()
            }
        }
    }
}

/// Folds each tray reading into at most one notice.
#[derive(Debug, Default)]
pub struct TransitionNotifier {
    pending: Option<(Problem, u64)>,
    announced: Option<Problem>,
}

impl TransitionNotifier {
    /// Observe the tray state at `now_secs` (any monotonic seconds clock).
    pub fn observe(
        &mut self,
        visual: TrayVisual,
        blocked: Option<SourceKind>,
        now_secs: u64,
    ) -> Option<Notice> {
        if let Some(problem) = Problem::from_tray(visual, blocked) {
            if self.announced == Some(problem) {
                self.pending = None;
                return None;
            }
            let since = match self.pending {
                Some((pending, since)) if pending == problem => since,
                _ => {
                    self.pending = Some((problem, now_secs));
                    now_secs
                }
            };
            if now_secs.saturating_sub(since) >= problem.dwell().as_secs() {
                self.pending = None;
                self.announced = Some(problem);
                return Some(Notice::Problem(problem));
            }
            return None;
        }

        match visual {
            TrayVisual::Healthy => {
                self.pending = None;
                self.announced.take().map(Notice::Recovered)
            }
            // The owner or the system chose this; whatever was wrong is no
            // longer the thing to report.
            TrayVisual::Paused | TrayVisual::Off => {
                self.pending = None;
                self.announced = None;
                None
            }
            // The engine passes through starting each time it retries a stopped
            // source, so that does not end the wait for a stop to clear.
            _ => {
                if !matches!(self.pending, Some((Problem::Stopped, _))) {
                    self.pending = None;
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(notifier: &mut TransitionNotifier, steps: &[(TrayVisual, u64)]) -> Vec<Notice> {
        steps
            .iter()
            .filter_map(|(visual, at)| notifier.observe(*visual, None, *at))
            .collect()
    }

    #[test]
    fn brief_upload_error_that_clears_on_its_own_produces_no_notice() {
        let mut notifier = TransitionNotifier::default();
        let mut steps = vec![(TrayVisual::Healthy, 0)];
        // One failed sync tick flips the tray to offline for a few seconds.
        steps.extend((1..=20).map(|t| (TrayVisual::Offline, t)));
        steps.extend((21..=40).map(|t| (TrayVisual::Healthy, t)));
        assert!(run(&mut notifier, &steps).is_empty());
    }

    #[test]
    fn brief_source_fault_that_recovers_produces_no_notice() {
        let mut notifier = TransitionNotifier::default();
        let steps = [
            (TrayVisual::Healthy, 0),
            (TrayVisual::Error, 1),
            (TrayVisual::Connecting, 3),
            (TrayVisual::Healthy, 5),
        ];
        assert!(run(&mut notifier, &steps).is_empty());
    }

    #[test]
    fn offline_that_holds_is_announced_once_then_its_recovery() {
        let mut notifier = TransitionNotifier::default();
        let dwell = OFFLINE_DWELL.as_secs();
        let steps = [
            (TrayVisual::Healthy, 0),
            (TrayVisual::Offline, 10),
            (TrayVisual::Offline, 10 + dwell - 1),
            (TrayVisual::Offline, 10 + dwell),
            (TrayVisual::Offline, 10 + dwell * 3),
            (TrayVisual::Healthy, 10 + dwell * 3 + 1),
            (TrayVisual::Healthy, 10 + dwell * 3 + 2),
        ];
        assert_eq!(
            run(&mut notifier, &steps),
            vec![
                Notice::Problem(Problem::Offline),
                Notice::Recovered(Problem::Offline)
            ]
        );
    }

    #[test]
    fn a_stop_that_keeps_failing_through_retries_is_announced() {
        let mut notifier = TransitionNotifier::default();
        let mut steps = Vec::new();
        let mut t = 0;
        while t <= STOPPED_DWELL.as_secs() + 10 {
            steps.push((TrayVisual::Error, t));
            steps.push((TrayVisual::Connecting, t + 1));
            t += 5;
        }
        assert_eq!(
            run(&mut notifier, &steps),
            vec![Notice::Problem(Problem::Stopped)]
        );
    }

    #[test]
    fn pauses_are_never_announced_and_end_a_problem_quietly() {
        let mut notifier = TransitionNotifier::default();
        let dwell = STOPPED_DWELL.as_secs();
        let steps = [
            (TrayVisual::Paused, 0),
            (TrayVisual::Paused, 10 * dwell),
            (TrayVisual::Error, 10 * dwell + 1),
            (TrayVisual::Error, 11 * dwell + 1),
            (TrayVisual::Paused, 11 * dwell + 2),
            (TrayVisual::Healthy, 11 * dwell + 3),
        ];
        assert_eq!(
            run(&mut notifier, &steps),
            vec![Notice::Problem(Problem::Stopped)]
        );
    }

    #[test]
    fn refused_permission_is_announced_after_its_short_dwell() {
        let mut notifier = TransitionNotifier::default();
        let mic = Some(SourceKind::Mic);
        let dwell = PERMISSION_DWELL.as_secs();
        assert_eq!(notifier.observe(TrayVisual::NeedsPermission, mic, 0), None);
        assert_eq!(
            notifier.observe(TrayVisual::NeedsPermission, mic, dwell),
            Some(Notice::Problem(Problem::NeedsPermission(SourceKind::Mic)))
        );
        assert_eq!(
            notifier.observe(TrayVisual::NeedsPermission, mic, dwell * 5),
            None
        );
        assert_eq!(
            notifier.observe(TrayVisual::Healthy, None, dwell * 5 + 1),
            Some(Notice::Recovered(Problem::NeedsPermission(SourceKind::Mic)))
        );
    }

    #[test]
    fn connecting_after_offline_does_not_carry_the_offline_wait() {
        let mut notifier = TransitionNotifier::default();
        let dwell = OFFLINE_DWELL.as_secs();
        let steps = [
            (TrayVisual::Offline, 0),
            (TrayVisual::Connecting, 1),
            (TrayVisual::Offline, dwell),
        ];
        assert!(run(&mut notifier, &steps).is_empty());
    }
}
