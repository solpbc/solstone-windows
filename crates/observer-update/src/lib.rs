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
//! dead button. A downloaded-and-staged update (pending relaunch)
//! has idle activity and no in-flight session, and "check now" remains enabled
//! while parked. [`check_enabled_while_staged_and_disabled_while_in_flight`] pins it.

#![forbid(unsafe_code)]

pub mod admission;

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

/// Persisted updater preferences. Defaults are the operator-approved set:
/// auto-check **on**, **daily**, auto-download **on** (install on relaunch).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatePrefs {
    pub auto_check: bool,
    pub interval: CheckInterval,
    pub auto_download: bool,
}

/// Which update preferences the owner has set.
///
/// A `None` field has not been set. It reads as the current default, so a later
/// default change still reaches that owner. `Some` is the owner's choice even
/// when it equals today's default. Owner setters write it. Opening a file from
/// before this record also keeps concrete values that differ from the defaults.
/// This is not the migration marker ([`UpdateDocument::daily_background`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplicitUpdateChoices {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_check: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<CheckInterval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_download: Option<bool>,
}

impl ExplicitUpdateChoices {
    pub fn is_unset(self) -> bool {
        self.auto_check.is_none() && self.interval.is_none() && self.auto_download.is_none()
    }

    /// Effective preferences. Unset fields take `defaults`.
    pub fn resolve(self, defaults: UpdatePrefs) -> UpdatePrefs {
        UpdatePrefs {
            auto_check: self.auto_check.unwrap_or(defaults.auto_check),
            interval: self.interval.unwrap_or(defaults.interval),
            auto_download: self.auto_download.unwrap_or(defaults.auto_download),
        }
    }

    /// A document that predates this record stored concrete prefs on every save.
    /// A value that still equals `defaults` was not an owner choice we can see.
    /// A value that differs was set by the owner and is kept.
    pub fn from_nondefault_prefs(prefs: UpdatePrefs, defaults: UpdatePrefs) -> Self {
        Self {
            auto_check: (prefs.auto_check != defaults.auto_check).then_some(prefs.auto_check),
            interval: (prefs.interval != defaults.interval).then_some(prefs.interval),
            auto_download: (prefs.auto_download != defaults.auto_download)
                .then_some(prefs.auto_download),
        }
    }
}

impl Default for UpdatePrefs {
    fn default() -> Self {
        Self {
            auto_check: true,
            interval: CheckInterval::Day,
            auto_download: true,
        }
    }
}

/// Persisted update document on disk (`update.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateDocument {
    #[serde(default)]
    pub prefs: UpdatePrefs,
    #[serde(default)]
    pub status: ReconciledUpdateStatus,
    /// Legacy migration has finished. Not a record of which prefs the owner set.
    #[serde(default)]
    pub daily_background: bool,
    /// Present once this file uses the explicit-choice record, including when
    /// every field is still unset. Absent on a `daily_background` file written
    /// before the record existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explicit_choices: Option<ExplicitUpdateChoices>,
}

impl UpdateDocument {
    fn choices_mut(&mut self) -> &mut ExplicitUpdateChoices {
        self.explicit_choices
            .get_or_insert_with(ExplicitUpdateChoices::default)
    }

    /// Record an owner change of auto-check. Does not touch the migration marker.
    pub fn set_auto_check(&mut self, on: bool) {
        self.choices_mut().auto_check = Some(on);
        self.prefs.auto_check = on;
    }

    /// Record an owner change of the check interval. Does not touch the migration marker.
    pub fn set_interval(&mut self, interval: CheckInterval) {
        self.choices_mut().interval = Some(interval);
        self.prefs.interval = interval;
    }

    /// Record an owner change of auto-download. Does not touch the migration marker.
    pub fn set_auto_download(&mut self, on: bool) {
        self.choices_mut().auto_download = Some(on);
        self.prefs.auto_download = on;
    }
}

/// Create a fresh document for an unreadable or missing file.
/// This sets the migration marker to true without running migration, and writes
/// an empty explicit-choice record so the defaults are not owner edits.
pub fn fresh_update_document() -> UpdateDocument {
    UpdateDocument {
        prefs: UpdatePrefs::default(),
        status: ReconciledUpdateStatus::default(),
        daily_background: true,
        explicit_choices: Some(ExplicitUpdateChoices::default()),
    }
}

/// Deserialize the persisted update document from JSON text.
pub fn load_update_document(text: &str) -> Result<UpdateDocument, serde_json::Error> {
    serde_json::from_str::<UpdateDocument>(text)
}

/// Migrate a legacy readable document once. If `daily_background` is false or missing,
/// sets the approved default preferences and the marker, copying status through unchanged.
/// Legacy preference values are not owner choices, so the explicit record is empty.
pub fn migrate_update_document(mut doc: UpdateDocument) -> UpdateDocument {
    if !doc.daily_background {
        doc.prefs = UpdatePrefs::default();
        doc.explicit_choices = Some(ExplicitUpdateChoices::default());
        doc.daily_background = true;
    }
    doc
}

/// Load path: migrate once, then resolve prefs from the explicit-choice record.
///
/// A `daily_background` file with no record yet adopted only the concrete prefs
/// that differ from the defaults (the owner setters of that format). Unset
/// fields then take the current defaults, ignoring a stale prefs cache.
pub fn open_update_document(mut doc: UpdateDocument) -> UpdateDocument {
    doc = migrate_update_document(doc);
    if doc.explicit_choices.is_none() {
        doc.explicit_choices = Some(ExplicitUpdateChoices::from_nondefault_prefs(
            doc.prefs,
            UpdatePrefs::default(),
        ));
    }
    let choices = doc.explicit_choices.unwrap_or_default();
    doc.prefs = choices.resolve(UpdatePrefs::default());
    doc
}

/// Network cost classification for update transfers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkCostKind {
    Unrestricted,
    Fixed,
    Variable,
    Unknown,
}

/// Network cost profile reported by the platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkCost {
    pub kind: NetworkCostKind,
    pub roaming: bool,
    pub background_restricted: bool,
}

/// True only when auto_download is enabled on an unrestricted, non-roaming,
/// background-unrestricted connection.
pub fn automatic_download_eligible(auto_download: bool, cost: NetworkCost) -> bool {
    auto_download
        && cost.kind == NetworkCostKind::Unrestricted
        && !cost.roaming
        && !cost.background_restricted
}

/// True when the user may explicitly initiate a download (metered or unknown allowed;
/// roaming and background-use restrictions refused).
pub fn explicit_download_allowed(cost: NetworkCost) -> bool {
    !cost.roaming && !cost.background_restricted
}

/// Reasons why an automatic download is deferred due to connection cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostDeferral {
    Metered,
    Roaming,
    Restricted,
    Unknown,
}

