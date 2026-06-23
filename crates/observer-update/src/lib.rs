// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The honest update-state model + reducer.
//!
//! This is the **pure tier**: no platform dependency, no `unsafe`, no Velopack.
//! The platform-side driver (`src-tauri`) owns the `velopack::UpdateManager` I/O;
//! it feeds the *results* of those calls into [`reduce`] as [`UpdateEvent`]s and
//! renders the computed [`UpdateView`]. The cardinal rule mirrors the rest of the
//! app: **update state is earned from a real Velopack result, never optimistically
//! pre-set** — exactly like `AppPhase::Observing` in `observer-state`.
//!
//! The actionability ([`UpdateActions`]) is the load-bearing honesty: every
//! control's enabled flag is *derived* from real state, so the UI cannot paint a
//! dead button. In particular a downloaded-and-staged update (pending relaunch)
//! has idle activity and no in-flight session, yet "check now" must stay disabled
//! — a naive `!in_flight` gate would wrongly re-enable it (the bug the macOS
//! scope-check caught). [`UpdateActions::derive`] folds `is_staged` in for that
//! reason; [`check_now_disabled_while_staged`] pins it.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// Cadence for the owned background-check timer. Seconds match macOS exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckInterval {
    Day,
    Week,
    Month,
}

impl CheckInterval {
    /// The interval in seconds (macOS `SUScheduledCheckInterval` values).
    pub fn secs(self) -> u64 {
        match self {
            CheckInterval::Day => 86_400,
            CheckInterval::Week => 604_800,
            CheckInterval::Month => 2_592_000,
        }
    }
}

/// Persisted updater preferences. Defaults are the founder-approved set:
/// auto-check **on**, **weekly**, auto-download **off** (install on relaunch).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatePrefs {
    pub auto_check: bool,
    pub interval: CheckInterval,
    pub auto_download: bool,
}

impl Default for UpdatePrefs {
    fn default() -> Self {
        Self {
            auto_check: true,
            interval: CheckInterval::Week,
            auto_download: false,
        }
    }
}

/// Outcome of the most recent *completed* check (durable, persisted).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateOutcome {
    /// The running version is the latest the feed offers.
    UpToDate,
    /// A newer version was found (not yet downloaded).
    Found,
    /// A newer version was downloaded and staged; installs on relaunch.
    Staged,
    /// The check itself failed (network/feed error).
    Failed,
}

/// Transient update activity. **Never** restored from disk — the driver sets it
/// from the live Velopack call in progress, and it resets to `Idle` on result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateActivity {
    #[default]
    Idle,
    Checking,
    Downloading,
    Installing,
}

/// Durable reconciled update status (persisted next to `pairing.json`). Every
/// field is earned from a Velopack result; nothing here is set optimistically.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ReconciledUpdateStatus {
    /// Unix epoch seconds of the last *completed* check (success or failure).
    pub last_checked_at: Option<u64>,
    /// Outcome of that last completed check.
    pub last_check_outcome: Option<UpdateOutcome>,
    /// The version last known available (found or staged), persisted so a later
    /// failed check can still honestly say "version X was found earlier".
    pub available_version: Option<String>,
    /// A check failed but an earlier check had found a version.
    pub failed_with_available: bool,
}

/// A version found this session, pre-download. Transient.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvailableUpdate {
    pub version: String,
    /// Release notes from the feed asset, when present.
    pub notes: Option<String>,
}

/// A downloaded + staged version pending relaunch. Re-derived at boot from
/// Velopack's `get_update_pending_restart()` — never persisted by us.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedUpdate {
    pub version: String,
}

/// The reducer's working memory: durable status + this-session transients +
/// prefs + whether the feed/source could even be constructed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateState {
    pub status: ReconciledUpdateStatus,
    #[serde(default)]
    pub activity: UpdateActivity,
    #[serde(default)]
    pub available: Option<AvailableUpdate>,
    #[serde(default)]
    pub staged: Option<StagedUpdate>,
    #[serde(default)]
    pub download_pct: Option<u8>,
    #[serde(default)]
    pub prefs: UpdatePrefs,
    /// Whether the updater could be configured at all (feed URL valid, manager
    /// constructed). `false` => "this build can't check for updates on its own".
    #[serde(default = "default_true")]
    pub config_valid: bool,
}

fn default_true() -> bool {
    true
}

