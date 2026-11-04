// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! When the app says the journal is connected, connecting or offline.
//!
//! One presenter decides it for every surface: the tray icon and tooltip, its
//! notifications, and the Settings journal line all read the state it returns.
//!
//! Connected is earned by hearing from the journal: an answered upload, an
//! answered listing, or an answered status check. A tick with nothing to send
//! proves nothing, and neither does a state remembered from an earlier run.
//! One answer keeps connected true for a lease that fits a whole check cycle,
//! so one slow or failed check changes nothing. A drop is held for a moment
//! before connected may return, so the icon never flickers. Connecting turns
//! into offline only after it has lasted.
//!
//! The clock is injected: `now_ms` is any monotonic millisecond clock.

use serde::{Deserialize, Serialize};
use strum::{EnumIter, IntoStaticStr};

/// While the app runs on mains power, a status check goes out once nothing has
/// been heard for this long, and never more often.
pub const FOREGROUND_CHECK_SECS: u64 = 15;
/// On battery a status check goes out only once nothing has been heard for a
/// whole segment period. Taking things in already sends one upload per
/// segment, so this spends no more than that.
pub const BACKGROUND_CHECK_SECS: u64 = 300;
/// A status check that has not been answered by then has failed.
pub const CHECK_TIMEOUT_SECS: u64 = 5;
/// A drop from connected is shown at least this long before connected returns.
pub const DOWN_DWELL_MS: u64 = 1_000;
/// Connecting becomes offline after this long without reaching the journal.
pub const OFFLINE_AFTER_MS: u64 = 30_000;
/// Back from a pause, a lock or sleep, connected is held at most this long
/// while a fresh check answers.
pub const RESUME_GRACE_MS: u64 = 500;

/// How long one answer keeps connected true under a check cadence: one full
/// check cycle plus its timeout, and a second of slack.
pub const fn lease_ms(check_secs: u64) -> u64 {
    (check_secs + CHECK_TIMEOUT_SECS + 1) * 1_000
}

/// The journal connection as every surface shows it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, EnumIter, IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum JournalLink {
    Connected,
    Connecting,
    Offline,
}

/// One reading of the evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkInput {
    /// Taking things in and paired with a confirmed journal. Otherwise another
    /// state (paused, not paired, waiting for the mark) is shown instead, and
    /// no link is presented.
    pub up: bool,
    /// A stamp that changes each time the journal answers this device.
    pub answered: Option<u64>,
    /// How long an answer keeps connected true, from the check cadence in force.
    pub lease_ms: u64,
}

/// Folds evidence readings into the presented [`JournalLink`].
#[derive(Debug, Default)]
pub struct LinkPresenter {
    answered: Option<u64>,
    heard_at: Option<u64>,
    /// The longest lease in force since the last answer, so a change of cadence
    /// never shortens what that answer already earned.
    lease_ms: u64,
    shown: Option<JournalLink>,
    last_shown: Option<JournalLink>,
    unreached_since: u64,
    dropped_at: Option<u64>,
    hold_until: Option<u64>,
}

impl LinkPresenter {
    /// Present the link at `now_ms`; `None` while the input is not up.
    pub fn present(&mut self, input: LinkInput, now_ms: u64) -> Option<JournalLink> {
        if input.answered != self.answered {
            self.answered = input.answered;
            self.heard_at = input.answered.map(|_| now_ms);
            self.lease_ms = input.lease_ms;
        } else {
            self.lease_ms = self.lease_ms.max(input.lease_ms);
        }

        if !input.up {
            self.shown = None;
            return None;
        }

        let fresh = self.fresh(now_ms);
        if self.shown.is_none() {
            // A new episode: launch, or back from a pause, a lock, sleep or a
            // new pairing. Connected shown before it is held only briefly.
            self.unreached_since = now_ms;
            self.dropped_at = None;
            self.hold_until = (self.last_shown == Some(JournalLink::Connected) && !fresh)
                .then_some(now_ms.saturating_add(RESUME_GRACE_MS));
        }

        let dwelling = self
            .dropped_at
            .is_some_and(|at| now_ms < at.saturating_add(DOWN_DWELL_MS));
        let holding = !fresh && self.hold_until.is_some_and(|until| now_ms < until);
        let next = if (fresh && !dwelling) || holding {
            JournalLink::Connected
        } else {
            if self.shown == Some(JournalLink::Connected) {
                self.dropped_at = Some(now_ms);
                self.unreached_since = now_ms;
            }
            if now_ms.saturating_sub(self.unreached_since) >= OFFLINE_AFTER_MS {
                JournalLink::Offline
            } else {
                JournalLink::Connecting
            }
        };

        if fresh && next == JournalLink::Connected {
            self.hold_until = None;
        }
        self.shown = Some(next);
        self.last_shown = Some(next);
        Some(next)
    }