/// Returns the deferral reason in priority order: Roaming, Restricted, Metered, Unknown.
/// Returns None when the connection is eligible for automatic download (ignoring the auto-download pref).
pub fn cost_deferral(cost: NetworkCost) -> Option<CostDeferral> {
    if cost.roaming {
        Some(CostDeferral::Roaming)
    } else if cost.background_restricted {
        Some(CostDeferral::Restricted)
    } else if matches!(
        cost.kind,
        NetworkCostKind::Fixed | NetworkCostKind::Variable
    ) {
        Some(CostDeferral::Metered)
    } else if cost.kind == NetworkCostKind::Unknown {
        Some(CostDeferral::Unknown)
    } else {
        None
    }
}

/// Determine whether a scheduled update check is due.
pub fn check_is_due(
    now: u64,
    last_checked_at: Option<u64>,
    interval: CheckInterval,
    auto_check: bool,
    in_flight: bool,
) -> bool {
    if !auto_check || in_flight {
        return false;
    }
    match last_checked_at {
        None => true,
        Some(t) => now.saturating_sub(t) >= interval.secs(),
    }
}

/// Automatic retry delay after an automatic download failure (15 minutes).
pub const AUTOMATIC_RETRY_SECS: u64 = 900;

/// Determine whether an automatic download should start.
pub fn automatic_download_should_start(
    now: u64,
    auto_download: bool,
    cost: NetworkCost,
    in_flight: bool,
    have_unadmitted_target: bool,
    last_automatic_failure_at: Option<u64>,
) -> bool {
    if in_flight || !have_unadmitted_target || !automatic_download_eligible(auto_download, cost) {
        return false;
    }
    if let Some(failed_at) = last_automatic_failure_at {
        now >= failed_at.saturating_add(AUTOMATIC_RETRY_SECS)
    } else {
        true
    }
}

/// Returns true only when there is no pending package or the feed offer is strictly newer.
pub fn feed_replaces_pending(pending: Option<&semver::Version>, feed: &semver::Version) -> bool {
    match pending {
        None => true,
        Some(p) => feed > p,
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
    #[serde(default)]
    pub last_checked_at: Option<u64>,
    /// Outcome of that last completed check.
    #[serde(default)]
    pub last_check_outcome: Option<UpdateOutcome>,
    /// The version last known available (found or staged), persisted so a later
    /// failed check can still honestly say "version X was found earlier".
    #[serde(default)]
    pub available_version: Option<String>,
    /// A check failed but an earlier check had found a version.
    #[serde(default)]
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
    pub cost_deferral: Option<CostDeferral>,
    #[serde(default)]
    pub download_failed: bool,
    #[serde(default)]
    pub install_failed: bool,
    #[serde(default)]
    pub prefs: UpdatePrefs,
    /// Owner choices carried through every save, including a background status write.
    /// Unset fields are not choices. Not the migration marker.
    #[serde(default)]
    pub explicit_choices: ExplicitUpdateChoices,
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
            cost_deferral: None,
            download_failed: false,
            install_failed: false,
            prefs: UpdatePrefs::default(),
            explicit_choices: ExplicitUpdateChoices::default(),
            config_valid: true,
        }
    }
}

impl UpdateState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Owner setter. Records the choice and the effective preference together.
    pub fn set_auto_check(&mut self, on: bool) {
        self.explicit_choices.auto_check = Some(on);
        self.prefs.auto_check = on;
    }

    /// Owner setter. Records the choice and the effective preference together.
    pub fn set_interval(&mut self, interval: CheckInterval) {
        self.explicit_choices.interval = Some(interval);
        self.prefs.interval = interval;
    }

    /// Owner setter. Records the choice and the effective preference together.
    pub fn set_auto_download(&mut self, on: bool) {
        self.explicit_choices.auto_download = Some(on);
        self.prefs.auto_download = on;
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
        if self.install_failed {
            return UpdateDisplay::Failed;
        }
        if self.download_failed {
            return if self.available.is_some() {
                UpdateDisplay::FailedWithAvailable
            } else {
                UpdateDisplay::Failed
            };
        }
        if matches!(self.status.last_check_outcome, Some(UpdateOutcome::Failed)) {
            if self.status.failed_with_available {
                return UpdateDisplay::FailedWithAvailable;
            } else {
                return UpdateDisplay::Failed;
            }
        }
        if self.available.is_some() {
            return UpdateDisplay::Available;
        }
        match self.status.last_check_outcome {
            Some(UpdateOutcome::UpToDate) => UpdateDisplay::UpToDate,
            // Found/Staged without a live transient: fall back to failed / failed-with-available
            // rather than claiming an up-to-date state after losing or missing files.
            Some(UpdateOutcome::Found) | Some(UpdateOutcome::Staged) => {
                if self.status.failed_with_available {
                    UpdateDisplay::FailedWithAvailable
                } else {
                    UpdateDisplay::Failed
                }
            }
            None => UpdateDisplay::NeverChecked,
            Some(UpdateOutcome::Failed) => unreachable!(),
        }
    }

    /// The full computed snapshot pushed to the webview over `update://changed`.
    pub fn view(&self, explicit_allowed: bool) -> UpdateView {
        let display = self.display();
        let recovery = if self.install_failed {
            Some(Recovery::Install)
        } else if self.download_failed {
            Some(Recovery::Download)
        } else if self.status.last_check_outcome == Some(UpdateOutcome::Failed)
            || matches!(
                display,
                UpdateDisplay::Failed | UpdateDisplay::FailedWithAvailable
            )
        {
            Some(Recovery::Check)
        } else {
            None
        };
        UpdateView {
            display,
            activity: self.activity,
            last_checked_at: self.status.last_checked_at,
            available_version: self
                .available
                .as_ref()
                .map(|a| a.version.clone())
                .or_else(|| {
                    if self.staged.is_none() {
                        self.status.available_version.clone()
                    } else {
                        None
                    }
                }),
            downloaded_version: self.staged.as_ref().map(|s| s.version.clone()),
            downloaded: self.is_staged(),
            notes: self.available.as_ref().and_then(|a| a.notes.clone()),
            download_pct: self.download_pct,
            cost_deferral: self.cost_deferral,
            installs_next_start: self.is_staged(),
            recovery,
            prefs: self.prefs,
            actions: UpdateActions::derive(self, display, explicit_allowed),
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

/// Recovery action available to the owner after a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Recovery {
    Check,
    Download,
    Install,
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
    /// Derive actionability from state. Check-now remains enabled while staged.
    pub fn derive(state: &UpdateState, display: UpdateDisplay, explicit_allowed: bool) -> Self {
        let in_flight = state.in_flight();
        let is_staged = state.is_staged();
        let config_valid = state.config_valid;
        let has_unadmitted_target = match (&state.available, &state.staged) {
            (Some(_avail), None) => true,
            (Some(avail), Some(staged)) => {
                match (
                    semver::Version::parse(&avail.version),
                    semver::Version::parse(&staged.version),
                ) {
                    (Ok(v_avail), Ok(v_staged)) => v_avail > v_staged,
                    _ => false,
                }
            }
            (None, _) => false,
        };
        let failed = matches!(
            display,
            UpdateDisplay::Failed | UpdateDisplay::FailedWithAvailable
        );

        Self {
            can_check_now: config_valid && !in_flight,
            can_cancel: matches!(
                state.activity,
                UpdateActivity::Checking | UpdateActivity::Downloading
            ),
            can_download: config_valid && !in_flight && explicit_allowed && has_unadmitted_target,
            can_install: !in_flight && is_staged,
            can_retry: config_valid && !in_flight && failed,
            can_dismiss: !in_flight && (state.available.is_some() || failed),
            frequency_enabled: state.prefs.auto_check,
        }
    }
}

/// The computed snapshot the driver serializes to `update://changed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateView {
    pub display: UpdateDisplay,
    pub activity: UpdateActivity,
    pub last_checked_at: Option<u64>,
    pub available_version: Option<String>,
    pub downloaded_version: Option<String>,
    pub downloaded: bool,
    pub notes: Option<String>,
    pub download_pct: Option<u8>,
    pub cost_deferral: Option<CostDeferral>,
    pub installs_next_start: bool,
    pub recovery: Option<Recovery>,
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
    /// Applying an authenticated pending update failed without changing its identity.
    InstallFailed,
    /// Operator dismissed a found/failed block back to idle.
    Dismissed,
    /// A check completed with the given outcome, at wall-clock `now` (epoch secs).
    CheckResult { now: u64, result: CheckOutcome },
    /// A download completed and staged `version` (pending relaunch). Also used at
    /// boot to rehydrate a staged asset from `get_update_pending_restart()`.
    Downloaded { version: String },
    /// A completed feed check failed at wall-clock `now` (epoch secs).
    CheckFailed { now: u64 },
    /// An in-progress package download failed.
    DownloadFailed,
    /// An automatic download was deferred due to connection cost.
    DownloadDeferred { reason: CostDeferral },
    /// Cache package file missing or rejected at boot or install.
    PendingMissing,
    /// The updater could not be configured (bad feed / unconstructable manager).
    ConfigInvalid,
}