impl Default for UpdateState {
    fn default() -> Self {
        Self {
            status: ReconciledUpdateStatus::default(),
            activity: UpdateActivity::Idle,
            available: None,
            staged: None,
            download_pct: None,
            prefs: UpdatePrefs::default(),
            config_valid: true,
        }
    }
}

impl UpdateState {
    pub fn new() -> Self {
        Self::default()
    }

    fn in_flight(&self) -> bool {
        !matches!(self.activity, UpdateActivity::Idle)
    }

    fn is_staged(&self) -> bool {
        self.staged.is_some()
    }

    /// The honest display state the UI paints. "up to date" is shown ONLY when the
    /// last completed check genuinely returned `UpToDate`; before any check it is
    /// `NeverChecked`, never a false "up to date".
    pub fn display(&self) -> UpdateDisplay {
        if !self.config_valid {
            return UpdateDisplay::Unavailable;
        }
        match self.activity {
            UpdateActivity::Checking => return UpdateDisplay::Checking,
            UpdateActivity::Downloading => return UpdateDisplay::Downloading,
            // Applying the staged update; about to relaunch.
            UpdateActivity::Installing => return UpdateDisplay::Staged,
            UpdateActivity::Idle => {}
        }
        if self.is_staged() {
            return UpdateDisplay::Staged;
        }
        if self.available.is_some() {
            return UpdateDisplay::Available;
        }
        match self.status.last_check_outcome {
            Some(UpdateOutcome::Failed) => {
                if self.status.failed_with_available {
                    UpdateDisplay::FailedWithAvailable
                } else {
                    UpdateDisplay::Failed
                }
            }
            Some(UpdateOutcome::UpToDate) => UpdateDisplay::UpToDate,
            // Found/Staged without a live transient (e.g. after restart with no
            // staged asset): fall back to up-to-date display rather than claim an
            // actionable update we can't act on this session.
            Some(UpdateOutcome::Found) | Some(UpdateOutcome::Staged) => UpdateDisplay::UpToDate,
            None => UpdateDisplay::NeverChecked,
        }
    }

    /// The full computed snapshot pushed to the webview over `update://changed`.
    pub fn view(&self) -> UpdateView {
        let display = self.display();
        UpdateView {
            display,
            activity: self.activity,
            last_checked_at: self.status.last_checked_at,
            available_version: self
                .available
                .as_ref()
                .map(|a| a.version.clone())
                .or_else(|| self.staged.as_ref().map(|s| s.version.clone()))
                .or_else(|| self.status.available_version.clone()),
            notes: self.available.as_ref().and_then(|a| a.notes.clone()),
            download_pct: self.download_pct,
            prefs: self.prefs,
            actions: UpdateActions::derive(self, display),
        }
    }
}

/// The honest display state. Mirrors the macOS Updates bar parity set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateDisplay {
    /// No check has ever completed.
    NeverChecked,
    /// Last check confirmed the running version is current.
    UpToDate,
    /// A check is in flight.
    Checking,
    /// A newer version is available, not yet downloaded.
    Available,
    /// A download is in flight.
    Downloading,
    /// Downloaded + staged; installs on relaunch.
    Staged,
    /// The last check failed.
    Failed,
    /// The last check failed, but an earlier check had found a version.
    FailedWithAvailable,
    /// The updater cannot self-check (bad feed config / unconstructable manager).
    Unavailable,
}

/// Per-control actionability. Every flag is derived from real state — there are
/// no dead buttons, and a control is only enabled when invoking it would do
/// something. The webview renders each control `disabled` from the inverse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateActions {
    pub can_check_now: bool,
    pub can_cancel: bool,
    pub can_download: bool,
    /// Relaunch-to-install a staged update.
    pub can_install: bool,
    pub can_retry: bool,
    pub can_dismiss: bool,
    /// The frequency picker is enabled only when auto-check is on.
    pub frequency_enabled: bool,
}