    /// The next instant after `now_ms` at which the presented link can change
    /// with no new evidence, so a caller can evaluate again exactly then.
    pub fn next_change_ms(&self, now_ms: u64) -> Option<u64> {
        let shown = self.shown?;
        let fresh_until = self
            .heard_at
            .map(|at| at.saturating_add(self.lease_ms))
            .filter(|_| shown == JournalLink::Connected);
        let dwell_until = self
            .dropped_at
            .map(|at| at.saturating_add(DOWN_DWELL_MS))
            .filter(|_| shown != JournalLink::Connected);
        let offline_at = (shown == JournalLink::Connecting)
            .then(|| self.unreached_since.saturating_add(OFFLINE_AFTER_MS));
        [self.hold_until, fresh_until, dwell_until, offline_at]
            .into_iter()
            .flatten()
            .filter(|at| *at > now_ms)
            .min()
    }

    fn fresh(&self, now_ms: u64) -> bool {
        self.heard_at
            .is_some_and(|at| now_ms < at.saturating_add(self.lease_ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEASE: u64 = lease_ms(FOREGROUND_CHECK_SECS);

    /// A scripted trace: each step is (time, up, answer stamp).
    fn run(steps: &[(u64, bool, Option<u64>)]) -> Vec<(u64, Option<JournalLink>)> {
        let mut presenter = LinkPresenter::default();
        steps
            .iter()
            .map(|&(at, up, answered)| {
                let input = LinkInput {
                    up,
                    answered,
                    lease_ms: LEASE,
                };
                (at, presenter.present(input, at))
            })
            .collect()
    }

    /// Readings once a second from `from` to `to` inclusive, with one answer
    /// stamp throughout.
    fn steady(from: u64, to: u64, answered: Option<u64>) -> Vec<(u64, bool, Option<u64>)> {
        (from..=to)
            .step_by(1_000)
            .map(|at| (at, true, answered))
            .collect()
    }

    fn at(trace: &[(u64, Option<JournalLink>)], when: u64) -> Option<JournalLink> {
        trace
            .iter()
            .rev()
            .find(|(at, _)| *at <= when)
            .and_then(|(_, link)| *link)
    }

    #[test]
    fn i6_a_fresh_launch_never_shows_connected_before_evidence() {
        let trace = run(&steady(0, 60_000, None));
        assert!(trace
            .iter()
            .all(|(_, link)| *link != Some(JournalLink::Connected)));
        assert_eq!(at(&trace, 0), Some(JournalLink::Connecting));

        // The first answer shows at once, with no confirm timer.
        let mut steps = steady(0, 3_000, None);
        steps.push((3_400, true, Some(1)));
        let trace = run(&steps);
        assert_eq!(at(&trace, 3_400), Some(JournalLink::Connected));
    }

    #[test]
    fn i2_connected_is_never_shown_without_an_answer_within_the_lease() {
        // One answer, then silence while everything is up.
        let mut steps = vec![(0, true, Some(7))];
        steps.extend(steady(1_000, 120_000, Some(7)));
        let trace = run(&steps);
        for (when, link) in &trace {
            if *link == Some(JournalLink::Connected) {
                assert!(*when < LEASE, "connected at {when} with the answer at 0");
            }
        }
        assert_eq!(at(&trace, LEASE), Some(JournalLink::Connecting));
    }

    #[test]
    fn i4_a_slow_or_failed_check_inside_the_lease_changes_nothing() {
        // Answers every 15 s; the check due at 45 s is slow and answers only
        // at its timeout. A failed check is no input at all: only the lease
        // running out withdraws connected.
        let late = 45_000 + CHECK_TIMEOUT_SECS * 1_000;
        let mut steps = Vec::new();
        let mut stamp = 0;
        for when in (0..=late).step_by(1_000) {
            if when % 15_000 == 0 && when != 45_000 || when == late {
                stamp += 1;
            }
            steps.push((when, true, Some(stamp)));
        }
        assert!(30_000 + LEASE > late, "the lease fits one slow check cycle");
        let trace = run(&steps);
        assert!(trace
            .iter()
            .all(|(_, link)| *link == Some(JournalLink::Connected)));
    }

    #[test]
    fn i1_a_hard_drop_is_shown_within_one_evaluation() {
        let trace = run(&[(0, true, Some(1)), (1_000, false, Some(1))]);
        assert_eq!(at(&trace, 0), Some(JournalLink::Connected));
        assert_eq!(at(&trace, 1_000), None);
    }

    #[test]
    fn i3_a_drop_holds_at_least_its_dwell_before_connected_returns() {
        // The lease runs out at LEASE; an answer lands 200 ms later.
        let mut steps = vec![(0, true, Some(1))];
        steps.extend(steady(1_000, LEASE, Some(1)));
        steps.push((LEASE + 200, true, Some(2)));
        steps.push((LEASE + 600, true, Some(2)));
        steps.push((LEASE + DOWN_DWELL_MS, true, Some(2)));
        let trace = run(&steps);
        assert_eq!(at(&trace, LEASE), Some(JournalLink::Connecting));
        assert_eq!(at(&trace, LEASE + 200), Some(JournalLink::Connecting));
        assert_eq!(at(&trace, LEASE + 600), Some(JournalLink::Connecting));
        assert_eq!(
            at(&trace, LEASE + DOWN_DWELL_MS),
            Some(JournalLink::Connected)
        );
    }

    #[test]
    fn connecting_becomes_offline_only_after_it_has_lasted() {
        let trace = run(&steady(0, 40_000, None));
        assert_eq!(
            at(&trace, OFFLINE_AFTER_MS - 1_000),
            Some(JournalLink::Connecting)
        );
        assert_eq!(at(&trace, OFFLINE_AFTER_MS), Some(JournalLink::Offline));

        // The first answer after an outage shows at once.
        let mut steps = steady(0, 40_000, None);
        steps.push((40_500, true, Some(1)));
        let trace = run(&steps);
        assert_eq!(at(&trace, 40_500), Some(JournalLink::Connected));
    }

    #[test]
    fn back_from_sleep_holds_connected_briefly_then_shows_the_truth() {
        // Connected, then a long pause for sleep: the old answer has expired.
        let wake = 3_600_000;
        let steps = [
            (0, true, Some(1)),
            (1_000, false, Some(1)),
            (wake, true, Some(1)),
            (wake + RESUME_GRACE_MS - 1, true, Some(1)),
            (wake + RESUME_GRACE_MS, true, Some(1)),
        ];
        let trace = run(&steps);
        assert_eq!(at(&trace, wake), Some(JournalLink::Connected));
        assert_eq!(
            at(&trace, wake + RESUME_GRACE_MS - 1),
            Some(JournalLink::Connected)
        );
        assert_eq!(
            at(&trace, wake + RESUME_GRACE_MS),
            Some(JournalLink::Connecting)
        );

        // A check that answers inside the grace keeps connected throughout.
        let steps = [
            (0, true, Some(1)),
            (1_000, false, Some(1)),
            (wake, true, Some(1)),
            (wake + 200, true, Some(2)),
            (wake + 1_000, true, Some(2)),
        ];
        let trace = run(&steps);
        assert!(trace[2..]
            .iter()
            .all(|(_, link)| *link == Some(JournalLink::Connected)));
    }

    #[test]
    fn a_cadence_change_never_shortens_an_answer_already_earned() {
        let battery = lease_ms(BACKGROUND_CHECK_SECS);
        let mut presenter = LinkPresenter::default();
        let reading = |lease_ms| LinkInput {
            up: true,
            answered: Some(1),
            lease_ms,
        };
        assert_eq!(
            presenter.present(reading(battery), 0),
            Some(JournalLink::Connected)
        );
        // Plugged in 100 s later: the battery lease still covers that answer.
        assert_eq!(
            presenter.present(reading(LEASE), 100_000),
            Some(JournalLink::Connected)
        );
    }

    #[test]
    fn next_change_names_the_moment_the_link_would_change() {
        let mut presenter = LinkPresenter::default();
        let reading = |answered| LinkInput {
            up: true,
            answered,
            lease_ms: LEASE,
        };
        presenter.present(reading(None), 0);
        assert_eq!(presenter.next_change_ms(0), Some(OFFLINE_AFTER_MS));
        presenter.present(reading(Some(1)), 2_000);
        assert_eq!(presenter.next_change_ms(2_000), Some(2_000 + LEASE));

        // Past the drop's dwell with nothing heard, only offline is ahead: an
        // expired deadline is never offered again.
        let dropped = 2_000 + LEASE;
        presenter.present(reading(Some(1)), dropped);
        presenter.present(reading(Some(1)), dropped + DOWN_DWELL_MS + 1);
        assert_eq!(
            presenter.next_change_ms(dropped + DOWN_DWELL_MS + 1),
            Some(dropped + OFFLINE_AFTER_MS)
        );
    }

    #[test]
    fn link_tokens_are_snake_case() {
        assert_eq!(
            serde_json::to_string(&JournalLink::Connecting).unwrap(),
            "\"connecting\""
        );
    }
}