/// Canonical relative-time spec for the "last checked …" line, with the
/// **< 60s → "just now"** threshold. The webview computes the *live* clock from
/// `last_checked_at` on a 1s tick (the JS analog of the macOS `TimelineView`), so
/// this is the spec the TS mirror is held to — pinned here so the threshold can
/// never silently drift. `now` and `checked_at` are unix epoch seconds.
pub fn last_checked_relative(checked_at: Option<u64>, now: u64) -> String {
    let Some(t) = checked_at else {
        return "never checked for updates".to_string();
    };
    let secs = now.saturating_sub(t);
    if secs < 60 {
        "checked just now".to_string()
    } else if secs < 3_600 {
        let m = secs / 60;
        format!("checked {m} minute{} ago", if m == 1 { "" } else { "s" })
    } else if secs < 86_400 {
        let h = secs / 3_600;
        format!("checked {h} hour{} ago", if h == 1 { "" } else { "s" })
    } else {
        let d = secs / 86_400;
        format!("checked {d} day{} ago", if d == 1 { "" } else { "s" })
    }
}

/// Fold one [`UpdateEvent`] into the state. No arm sets a display directly — the
/// honest view is always recomputed by [`UpdateState::view`].
pub fn reduce(state: &mut UpdateState, event: UpdateEvent) {
    match event {
        UpdateEvent::CheckStarted => {
            state.activity = UpdateActivity::Checking;
            state.install_failed = false;
            state.cost_deferral = None;
            state.download_failed = false;
        }
        UpdateEvent::DownloadStarted => {
            state.activity = UpdateActivity::Downloading;
            state.install_failed = false;
            state.download_pct = Some(0);
            state.cost_deferral = None;
            state.download_failed = false;
        }
        UpdateEvent::DownloadProgress(pct) => {
            state.download_pct = Some(pct.min(100));
        }
        UpdateEvent::InstallStarted => {
            state.activity = UpdateActivity::Installing;
            state.install_failed = false;
        }
        UpdateEvent::InstallFailed => {
            state.activity = UpdateActivity::Idle;
            state.install_failed = true;
        }
        UpdateEvent::Dismissed => {
            state.install_failed = false;
            state.activity = UpdateActivity::Idle;
            state.available = None;
            state.download_pct = None;
            state.cost_deferral = None;
            state.download_failed = false;
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
            state.install_failed = false;
            state.activity = UpdateActivity::Idle;
            state.status.last_checked_at = Some(now);
            state.status.failed_with_available = false;
            state.cost_deferral = None;
            state.download_failed = false;
            match result {
                CheckOutcome::UpToDate | CheckOutcome::Empty => {
                    state.available = None;
                    if state.staged.is_none() {
                        state.status.last_check_outcome = Some(UpdateOutcome::UpToDate);
                        state.status.available_version = None;
                    }
                }
                CheckOutcome::UpdateAvailable { version, notes } => {
                    let pending_ver = state
                        .staged
                        .as_ref()
                        .and_then(|s| semver::Version::parse(&s.version).ok());
                    let replaces = semver::Version::parse(&version).is_ok_and(|feed_ver| {
                        feed_replaces_pending(pending_ver.as_ref(), &feed_ver)
                    });
                    if replaces {
                        state.status.last_check_outcome = Some(UpdateOutcome::Found);
                        state.status.available_version = Some(version.clone());
                        state.available = Some(AvailableUpdate { version, notes });
                    } else if state.staged.is_some() {
                        state.status.last_check_outcome = Some(UpdateOutcome::Staged);
                    } else {
                        state.status.last_check_outcome = Some(UpdateOutcome::UpToDate);
                    }
                }
            }
        }
        UpdateEvent::Downloaded { version } => {
            state.activity = UpdateActivity::Idle;
            state.download_pct = None;
            state.available = None;
            state.cost_deferral = None;
            state.download_failed = false;
            state.install_failed = false;
            state.status.available_version = Some(version.clone());
            state.status.last_check_outcome = Some(UpdateOutcome::Staged);
            state.status.failed_with_available = false;
            state.staged = Some(StagedUpdate { version });
        }
        UpdateEvent::CheckFailed { now } => {
            state.install_failed = false;
            state.activity = UpdateActivity::Idle;
            state.download_pct = None;
            state.cost_deferral = None;
            state.download_failed = false;
            // Feed failure does not clear staged or available
            state.status.last_checked_at = Some(now);
            state.status.last_check_outcome = Some(UpdateOutcome::Failed);
            state.status.failed_with_available = state.status.available_version.is_some();
        }
        UpdateEvent::DownloadFailed => {
            state.install_failed = false;
            state.activity = UpdateActivity::Idle;
            state.download_pct = None;
            state.download_failed = true;
        }
        UpdateEvent::DownloadDeferred { reason } => {
            state.activity = UpdateActivity::Idle;
            state.cost_deferral = Some(reason);
            state.download_failed = false;
        }
        UpdateEvent::PendingMissing => {
            state.activity = UpdateActivity::Idle;
            state.staged = None;
            state.install_failed = true;
            if state.status.last_check_outcome == Some(UpdateOutcome::Staged) {
                state.status.last_check_outcome = if state.status.available_version.is_some() {
                    Some(UpdateOutcome::Found)
                } else {
                    None
                };
            }
            state.status.failed_with_available = state.status.available_version.is_some();
        }
        UpdateEvent::ConfigInvalid => {
            state.config_valid = false;
            state.activity = UpdateActivity::Idle;
        }
    }
}