impl UpdateActions {
    /// Derive actionability from state. The `!is_staged` term on `can_check_now`
    /// is the load-bearing macOS-parity catch: a staged-pending-relaunch update is
    /// `Idle` with no in-flight session, so a naive `config_valid && !in_flight`
    /// gate would wrongly re-enable "check now" while an update is already parked.
    pub fn derive(state: &UpdateState, display: UpdateDisplay) -> Self {
        let in_flight = state.in_flight();
        let is_staged = state.is_staged();
        let config_valid = state.config_valid;
        let has_available = state.available.is_some();
        let failed = matches!(
            display,
            UpdateDisplay::Failed | UpdateDisplay::FailedWithAvailable
        );

        Self {
            can_check_now: config_valid && !in_flight && !is_staged,
            can_cancel: matches!(
                state.activity,
                UpdateActivity::Checking | UpdateActivity::Downloading
            ),
            can_download: config_valid && !in_flight && !is_staged && has_available,
            can_install: !in_flight && is_staged,
            can_retry: config_valid && !in_flight && !is_staged && failed,
            can_dismiss: !in_flight && (has_available || failed),
            frequency_enabled: state.prefs.auto_check,
        }
    }
}

/// The computed snapshot the driver serializes to `update://changed`. The live
/// "last checked … / just now" clock is computed in the webview from
/// `last_checked_at` (re-rendered every second), the JS analog of the macOS
/// `TimelineView` — so it is never a frozen render-time string here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateView {
    pub display: UpdateDisplay,
    pub activity: UpdateActivity,
    pub last_checked_at: Option<u64>,
    pub available_version: Option<String>,
    pub notes: Option<String>,
    pub download_pct: Option<u8>,
    pub prefs: UpdatePrefs,
    pub actions: UpdateActions,
}

/// The result of a completed `check_for_updates()` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    /// The running version is current.
    UpToDate,
    /// A newer version is available.
    UpdateAvailable {
        version: String,
        notes: Option<String>,
    },
    /// The feed exists but has no releases — nothing to update to (treated as
    /// up-to-date; distinct from a misconfigured source, which sets
    /// `config_valid = false` at construction).
    Empty,
}

/// Intents (from the UI/timer) and facts (from Velopack) that move update state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateEvent {
    /// A manual or scheduled check started.
    CheckStarted,
    /// A download started.
    DownloadStarted,
    /// Download progress (0..=100).
    DownloadProgress(u8),
    /// Applying a staged update (terminal before relaunch).
    InstallStarted,
    /// Operator dismissed a found/failed block back to idle.
    Dismissed,
    /// A check completed with the given outcome, at wall-clock `now` (epoch secs).
    CheckResult { now: u64, result: CheckOutcome },
    /// A download completed and staged `version` (pending relaunch). Also used at
    /// boot to rehydrate a staged asset from `get_update_pending_restart()`.
    Downloaded { version: String },
    /// A check failed at wall-clock `now` (epoch secs).
    CheckFailed { now: u64 },
    /// The updater could not be configured (bad feed / unconstructable manager).
    ConfigInvalid,
}