/// Pure descriptor of an update asset (filename, version, and type).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateAssetDescriptor {
    pub file_name: String,
    pub version: String,
    pub asset_type: String,
}

impl UpdateAssetDescriptor {
    pub fn new(
        file_name: impl Into<String>,
        version: impl Into<String>,
        asset_type: impl Into<String>,
    ) -> Self {
        Self {
            file_name: file_name.into(),
            version: version.into(),
            asset_type: asset_type.into(),
        }
    }
}

/// The classified strategy Velopack offered for an available update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdatePlan {
    FullOnly {
        target: UpdateAssetDescriptor,
    },
    DeltaChain {
        base: UpdateAssetDescriptor,
        deltas: Vec<UpdateAssetDescriptor>,
        target: UpdateAssetDescriptor,
    },
}

/// Classify an update into a pure `UpdatePlan`.
/// Returns `DeltaChain` only when `base` is `Some` and `deltas` is non-empty;
/// returns `FullOnly` otherwise.
pub fn classify_update_plan(
    base: Option<UpdateAssetDescriptor>,
    deltas: Vec<UpdateAssetDescriptor>,
    target: UpdateAssetDescriptor,
) -> UpdatePlan {
    match base {
        Some(base) if !deltas.is_empty() => UpdatePlan::DeltaChain {
            base,
            deltas,
            target,
        },
        _ => UpdatePlan::FullOnly { target },
    }
}

/// Format the update plan line for CLI stdout.
pub fn format_update_plan(plan: &UpdatePlan) -> String {
    match plan {
        UpdatePlan::FullOnly { target } => {
            format!(
                "--check-update: plan=full-only target={} type={} version={}",
                target.file_name, target.asset_type, target.version
            )
        }
        UpdatePlan::DeltaChain {
            base,
            deltas,
            target,
        } => {
            let delta_list: Vec<&str> = deltas.iter().map(|d| d.file_name.as_str()).collect();
            format!(
                "--check-update: plan=delta-chain base={} version={} deltas={} target={} version={}",
                base.file_name,
                base.version,
                delta_list.join(","),
                target.file_name,
                target.version
            )
        }
    }
}