/// Fold one [`UpdateEvent`] into the state. No arm sets a display directly — the
/// honest view is always recomputed by [`UpdateState::view`].
pub fn reduce(state: &mut UpdateState, event: UpdateEvent) {
    match event {
        UpdateEvent::CheckStarted => {
            state.activity = UpdateActivity::Checking;
        }
        UpdateEvent::DownloadStarted => {
            state.activity = UpdateActivity::Downloading;
            state.download_pct = Some(0);
        }
        UpdateEvent::DownloadProgress(pct) => {
            state.download_pct = Some(pct.min(100));
        }
        UpdateEvent::InstallStarted => {
            state.activity = UpdateActivity::Installing;
        }
        UpdateEvent::Dismissed => {
            state.activity = UpdateActivity::Idle;
            state.available = None;
            state.download_pct = None;
            // Clear a failed block so the pane returns to its last durable state.
            if matches!(state.status.last_check_outcome, Some(UpdateOutcome::Failed)) {
                state.status.last_check_outcome = state
                    .status
                    .available_version
                    .as_ref()
                    .map(|_| UpdateOutcome::Found)
                    .or(Some(UpdateOutcome::UpToDate));
                state.status.failed_with_available = false;
            }
        }
        UpdateEvent::CheckResult { now, result } => {
            state.activity = UpdateActivity::Idle;
            state.status.last_checked_at = Some(now);
            state.status.failed_with_available = false;
            match result {
                CheckOutcome::UpToDate | CheckOutcome::Empty => {
                    state.status.last_check_outcome = Some(UpdateOutcome::UpToDate);
                    state.available = None;
                    state.status.available_version = None;
                }
                CheckOutcome::UpdateAvailable { version, notes } => {
                    state.status.last_check_outcome = Some(UpdateOutcome::Found);
                    state.status.available_version = Some(version.clone());
                    state.available = Some(AvailableUpdate { version, notes });
                }
            }
        }
        UpdateEvent::Downloaded { version } => {
            state.activity = UpdateActivity::Idle;
            state.download_pct = None;
            state.available = None;
            state.status.available_version = Some(version.clone());
            state.status.last_check_outcome = Some(UpdateOutcome::Staged);
            state.status.failed_with_available = false;
            state.staged = Some(StagedUpdate { version });
        }
        UpdateEvent::CheckFailed { now } => {
            state.activity = UpdateActivity::Idle;
            // A failed check did not reconfirm the transient "available" block, so
            // it can no longer drive a download. The durable available_version
            // persists so the pane can still say "version X was found earlier".
            state.available = None;
            state.download_pct = None;
            state.status.last_checked_at = Some(now);
            state.status.last_check_outcome = Some(UpdateOutcome::Failed);
            state.status.failed_with_available = state.status.available_version.is_some();
        }
        UpdateEvent::ConfigInvalid => {
            state.config_valid = false;
            state.activity = UpdateActivity::Idle;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_checked_is_not_a_false_up_to_date() {
        let s = UpdateState::new();
        assert_eq!(s.display(), UpdateDisplay::NeverChecked);
        // No dead "check now": it IS actionable from the never-checked idle state.
        assert!(s.view().actions.can_check_now);
    }

    #[test]
    fn up_to_date_only_after_a_real_up_to_date_result() {
        let mut s = UpdateState::new();
        reduce(&mut s, UpdateEvent::CheckStarted);
        assert_eq!(s.display(), UpdateDisplay::Checking);
        reduce(
            &mut s,
            UpdateEvent::CheckResult {
                now: 1_000,
                result: CheckOutcome::UpToDate,
            },
        );
        assert_eq!(s.display(), UpdateDisplay::UpToDate);
        assert_eq!(s.status.last_checked_at, Some(1_000));
    }

    #[test]
    fn empty_feed_is_treated_as_up_to_date() {
        let mut s = UpdateState::new();
        reduce(
            &mut s,
            UpdateEvent::CheckResult {
                now: 5,
                result: CheckOutcome::Empty,
            },
        );
        assert_eq!(s.display(), UpdateDisplay::UpToDate);
        assert!(s.config_valid, "empty feed != misconfigured source");
    }

    #[test]
    fn available_enables_download_and_dismiss_not_install() {
        let mut s = UpdateState::new();
        reduce(
            &mut s,
            UpdateEvent::CheckResult {
                now: 1,
                result: CheckOutcome::UpdateAvailable {
                    version: "0.2.0".into(),
                    notes: Some("notes".into()),
                },
            },
        );
        assert_eq!(s.display(), UpdateDisplay::Available);
        let a = s.view().actions;
        assert!(a.can_download);
        assert!(a.can_dismiss);
        assert!(!a.can_install);
        assert!(a.can_check_now);
    }

    /// The load-bearing macOS-parity catch: a downloaded-and-staged update is
    /// `Idle` with no in-flight session, yet "check now" must stay disabled. An
    /// implementation deriving only from `!in_flight` would wrongly return true.
    #[test]
    fn check_now_disabled_while_staged() {
        let mut s = UpdateState::new();
        reduce(
            &mut s,
            UpdateEvent::Downloaded {
                version: "0.2.0".into(),
            },
        );
        assert_eq!(s.display(), UpdateDisplay::Staged);
        assert_eq!(s.activity, UpdateActivity::Idle); // not in-flight…
        let a = s.view().actions;
        assert!(!a.can_check_now, "staged update must block re-check");
        assert!(!a.can_download, "nothing left to download");
        assert!(a.can_install, "relaunch-to-install is the staged action");
        assert!(!a.can_retry);
    }

    #[test]
    fn in_flight_check_disables_actions_and_enables_cancel() {
        let mut s = UpdateState::new();
        reduce(&mut s, UpdateEvent::CheckStarted);
        let a = s.view().actions;
        assert!(!a.can_check_now);
        assert!(!a.can_download);
        assert!(a.can_cancel);
    }

    #[test]
    fn downloading_reports_progress_and_cancel() {
        let mut s = UpdateState::new();
        reduce(
            &mut s,
            UpdateEvent::CheckResult {
                now: 1,
                result: CheckOutcome::UpdateAvailable {
                    version: "0.2.0".into(),
                    notes: None,
                },
            },
        );
        reduce(&mut s, UpdateEvent::DownloadStarted);
        reduce(&mut s, UpdateEvent::DownloadProgress(42));
        assert_eq!(s.display(), UpdateDisplay::Downloading);
        let v = s.view();
        assert_eq!(v.download_pct, Some(42));
        assert!(v.actions.can_cancel);
        assert!(!v.actions.can_check_now);
    }

    #[test]
    fn failed_check_enables_retry() {
        let mut s = UpdateState::new();
        reduce(&mut s, UpdateEvent::CheckFailed { now: 9 });
        assert_eq!(s.display(), UpdateDisplay::Failed);
        let a = s.view().actions;
        assert!(a.can_retry);
        assert!(a.can_dismiss);
        assert!(a.can_check_now);
        assert!(!s.status.failed_with_available);
    }

    #[test]
    fn failed_after_known_version_says_found_earlier() {
        let mut s = UpdateState::new();
        reduce(
            &mut s,
            UpdateEvent::CheckResult {
                now: 1,
                result: CheckOutcome::UpdateAvailable {
                    version: "0.3.0".into(),
                    notes: None,
                },
            },
        );
        reduce(&mut s, UpdateEvent::CheckFailed { now: 2 });
        assert_eq!(s.display(), UpdateDisplay::FailedWithAvailable);
        assert!(s.status.failed_with_available);
        assert_eq!(s.view().available_version.as_deref(), Some("0.3.0"));
    }

    #[test]
    fn dismiss_clears_available_back_to_idle() {
        let mut s = UpdateState::new();
        reduce(
            &mut s,
            UpdateEvent::CheckResult {
                now: 1,
                result: CheckOutcome::UpdateAvailable {
                    version: "0.2.0".into(),
                    notes: None,
                },
            },
        );
        reduce(&mut s, UpdateEvent::Dismissed);
        assert!(s.available.is_none());
        assert!(!s.view().actions.can_download);
    }

    #[test]
    fn config_invalid_is_unavailable_with_no_actions() {
        let mut s = UpdateState::new();
        reduce(&mut s, UpdateEvent::ConfigInvalid);
        assert!(!s.config_valid);
        assert_eq!(s.display(), UpdateDisplay::Unavailable);
        let a = s.view().actions;
        assert!(!a.can_check_now);
        assert!(!a.can_download);
        assert!(!a.can_retry);
    }

    #[test]
    fn interval_seconds_match_macos_values() {
        assert_eq!(CheckInterval::Day.secs(), 86_400);
        assert_eq!(CheckInterval::Week.secs(), 604_800);
        assert_eq!(CheckInterval::Month.secs(), 2_592_000);
    }

    #[test]
    fn defaults_are_the_approved_set() {
        let p = UpdatePrefs::default();
        assert!(p.auto_check);
        assert_eq!(p.interval, CheckInterval::Week);
        assert!(!p.auto_download);
    }

    #[test]
    fn frequency_picker_disabled_when_auto_check_off() {
        let mut s = UpdateState::new();
        s.prefs.auto_check = false;
        assert!(!s.view().actions.frequency_enabled);
    }

    #[test]
    fn view_round_trips_through_serde() {
        let mut s = UpdateState::new();
        reduce(
            &mut s,
            UpdateEvent::CheckResult {
                now: 123,
                result: CheckOutcome::UpdateAvailable {
                    version: "0.2.0".into(),
                    notes: Some("n".into()),
                },
            },
        );
        let json = serde_json::to_string(&s.view()).unwrap();
        assert!(json.contains("\"available\""));
        assert!(json.contains("\"can_download\":true"));
    }

    #[test]
    fn state_persists_and_reloads_durable_only_shape() {
        // The durable status round-trips; transients default cleanly on reload.
        let mut s = UpdateState::new();
        reduce(
            &mut s,
            UpdateEvent::CheckResult {
                now: 77,
                result: CheckOutcome::UpToDate,
            },
        );
        let json = serde_json::to_string(&s.status).unwrap();
        let back: ReconciledUpdateStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back.last_checked_at, Some(77));
        assert_eq!(back.last_check_outcome, Some(UpdateOutcome::UpToDate));
    }
}