/// Format the downloading asset line for CLI stdout.
pub fn format_downloading_asset(file_name: &str, asset_type: &str) -> String {
    format!(
        "--check-update: downloading asset: {} type={}",
        file_name, asset_type
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_checked_is_not_a_false_up_to_date() {
        let s = UpdateState::new();
        assert_eq!(s.display(), UpdateDisplay::NeverChecked);
        // No dead "check now": it IS actionable from the never-checked idle state.
        assert!(s.view(true).actions.can_check_now);
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
        let a = s.view(true).actions;
        assert!(a.can_download);
        assert!(a.can_dismiss);
        assert!(!a.can_install);
        assert!(a.can_check_now);
    }

    #[test]
    fn check_enabled_while_staged_and_disabled_while_in_flight() {
        let mut s = UpdateState::new();
        reduce(
            &mut s,
            UpdateEvent::Downloaded {
                version: "0.2.0".into(),
            },
        );
        assert_eq!(s.display(), UpdateDisplay::Staged);
        assert_eq!(s.activity, UpdateActivity::Idle);
        let a = s.view(true).actions;
        assert!(a.can_check_now, "check now remains enabled while parked");
        assert!(!a.can_download, "nothing unadmitted left to download");
        assert!(a.can_install, "relaunch-to-install is the staged action");
        assert!(!a.can_retry);

        // While in-flight check, check now is disabled
        reduce(&mut s, UpdateEvent::CheckStarted);
        assert!(!s.view(true).actions.can_check_now);
    }

    #[test]
    fn in_flight_check_disables_actions_and_enables_cancel() {
        let mut s = UpdateState::new();
        reduce(&mut s, UpdateEvent::CheckStarted);
        let a = s.view(true).actions;
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
        let v = s.view(true);
        assert_eq!(v.download_pct, Some(42));
        assert!(v.actions.can_cancel);
        assert!(!v.actions.can_check_now);
    }

    #[test]
    fn failed_check_enables_retry() {
        let mut s = UpdateState::new();
        reduce(&mut s, UpdateEvent::CheckFailed { now: 9 });
        assert_eq!(s.display(), UpdateDisplay::Failed);
        let a = s.view(true).actions;
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
        assert_eq!(s.view(true).available_version.as_deref(), Some("0.3.0"));
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
        assert!(!s.view(true).actions.can_download);
    }

    #[test]
    fn config_invalid_is_unavailable_with_no_actions() {
        let mut s = UpdateState::new();
        reduce(&mut s, UpdateEvent::ConfigInvalid);
        assert!(!s.config_valid);
        assert_eq!(s.display(), UpdateDisplay::Unavailable);
        let a = s.view(true).actions;
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
        assert_eq!(p.interval, CheckInterval::Day);
        assert!(p.auto_download);
    }

    #[test]
    fn frequency_picker_disabled_when_auto_check_off() {
        let mut s = UpdateState::new();
        s.prefs.auto_check = false;
        assert!(!s.view(true).actions.frequency_enabled);
    }

    #[test]
    fn last_checked_just_now_threshold_is_60s() {
        // The load-bearing boundary: 59s is still "just now", 60s flips to minutes.
        assert_eq!(
            last_checked_relative(Some(1_000), 1_000),
            "checked just now"
        );
        assert_eq!(
            last_checked_relative(Some(1_000), 1_059),
            "checked just now"
        );
        assert_eq!(
            last_checked_relative(Some(1_000), 1_060),
            "checked 1 minute ago"
        );
        assert_eq!(
            last_checked_relative(Some(1_000), 1_000 + 120),
            "checked 2 minutes ago"
        );
        assert_eq!(
            last_checked_relative(Some(1_000), 1_000 + 7_200),
            "checked 2 hours ago"
        );
        assert_eq!(
            last_checked_relative(Some(1_000), 1_000 + 86_400),
            "checked 1 day ago"
        );
    }

    #[test]
    fn last_checked_never() {
        assert_eq!(
            last_checked_relative(None, 9_999),
            "never checked for updates"
        );
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
        let json = serde_json::to_string(&s.view(true)).unwrap();
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

    #[test]
    fn classify_full_only_when_no_base() {
        let target = UpdateAssetDescriptor::new("Solstone-2.0.3-full.nupkg", "2.0.3", "Full");
        let plan = classify_update_plan(None, Vec::new(), target.clone());
        assert_eq!(plan, UpdatePlan::FullOnly { target });
    }

    #[test]
    fn classify_full_only_when_no_base_but_nonempty_deltas() {
        let delta = UpdateAssetDescriptor::new("Solstone-2.0.3-delta.nupkg", "2.0.3", "Delta");
        let target = UpdateAssetDescriptor::new("Solstone-2.0.3-full.nupkg", "2.0.3", "Full");
        let plan = classify_update_plan(None, vec![delta], target.clone());
        assert_eq!(plan, UpdatePlan::FullOnly { target });
    }

    #[test]
    fn classify_full_only_when_base_present_but_empty_deltas() {
        let base = UpdateAssetDescriptor::new("Solstone-2.0.2-full.nupkg", "2.0.2", "Full");
        let target = UpdateAssetDescriptor::new("Solstone-2.0.3-full.nupkg", "2.0.3", "Full");
        let plan = classify_update_plan(Some(base), Vec::new(), target.clone());
        assert_eq!(plan, UpdatePlan::FullOnly { target });
    }

    #[test]
    fn classify_delta_chain_single_delta() {
        let base = UpdateAssetDescriptor::new("Solstone-2.0.2-full.nupkg", "2.0.2", "Full");
        let delta = UpdateAssetDescriptor::new("Solstone-2.0.3-delta.nupkg", "2.0.3", "Delta");
        let target = UpdateAssetDescriptor::new("Solstone-2.0.3-full.nupkg", "2.0.3", "Full");
        let plan = classify_update_plan(Some(base.clone()), vec![delta.clone()], target.clone());
        assert_eq!(
            plan,
            UpdatePlan::DeltaChain {
                base,
                deltas: vec![delta],
                target,
            }
        );
    }

    #[test]
    fn classify_delta_chain_multiple_deltas_preserves_order() {
        let base = UpdateAssetDescriptor::new("Solstone-2.0.1-full.nupkg", "2.0.1", "Full");
        let d1 = UpdateAssetDescriptor::new("Solstone-2.0.2-delta.nupkg", "2.0.2", "Delta");
        let d2 = UpdateAssetDescriptor::new("Solstone-2.0.3-delta.nupkg", "2.0.3", "Delta");
        let target = UpdateAssetDescriptor::new("Solstone-2.0.3-full.nupkg", "2.0.3", "Full");
        let plan = classify_update_plan(
            Some(base.clone()),
            vec![d1.clone(), d2.clone()],
            target.clone(),
        );
        match plan {
            UpdatePlan::DeltaChain {
                base: b,
                deltas,
                target: t,
            } => {
                assert_eq!(b, base);
                assert_eq!(deltas, vec![d1, d2]);
                assert_eq!(t, target);
            }
            UpdatePlan::FullOnly { .. } => panic!("expected DeltaChain"),
        }
    }

    #[test]
    fn format_output_contains_tokens_and_exact_filenames() {
        let base = UpdateAssetDescriptor::new("Solstone-2.0.2-full.nupkg", "2.0.2", "Full");
        let delta = UpdateAssetDescriptor::new("Solstone-2.0.3-delta.nupkg", "2.0.3", "Delta");
        let target = UpdateAssetDescriptor::new("Solstone-2.0.3-full.nupkg", "2.0.3", "Full");

        let full_plan = UpdatePlan::FullOnly {
            target: target.clone(),
        };
        let full_line = format_update_plan(&full_plan);
        assert!(full_line.contains("plan=full-only"));
        assert!(full_line.contains("Solstone-2.0.3-full.nupkg"));

        let delta_plan = UpdatePlan::DeltaChain {
            base: base.clone(),
            deltas: vec![delta.clone()],
            target: target.clone(),
        };
        let delta_line = format_update_plan(&delta_plan);
        assert!(delta_line.contains("plan=delta-chain"));
        assert!(delta_line.contains("Solstone-2.0.2-full.nupkg"));
        assert!(delta_line.contains("Solstone-2.0.3-delta.nupkg"));
        assert!(delta_line.contains("Solstone-2.0.3-full.nupkg"));

        let download_line = format_downloading_asset(&delta.file_name, &delta.asset_type);
        assert!(download_line.contains("downloading asset: Solstone-2.0.3-delta.nupkg"));
        assert!(download_line.contains("type=Delta"));
    }

    #[test]
    fn check_failed_after_persisted_staged_displays_failed_not_uptodate() {
        let mut s = UpdateState::new();
        s.status.last_check_outcome = Some(UpdateOutcome::Staged);
        s.status.available_version = Some("0.3.0".into());
        s.staged = None;

        reduce(&mut s, UpdateEvent::CheckFailed { now: 20 });
        assert_eq!(s.display(), UpdateDisplay::FailedWithAvailable);
        assert!(!s.view(true).actions.can_install);
        assert!(s.view(true).actions.can_retry);
    }

    #[test]
    fn migration_runs_once_for_legacy_document() {
        let legacy_json = r#"{"prefs":{"auto_check":true,"interval":"week","auto_download":false},"status":{"last_checked_at":100,"last_check_outcome":"up_to_date","available_version":null,"failed_with_available":false}}"#;
        let doc = load_update_document(legacy_json).unwrap();
        assert!(!doc.daily_background);
        let migrated = migrate_update_document(doc);
        assert!(migrated.daily_background);
        assert_eq!(migrated.prefs.interval, CheckInterval::Day);
        assert!(migrated.prefs.auto_download);
        assert!(migrated.prefs.auto_check);
        assert!(migrated
            .explicit_choices
            .expect("migration writes an empty record")
            .is_unset());
        assert_eq!(migrated.status.last_checked_at, Some(100));
    }

    #[test]
    fn migration_sets_daily_and_auto_download_for_legacy_off() {
        let legacy_json = r#"{"prefs":{"auto_check":false,"interval":"month","auto_download":false},"status":{}}"#;
        let doc = load_update_document(legacy_json).unwrap();
        assert!(!doc.daily_background);
        let migrated = migrate_update_document(doc);
        assert!(migrated.daily_background);
        assert_eq!(migrated.prefs.interval, CheckInterval::Day);
        assert!(migrated.prefs.auto_download);
        assert!(migrated.prefs.auto_check);
        assert!(migrated
            .explicit_choices
            .expect("migration writes an empty record")
            .is_unset());
    }

    #[test]
    fn migration_preserves_status_bytes() {
        let legacy_json = r#"{"prefs":{"auto_check":true,"interval":"week","auto_download":false},"status":{"last_checked_at":555,"last_check_outcome":"failed","available_version":"1.2.3","failed_with_available":true}}"#;
        let doc = load_update_document(legacy_json).unwrap();
        let migrated = migrate_update_document(doc);
        assert_eq!(migrated.status.last_checked_at, Some(555));
        assert_eq!(
            migrated.status.last_check_outcome,
            Some(UpdateOutcome::Failed)
        );
        assert_eq!(migrated.status.available_version.as_deref(), Some("1.2.3"));
        assert!(migrated.status.failed_with_available);
    }

    #[test]
    fn owner_edit_after_migration_retains_marker_and_choices() {
        let legacy_json = r#"{"prefs":{"auto_check":false,"interval":"month","auto_download":false},"status":{"last_checked_at":9}}"#;
        let mut doc = open_update_document(load_update_document(legacy_json).unwrap());
        assert!(doc.daily_background);
        assert!(doc.explicit_choices.expect("record").is_unset());
        assert_eq!(doc.status.last_checked_at, Some(9));
        doc.set_interval(CheckInterval::Month);
        doc.set_auto_download(false);
        let migrated_again = migrate_update_document(doc.clone());
        assert!(migrated_again.daily_background);
        assert_eq!(migrated_again.explicit_choices, doc.explicit_choices);
        assert_eq!(migrated_again.prefs.interval, CheckInterval::Month);
        assert!(!migrated_again.prefs.auto_download);
        assert!(migrated_again.prefs.auto_check);
        assert!(migrated_again
            .explicit_choices
            .expect("record")
            .auto_check
            .is_none());

        let text = serde_json::to_string(&migrated_again).unwrap();
        let reloaded = open_update_document(load_update_document(&text).unwrap());
        assert_eq!(reloaded.explicit_choices, migrated_again.explicit_choices);
        assert_eq!(reloaded.prefs, migrated_again.prefs);
        assert_eq!(reloaded.status.last_checked_at, Some(9));
        assert!(reloaded.daily_background);
    }

    #[test]
    fn owner_edit_of_defaults_differs_from_untouched_and_survives_later_defaults() {
        let legacy_json =
            r#"{"prefs":{"auto_check":false,"interval":"week","auto_download":false},"status":{}}"#;
        let untouched = open_update_document(load_update_document(legacy_json).unwrap());
        let mut edited = untouched.clone();
        edited.set_auto_check(true);
        edited.set_interval(CheckInterval::Day);
        edited.set_auto_download(true);

        assert_eq!(untouched.prefs, edited.prefs);
        assert_eq!(untouched.prefs, UpdatePrefs::default());
        assert_ne!(untouched.explicit_choices, edited.explicit_choices);
        assert!(untouched.explicit_choices.expect("record").is_unset());
        let edited_choices = edited.explicit_choices.expect("record");
        assert_eq!(edited_choices.auto_check, Some(true));
        assert_eq!(edited_choices.interval, Some(CheckInterval::Day));
        assert_eq!(edited_choices.auto_download, Some(true));

        let later = UpdatePrefs {
            auto_check: false,
            interval: CheckInterval::Week,
            auto_download: false,
        };
        assert_eq!(
            untouched.explicit_choices.expect("record").resolve(later),
            later
        );
        assert_eq!(edited_choices.resolve(later), UpdatePrefs::default());

        let untouched_text = serde_json::to_string(&untouched).unwrap();
        let edited_text = serde_json::to_string(&edited).unwrap();
        assert_ne!(untouched_text, edited_text);
        let untouched_loaded = open_update_document(load_update_document(&untouched_text).unwrap());
        let edited_loaded = open_update_document(load_update_document(&edited_text).unwrap());
        assert_eq!(
            untouched_loaded.explicit_choices,
            untouched.explicit_choices
        );
        assert_eq!(edited_loaded.explicit_choices, edited.explicit_choices);
        assert_eq!(untouched_loaded.prefs, edited_loaded.prefs);
    }

    #[test]
    fn background_status_save_keeps_explicit_choices_and_adds_none() {
        let mut doc = fresh_update_document();
        doc.set_auto_download(true);
        doc.set_interval(CheckInterval::Week);
        doc.status.last_checked_at = Some(42);
        doc.status.last_check_outcome = Some(UpdateOutcome::UpToDate);

        let text = serde_json::to_string(&doc).unwrap();
        let reloaded = open_update_document(load_update_document(&text).unwrap());
        let choices = reloaded.explicit_choices.expect("record");
        assert!(choices.auto_check.is_none());
        assert_eq!(choices.auto_download, Some(true));
        assert_eq!(choices.interval, Some(CheckInterval::Week));
        assert!(reloaded.prefs.auto_check);
        assert!(reloaded.prefs.auto_download);
        assert_eq!(reloaded.prefs.interval, CheckInterval::Week);
        assert_eq!(reloaded.status.last_checked_at, Some(42));
        assert_eq!(
            reloaded.status.last_check_outcome,
            Some(UpdateOutcome::UpToDate)
        );
        assert!(reloaded.daily_background);
    }

    #[test]
    fn concrete_prefs_without_explicit_record_adopt_only_nondefaults() {
        let json = r#"{"prefs":{"auto_check":true,"interval":"week","auto_download":false},"status":{"last_checked_at":7},"daily_background":true}"#;
        let opened = open_update_document(load_update_document(json).unwrap());
        let choices = opened.explicit_choices.expect("adopted record");
        assert!(choices.auto_check.is_none());
        assert_eq!(choices.interval, Some(CheckInterval::Week));
        assert_eq!(choices.auto_download, Some(false));
        assert!(opened.prefs.auto_check);
        assert_eq!(opened.prefs.interval, CheckInterval::Week);
        assert!(!opened.prefs.auto_download);
        assert_eq!(opened.status.last_checked_at, Some(7));

        let mut saved = opened;
        saved.set_interval(CheckInterval::Day);
        let text = serde_json::to_string(&saved).unwrap();
        let reloaded = open_update_document(load_update_document(&text).unwrap());
        assert_eq!(
            reloaded.explicit_choices.expect("record").interval,
            Some(CheckInterval::Day)
        );
        assert_eq!(reloaded.prefs.interval, CheckInterval::Day);
        assert_eq!(
            reloaded.explicit_choices.expect("record").auto_download,
            Some(false)
        );
    }

    #[test]
    fn state_setter_document_keeps_choices_across_a_status_save() {
        let mut state = UpdateState::new();
        state.set_auto_check(false);
        state.status.last_checked_at = Some(3);
        let mut saved = UpdateDocument {
            prefs: state.prefs,
            status: state.status.clone(),
            daily_background: true,
            explicit_choices: Some(state.explicit_choices),
        };
        saved.status.last_check_outcome = Some(UpdateOutcome::Failed);
        let text = serde_json::to_string(&saved).unwrap();
        let opened = open_update_document(load_update_document(&text).unwrap());
        let choices = opened.explicit_choices.expect("record");
        assert_eq!(choices.auto_check, Some(false));
        assert!(choices.interval.is_none());
        assert!(choices.auto_download.is_none());
        assert!(!opened.prefs.auto_check);
        assert_eq!(opened.prefs.interval, CheckInterval::Day);
        assert!(opened.prefs.auto_download);
        assert_eq!(opened.status.last_checked_at, Some(3));
        assert_eq!(
            opened.status.last_check_outcome,
            Some(UpdateOutcome::Failed)
        );
    }

    #[test]
    fn unset_explicit_record_ignores_stale_prefs() {
        let json = r#"{"prefs":{"auto_check":false,"interval":"month","auto_download":false},"status":{},"daily_background":true,"explicit_choices":{"auto_check":true}}"#;
        let opened = open_update_document(load_update_document(json).unwrap());
        assert!(opened.prefs.auto_check);
        assert_eq!(opened.prefs.interval, CheckInterval::Day);
        assert!(opened.prefs.auto_download);
        let choices = opened.explicit_choices.expect("record");
        assert_eq!(choices.auto_check, Some(true));
        assert!(choices.interval.is_none());
        assert!(choices.auto_download.is_none());

        let stale_defaults = r#"{"prefs":{"auto_check":false,"interval":"month","auto_download":false},"status":{},"daily_background":true,"explicit_choices":{}}"#;
        let defaults = open_update_document(load_update_document(stale_defaults).unwrap());
        assert_eq!(defaults.prefs, UpdatePrefs::default());
        assert!(defaults.explicit_choices.expect("record").is_unset());
    }

    #[test]
    fn fresh_document_is_not_migration_of_garbage_json() {
        let fresh = fresh_update_document();
        assert!(fresh.daily_background);
        assert_eq!(fresh.prefs, UpdatePrefs::default());
        assert!(fresh.explicit_choices.expect("record").is_unset());
        assert_eq!(fresh.status, ReconciledUpdateStatus::default());
        assert!(load_update_document("not-json").is_err());
        let text = serde_json::to_string(&fresh).unwrap();
        assert!(text.contains("\"explicit_choices\""));
        let opened = open_update_document(load_update_document(&text).unwrap());
        assert_eq!(opened.explicit_choices, fresh.explicit_choices);
        assert_eq!(opened.prefs, fresh.prefs);
    }

    #[test]
    fn check_is_due_boundary_86399_and_86400() {
        let now = 100_000;
        let last = Some(now - 86_399);
        assert!(!check_is_due(now, last, CheckInterval::Day, true, false));
        let last_due = Some(now - 86_400);
        assert!(check_is_due(now, last_due, CheckInterval::Day, true, false));
    }

    #[test]
    fn check_is_due_not_suppressed_by_pending_staged() {
        // Staged status does not pass into check_is_due and does not suppress it
        let now = 100_000;
        let last = Some(now - 86_400);
        assert!(check_is_due(now, last, CheckInterval::Day, true, false));
    }

    #[test]
    fn check_is_due_false_when_in_flight() {
        let now = 100_000;
        let last = Some(now - 86_400);
        assert!(!check_is_due(now, last, CheckInterval::Day, true, true));
    }

    #[test]
    fn check_is_due_false_when_auto_check_off() {
        let now = 100_000;
        let last = Some(now - 86_400);
        assert!(!check_is_due(now, last, CheckInterval::Day, false, false));
    }

    #[test]
    fn check_is_due_week_at_86400_is_false() {
        let now = 100_000;
        let last = Some(now - 86_400);
        assert!(!check_is_due(now, last, CheckInterval::Week, true, false));
    }

    #[test]
    fn automatic_download_eligible_predicate_table() {
        let eligible_cost = NetworkCost {
            kind: NetworkCostKind::Unrestricted,
            roaming: false,
            background_restricted: false,
        };
        assert!(automatic_download_eligible(true, eligible_cost));
        assert!(!automatic_download_eligible(false, eligible_cost));

        for kind in [
            NetworkCostKind::Fixed,
            NetworkCostKind::Variable,
            NetworkCostKind::Unknown,
        ] {
            let cost = NetworkCost {
                kind,
                roaming: false,
                background_restricted: false,
            };
            assert!(!automatic_download_eligible(true, cost));
        }

        let roaming_cost = NetworkCost {
            kind: NetworkCostKind::Unrestricted,
            roaming: true,
            background_restricted: false,
        };
        assert!(!automatic_download_eligible(true, roaming_cost));

        let restricted_cost = NetworkCost {
            kind: NetworkCostKind::Unrestricted,
            roaming: false,
            background_restricted: true,
        };
        assert!(!automatic_download_eligible(true, restricted_cost));
    }

    #[test]
    fn explicit_download_allowed_metered_and_unknown_allowed_roaming_restricted_refused() {
        for kind in [
            NetworkCostKind::Unrestricted,
            NetworkCostKind::Fixed,
            NetworkCostKind::Variable,
            NetworkCostKind::Unknown,
        ] {
            let cost = NetworkCost {
                kind,
                roaming: false,
                background_restricted: false,
            };
            assert!(explicit_download_allowed(cost));
        }

        let roaming_cost = NetworkCost {
            kind: NetworkCostKind::Unrestricted,
            roaming: true,
            background_restricted: false,
        };
        assert!(!explicit_download_allowed(roaming_cost));

        let restricted_cost = NetworkCost {
            kind: NetworkCostKind::Unrestricted,
            roaming: false,
            background_restricted: true,
        };
        assert!(!explicit_download_allowed(restricted_cost));
    }

    #[test]
    fn cost_deferral_priority_roaming_restricted_metered_unknown() {
        // Eligible: None
        let eligible = NetworkCost {
            kind: NetworkCostKind::Unrestricted,
            roaming: false,
            background_restricted: false,
        };
        assert_eq!(cost_deferral(eligible), None);

        // Roaming takes precedence over restricted and metered
        let roaming_metered = NetworkCost {
            kind: NetworkCostKind::Fixed,
            roaming: true,
            background_restricted: true,
        };
        assert_eq!(cost_deferral(roaming_metered), Some(CostDeferral::Roaming));

        // Restricted takes precedence over metered
        let restricted_metered = NetworkCost {
            kind: NetworkCostKind::Fixed,
            roaming: false,
            background_restricted: true,
        };
        assert_eq!(
            cost_deferral(restricted_metered),
            Some(CostDeferral::Restricted)
        );

        // Metered (Fixed and Variable)
        let fixed = NetworkCost {
            kind: NetworkCostKind::Fixed,
            roaming: false,
            background_restricted: false,
        };
        assert_eq!(cost_deferral(fixed), Some(CostDeferral::Metered));

        let variable = NetworkCost {
            kind: NetworkCostKind::Variable,
            roaming: false,
            background_restricted: false,
        };
        assert_eq!(cost_deferral(variable), Some(CostDeferral::Metered));

        // Unknown
        let unknown = NetworkCost {
            kind: NetworkCostKind::Unknown,
            roaming: false,
            background_restricted: false,
        };
        assert_eq!(cost_deferral(unknown), Some(CostDeferral::Unknown));
    }

    #[test]
    fn automatic_retry_delay_899_false_and_900_true() {
        let eligible_cost = NetworkCost {
            kind: NetworkCostKind::Unrestricted,
            roaming: false,
            background_restricted: false,
        };
        let failed_at = 1_000;
        assert!(!automatic_download_should_start(
            failed_at + 899,
            true,
            eligible_cost,
            false,
            true,
            Some(failed_at),
        ));
        assert!(automatic_download_should_start(
            failed_at + 900,
            true,
            eligible_cost,
            false,
            true,
            Some(failed_at),
        ));
    }

    #[test]
    fn automatic_retry_not_a_check_and_does_not_move_last_checked_at() {
        let eligible_cost = NetworkCost {
            kind: NetworkCostKind::Unrestricted,
            roaming: false,
            background_restricted: false,
        };
        let failed_at = 1_000;
        let should_start = automatic_download_should_start(
            failed_at + 900,
            true,
            eligible_cost,
            false,
            true,
            Some(failed_at),
        );
        assert!(should_start);
        // Pure function takes no status and does not mutate checked_at
    }

    #[test]
    fn automatic_retry_deferral_without_failure_starts_immediately_when_eligible() {
        let eligible_cost = NetworkCost {
            kind: NetworkCostKind::Unrestricted,
            roaming: false,
            background_restricted: false,
        };
        assert!(automatic_download_should_start(
            500,
            true,
            eligible_cost,
            false,
            true,
            None,
        ));
    }

    #[test]
    fn feed_failure_empty_and_uptodate_preserve_staged_package() {
        let mut s = UpdateState::new();
        reduce(
            &mut s,
            UpdateEvent::Downloaded {
                version: "1.0.0".into(),
            },
        );
        assert_eq!(s.display(), UpdateDisplay::Staged);

        // CheckFailed preserves staged
        reduce(&mut s, UpdateEvent::CheckFailed { now: 50 });
        assert_eq!(s.staged.as_ref().map(|x| x.version.as_str()), Some("1.0.0"));
        assert_eq!(s.display(), UpdateDisplay::Staged);
        let view = s.view(true);
        assert_eq!(view.recovery, Some(Recovery::Check));
        assert_eq!(view.downloaded_version.as_deref(), Some("1.0.0"));
        assert_eq!(view.last_checked_at, Some(50));
        assert_eq!(s.staged.as_ref().map(|x| x.version.as_str()), Some("1.0.0"));

        // Empty preserves staged
        reduce(
            &mut s,
            UpdateEvent::CheckResult {
                now: 60,
                result: CheckOutcome::Empty,
            },
        );
        assert_eq!(s.staged.as_ref().map(|x| x.version.as_str()), Some("1.0.0"));
        assert_eq!(s.display(), UpdateDisplay::Staged);

        // UpToDate preserves staged
        reduce(
            &mut s,
            UpdateEvent::CheckResult {
                now: 70,
                result: CheckOutcome::UpToDate,
            },
        );
        assert_eq!(s.staged.as_ref().map(|x| x.version.as_str()), Some("1.0.0"));
        assert_eq!(s.display(), UpdateDisplay::Staged);
    }

    #[test]
    fn feed_failure_moves_last_checked_at_download_failure_does_not() {
        let mut s = UpdateState::new();
        reduce(&mut s, UpdateEvent::CheckFailed { now: 123 });
        assert_eq!(s.status.last_checked_at, Some(123));

        reduce(&mut s, UpdateEvent::DownloadFailed);
        assert_eq!(s.status.last_checked_at, Some(123));
        assert!(s.download_failed);
    }

    #[test]
    fn download_deferred_transient_does_not_alter_status() {
        let mut s = UpdateState::new();
        s.status.last_checked_at = Some(10);
        reduce(
            &mut s,
            UpdateEvent::DownloadDeferred {
                reason: CostDeferral::Metered,
            },
        );
        assert_eq!(s.status.last_checked_at, Some(10));
        assert_eq!(s.cost_deferral, Some(CostDeferral::Metered));
    }

    #[test]
    fn pending_missing_clears_staged_without_moving_last_checked_at_and_not_uptodate() {
        let mut s = UpdateState::new();
        reduce(
            &mut s,
            UpdateEvent::Downloaded {
                version: "1.0.0".into(),
            },
        );
        s.status.last_checked_at = Some(88);
        reduce(&mut s, UpdateEvent::PendingMissing);
        assert!(s.staged.is_none());
        assert_eq!(s.status.last_checked_at, Some(88));
        assert_ne!(s.status.last_check_outcome, Some(UpdateOutcome::UpToDate));
    }

    #[test]
    fn feed_replaces_pending_newer_true_same_or_older_false() {
        let v1_0_0 = semver::Version::parse("1.0.0").unwrap();
        let v1_1_0 = semver::Version::parse("1.1.0").unwrap();
        let v0_9_0 = semver::Version::parse("0.9.0").unwrap();

        assert!(feed_replaces_pending(None, &v1_0_0));
        assert!(feed_replaces_pending(Some(&v1_0_0), &v1_1_0));
        assert!(!feed_replaces_pending(Some(&v1_0_0), &v1_0_0));
        assert!(!feed_replaces_pending(Some(&v1_0_0), &v0_9_0));
    }
    #[test]
    fn download_failure_uses_failure_view_without_changing_feed_time() {
        let mut state = UpdateState::new();
        reduce(
            &mut state,
            UpdateEvent::CheckResult {
                now: 123,
                result: CheckOutcome::UpdateAvailable {
                    version: "2.0.34".into(),
                    notes: None,
                },
            },
        );
        reduce(&mut state, UpdateEvent::DownloadStarted);
        reduce(&mut state, UpdateEvent::DownloadFailed);
        let view = state.view(true);
        assert_eq!(view.display, UpdateDisplay::FailedWithAvailable);
        assert_eq!(view.recovery, Some(Recovery::Download));
        assert_eq!(view.available_version.as_deref(), Some("2.0.34"));
        assert_eq!(view.last_checked_at, Some(123));
        assert!(view.actions.can_download);
        reduce(
            &mut state,
            UpdateEvent::DownloadDeferred {
                reason: CostDeferral::Metered,
            },
        );
        assert_eq!(state.view(true).display, UpdateDisplay::Available);
        assert_eq!(state.view(true).recovery, None);
    }

    #[test]
    fn failed_install_preserves_pending_identity_and_install_action() {
        let mut state = UpdateState::new();
        state.status.last_checked_at = Some(123);
        reduce(
            &mut state,
            UpdateEvent::Downloaded {
                version: "2.0.34".into(),
            },
        );
        reduce(&mut state, UpdateEvent::InstallStarted);
        reduce(&mut state, UpdateEvent::InstallFailed);
        let view = state.view(true);
        assert_eq!(view.display, UpdateDisplay::Staged);
        assert_eq!(view.recovery, Some(Recovery::Install));
        assert_eq!(view.downloaded_version.as_deref(), Some("2.0.34"));
        assert!(view.actions.can_install);
        assert_eq!(view.last_checked_at, Some(123));
        reduce(&mut state, UpdateEvent::PendingMissing);
        assert_eq!(state.view(true).display, UpdateDisplay::Failed);
        assert!(!state.view(true).actions.can_install);
    }
    #[test]
    fn recovery_or_dismissal_clears_the_previous_install_failure() {
        let mut state = UpdateState::new();
        reduce(&mut state, UpdateEvent::PendingMissing);
        assert_eq!(state.view(true).recovery, Some(Recovery::Install));
        reduce(
            &mut state,
            UpdateEvent::CheckResult {
                now: 123,
                result: CheckOutcome::UpToDate,
            },
        );
        assert_eq!(state.view(true).display, UpdateDisplay::UpToDate);
        assert_eq!(state.view(true).recovery, None);
        reduce(&mut state, UpdateEvent::InstallFailed);
        reduce(&mut state, UpdateEvent::Dismissed);
        assert_eq!(state.view(true).recovery, None);
        reduce(&mut state, UpdateEvent::InstallFailed);
        reduce(&mut state, UpdateEvent::DownloadFailed);
        assert_eq!(state.view(true).recovery, Some(Recovery::Download));
    }
}
