// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! In-app updater driver (platform side).
//!
//! Owns the `velopack::UpdateManager` I/O plus an owned background-check timer,
//! and drives the pure honest reducer in `observer-update`. The split follows the
//! DAG rule: the pure tier decides *what the state is* and *which controls are
//! actionable*; this module performs the blocking Velopack calls off the UI
//! thread and feeds their results in as [`UpdateEvent`]s, then pushes the
//! recomputed honest snapshot to the webview over `update://changed`. Update
//! state is **earned from a real Velopack result, never optimistically pre-set** —
//! the same discipline as the capture engine's health.
//!
//! Cancellation: Velopack 1.2.0 exposes no cancel token for check/download, so we
//! deliberately do NOT surface a cancel control — an in-flight check/download
//! shows progress and runs to completion. (A future Velopack with cancellation
//! would light up `UpdateActions::can_cancel`, which the pure model already
//! computes; the webview simply renders no cancel button today.)
//!
//! Privacy (Article 8): our custom [`R2FeedSource`] performs a bare, query-free
//! first-party manifest GET to the R2 feed: no app version, no app id, no staging
//! id. Package downloads still request package files by filename from the same
//! first-party feed host, with same-origin enforcement in the source. Velopack's
//! manager remains in charge of version comparison, delta selection, checksum,
//! staging, pending-restart, and relaunch.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use observer_update::{
    reduce, CheckInterval, CheckOutcome, ReconciledUpdateStatus, UpdateActivity, UpdateEvent,
    UpdatePrefs, UpdateState, UpdateView,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use velopack::{UpdateCheck, UpdateInfo, UpdateManager, UpdateOptions, VelopackAsset};

use crate::update_feed::R2FeedSource;

/// The privacy-clean R2 update feed — a plain static GET surface, no identifiers,
/// no analytics (Article 8). Channel `win` => the client GETs
/// `{FEED_URL}/releases.win.json`.
const FEED_URL: &str = "https://updates.solstone.app/solstone-windows";
const CHANNEL: &str = "win";
/// The updater event the webview subscribes to (honest snapshot push).
const UPDATE_EVENT: &str = "update://changed";
/// How often the timer wakes to evaluate whether a scheduled check is due. Cheap;
/// the real cadence is `prefs.interval`, gated on `last_checked_at`.
const TIMER_TICK: Duration = Duration::from_secs(60);

/// The durable parts persisted to `update.json` (next to `pairing.json`).
#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedUpdate {
    #[serde(default)]
    prefs: UpdatePrefs,
    #[serde(default)]
    status: ReconciledUpdateStatus,
}

/// The reducer state plus the live Velopack objects the pure tier can't hold.
struct Runtime {
    state: UpdateState,
    /// The live update object for the currently-available release (to download).
    available_info: Option<UpdateInfo>,
    /// The staged asset pending relaunch (to apply).
    staged_asset: Option<VelopackAsset>,
}

struct Inner {
    rt: Mutex<Runtime>,
    /// `None` when the app is not Velopack-installed (e.g. a dev tree) — surfaced
    /// honestly as the `Unavailable` display, never a fake "up to date".
    manager: Option<UpdateManager>,
    app: AppHandle,
    state_path: PathBuf,
    /// Serializes the one in-flight Velopack op (check/download) against the timer.
    busy: AtomicBool,
}

/// Cheaply-clonable handle shared by the IPC commands and the background timer.
#[derive(Clone)]
pub struct UpdateController {
    inner: Arc<Inner>,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Neutralize velopack's per-install staging identifier so no device/user
/// identifier is minted or persisted (Article 8). velopack's manager reads a
/// per-install staging UUID from `.betaId`, minting a random `Uuid::new_v4()`
/// when absent, and threads it to the source as `staged_user_id`. Our
/// `R2FeedSource` ignores that value so it never reaches the wire, but we still
/// pre-seed `.betaId` **empty** at boot (overwriting any prior UUID) so no stable
/// per-install identifier is minted/persisted on disk and the staged-rollout id
/// stays unused. Pinned to the `velopack = "=1.2.0"` per-user layout:
/// `%LocalAppData%\Solstone\packages`, which is `data_root/packages` (our
/// `local_data_root()` is that same root).
fn neutralize_staging_id(data_root: &Path) {
    let beta_id = data_root.join("packages").join(".betaId");
    if let Some(dir) = beta_id.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            tracing::warn!(
                target: "update",
                step = "neutralize_staging_id_dir",
                error = %e,
                "updater staging id neutralization failed"
            );
            return;
        }
    }
    if let Err(e) = std::fs::write(&beta_id, "") {
        tracing::warn!(
            target: "update",
            step = "neutralize_staging_id_write",
            error = %e,
            "updater staging id neutralization failed"
        );
    }
}

/// Construct the Velopack `UpdateManager` over our R2 feed. `None` when the app is
/// not Velopack-installed (e.g. a dev tree), surfaced honestly as `Unavailable`.
fn build_manager() -> Option<UpdateManager> {
    let opts = UpdateOptions {
        // Explicit so feed resolution is deterministic (`releases.win.json`),
        // even though the build's default channel is already `win`.
        ExplicitChannel: Some(CHANNEL.to_string()),
        ..Default::default()
    };
    match UpdateManager::new(R2FeedSource::new(FEED_URL), Some(opts), None) {
        Ok(m) => Some(m),
        Err(e) => {
            tracing::warn!(
                target: "update",
                step = "build_manager",
                error = %e,
                "updater unavailable"
            );
            None
        }
    }
}

/// Headless check + stage of an update (`--check-update`) — readies an update
/// without the GUI: checks the feed, and if a newer version is available downloads
/// it (full or delta) and stages it for the next launch. Apply it with
/// `--apply-update`. Neutralizes the staging id first (Article 8), same as the GUI
/// boot. Enables unattended update + the build-box end-to-end delta validation.
pub fn check_update_cli() -> std::process::ExitCode {
    use std::process::ExitCode;
    // Article 8: strip velopack's per-install staging UUID before the check.
    neutralize_staging_id(&platform_win::local_data_root());
    let Some(manager) = build_manager() else {
        eprintln!("--check-update: updater unavailable (not installed via Velopack?)");
        return ExitCode::FAILURE;
    };
    match manager.check_for_updates() {
        Ok(UpdateCheck::UpdateAvailable(info)) => {
            let v = info.TargetFullRelease.Version.clone();
            println!("--check-update: update available: {v}; downloading...");
            if let Err(e) = manager.download_updates(&info, None) {
                eprintln!("--check-update: download failed: {e}");
                return ExitCode::FAILURE;
            }
            println!("--check-update: downloaded + staged {v} (apply with --apply-update)");
            ExitCode::SUCCESS
        }
        Ok(UpdateCheck::NoUpdateAvailable) => {
            println!("--check-update: up to date");
            ExitCode::SUCCESS
        }
        Ok(UpdateCheck::RemoteIsEmpty) => {
            println!("--check-update: remote feed is empty");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("--check-update: check failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Headless apply of a staged update — the CLI analog of the in-app
/// relaunch-to-install (`--apply-update`). Applies the pending-restart package via
/// Velopack (which relaunches the app), or exits nonzero when nothing is staged.
/// Enables unattended apply and the build-box end-to-end delta validation.
pub fn apply_pending_cli() -> std::process::ExitCode {
    use std::process::ExitCode;
    let Some(manager) = build_manager() else {
        eprintln!("--apply-update: updater unavailable (not installed via Velopack?)");
        return ExitCode::FAILURE;
    };
    match manager.get_update_pending_restart() {
        Some(asset) => {
            eprintln!(
                "--apply-update: applying staged {} and relaunching…",
                asset.Version
            );
            if let Err(e) = manager.apply_updates_and_restart(&asset) {
                eprintln!("--apply-update: apply failed: {e}");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS // typically unreached: the process is replaced.
        }
        None => {
            eprintln!("--apply-update: no staged update pending");
            ExitCode::FAILURE
        }
    }
}

impl UpdateController {
    /// Build the controller: construct the `UpdateManager` (`None` if the app is
    /// not Velopack-installed), load persisted prefs+status, and rehydrate any
    /// staged-pending-restart asset from Velopack (earned, not persisted).
    pub fn new(app: AppHandle, state_path: PathBuf) -> Self {
        // Article 8: strip velopack's per-install staging UUID before any check.
        if let Some(root) = state_path.parent() {
            neutralize_staging_id(root);
        }

        let manager = build_manager();

        let mut state = UpdateState::new();
        if let Ok(text) = std::fs::read_to_string(&state_path) {
            if let Ok(p) = serde_json::from_str::<PersistedUpdate>(&text) {
                state.prefs = p.prefs;
                state.status = p.status;
            }
        }
        state.config_valid = manager.is_some();

        let mut staged_asset = None;
        if let Some(m) = &manager {
            if let Some(asset) = m.get_update_pending_restart() {
                reduce(
                    &mut state,
                    UpdateEvent::Downloaded {
                        version: asset.Version.clone(),
                    },
                );
                staged_asset = Some(asset);
            }
        }

        let inner = Arc::new(Inner {
            rt: Mutex::new(Runtime {
                state,
                available_info: None,
                staged_asset,
            }),
            manager,
            app,
            state_path,
            busy: AtomicBool::new(false),
        });
        let ctrl = Self { inner };
        ctrl.persist();
        ctrl
    }

    /// The current honest snapshot (for the initial render on Settings open).
    pub fn view(&self) -> UpdateView {
        self.inner.rt.lock().expect("update rt").state.view()
    }

    fn emit(&self) {
        let view = self.view();
        let _ = self.inner.app.emit(UPDATE_EVENT, &view);
    }

    fn persist(&self) {
        let p = {
            let rt = self.inner.rt.lock().expect("update rt");
            PersistedUpdate {
                prefs: rt.state.prefs,
                status: rt.state.status.clone(),
            }
        };
        match serde_json::to_string_pretty(&p) {
            Ok(text) => {
                if let Some(dir) = self.inner.state_path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                if let Err(e) = std::fs::write(&self.inner.state_path, text) {
                    tracing::warn!(
                        target: "update",
                        path = %self.inner.state_path.display(),
                        error = %e,
                        "persist failed"
                    );
                }
            }
            Err(e) => tracing::warn!(
                target: "update",
                error = %e,
                "serialize failed"
            ),
        }
    }

    /// Reduce a durable-affecting event, persist, and push the snapshot.
    fn apply(&self, event: UpdateEvent) {
        {
            let mut rt = self.inner.rt.lock().expect("update rt");
            reduce(&mut rt.state, event);
        }
        self.persist();
        self.emit();
    }

    /// Reduce a transient event (activity/progress only) and push — no disk write.
    fn signal(&self, event: UpdateEvent) {
        {
            let mut rt = self.inner.rt.lock().expect("update rt");
            reduce(&mut rt.state, event);
        }
        self.emit();
    }

    fn try_begin(&self) -> bool {
        self.inner
            .busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
    fn end(&self) {
        self.inner.busy.store(false, Ordering::SeqCst);
    }

    // ── Intents ──────────────────────────────────────────────────────────────

    /// Start a check (manual or scheduled). No-op when there is no manager, an op
    /// is already in flight, or a staged update is parked (the staged-blocks-
    /// recheck rule the actionability already enforces in the UI).
    pub fn check(&self) {
        let manager = match &self.inner.manager {
            Some(m) => m.clone(),
            None => return,
        };
        if self
            .inner
            .rt
            .lock()
            .expect("update rt")
            .staged_asset
            .is_some()
        {
            return;
        }
        if !self.try_begin() {
            return;
        }
        self.signal(UpdateEvent::CheckStarted);

        let ctrl = self.clone();
        std::thread::spawn(move || match manager.check_for_updates() {
            Ok(UpdateCheck::UpdateAvailable(info)) => {
                let version = info.TargetFullRelease.Version.clone();
                let notes = non_empty(&info.TargetFullRelease.NotesMarkdown);
                ctrl.inner.rt.lock().expect("update rt").available_info = Some(*info);
                tracing::info!(
                    target: "update",
                    operation = "check",
                    result = "available",
                    version = %version,
                    "update check result"
                );
                ctrl.apply(UpdateEvent::CheckResult {
                    now: now_secs(),
                    result: CheckOutcome::UpdateAvailable { version, notes },
                });
                ctrl.end();
                let auto = ctrl
                    .inner
                    .rt
                    .lock()
                    .expect("update rt")
                    .state
                    .prefs
                    .auto_download;
                if auto {
                    ctrl.download();
                }
            }
            Ok(UpdateCheck::NoUpdateAvailable) => {
                ctrl.inner.rt.lock().expect("update rt").available_info = None;
                tracing::info!(
                    target: "update",
                    operation = "check",
                    result = "up_to_date",
                    "update check result"
                );
                ctrl.apply(UpdateEvent::CheckResult {
                    now: now_secs(),
                    result: CheckOutcome::UpToDate,
                });
                ctrl.end();
            }
            Ok(UpdateCheck::RemoteIsEmpty) => {
                ctrl.inner.rt.lock().expect("update rt").available_info = None;
                tracing::info!(
                    target: "update",
                    operation = "check",
                    result = "empty",
                    "update check result"
                );
                ctrl.apply(UpdateEvent::CheckResult {
                    now: now_secs(),
                    result: CheckOutcome::Empty,
                });
                ctrl.end();
            }
            Err(e) => {
                tracing::warn!(
                    target: "update",
                    operation = "check",
                    error = %e,
                    "update check failed"
                );
                ctrl.apply(UpdateEvent::CheckFailed { now: now_secs() });
                ctrl.end();
            }
        });
    }

    /// Download the currently-available update (full or delta — Velopack chooses).
    pub fn download(&self) {
        let manager = match &self.inner.manager {
            Some(m) => m.clone(),
            None => return,
        };
        let info = {
            let rt = self.inner.rt.lock().expect("update rt");
            match &rt.available_info {
                Some(i) => i.clone(),
                None => return,
            }
        };
        if !self.try_begin() {
            return;
        }
        self.signal(UpdateEvent::DownloadStarted);

        let (tx, rx) = mpsc::channel::<i16>();
        // Progress drainer — transient emits only (no disk write per percent).
        let ctrl_p = self.clone();
        std::thread::spawn(move || {
            while let Ok(pct) = rx.recv() {
                if pct >= 0 {
                    ctrl_p.signal(UpdateEvent::DownloadProgress(pct.min(100) as u8));
                }
            }
        });
        // Downloader.
        let ctrl = self.clone();
        std::thread::spawn(move || {
            match manager.download_updates(&info, Some(tx)) {
                Ok(()) => {
                    let staged = manager
                        .get_update_pending_restart()
                        .unwrap_or_else(|| info.TargetFullRelease.clone());
                    let version = staged.Version.clone();
                    ctrl.inner.rt.lock().expect("update rt").staged_asset = Some(staged);
                    tracing::info!(
                        target: "update",
                        operation = "download",
                        result = "success",
                        version = %version,
                        "update download result"
                    );
                    ctrl.apply(UpdateEvent::Downloaded { version });
                }
                Err(e) => {
                    tracing::warn!(
                        target: "update",
                        operation = "download",
                        error = %e,
                        "update download failed"
                    );
                    ctrl.apply(UpdateEvent::CheckFailed { now: now_secs() });
                }
            }
            ctrl.end();
        });
    }

    /// Apply the staged update and relaunch into it. Does not return on success.
    pub fn install(&self) {
        let manager = match &self.inner.manager {
            Some(m) => m.clone(),
            None => return,
        };
        let asset = {
            let rt = self.inner.rt.lock().expect("update rt");
            match &rt.staged_asset {
                Some(a) => a.clone(),
                None => return,
            }
        };
        self.signal(UpdateEvent::InstallStarted);
        tracing::info!(
            target: "update",
            operation = "apply_restart",
            result = "install_started",
            version = %asset.Version,
            "update apply result"
        );
        let ctrl = self.clone();
        std::thread::spawn(move || {
            if let Err(e) = manager.apply_updates_and_restart(&asset) {
                tracing::warn!(
                    target: "update",
                    operation = "apply_restart",
                    error = %e,
                    "update apply failed"
                );
                // Relaunch did not happen — fall back to the honest staged state.
                ctrl.apply(UpdateEvent::Downloaded {
                    version: asset.Version.clone(),
                });
            }
        });
    }

    pub fn dismiss(&self) {
        self.inner.rt.lock().expect("update rt").available_info = None;
        self.apply(UpdateEvent::Dismissed);
    }

    pub fn set_auto_check(&self, on: bool) {
        self.inner
            .rt
            .lock()
            .expect("update rt")
            .state
            .prefs
            .auto_check = on;
        self.persist();
        self.emit();
        // Turning auto-check on may make a check immediately due.
        if on {
            self.scheduled_check_if_due();
        }
    }

    pub fn set_auto_download(&self, on: bool) {
        self.inner
            .rt
            .lock()
            .expect("update rt")
            .state
            .prefs
            .auto_download = on;
        self.persist();
        self.emit();
    }

    pub fn set_interval(&self, interval: CheckInterval) {
        self.inner
            .rt
            .lock()
            .expect("update rt")
            .state
            .prefs
            .interval = interval;
        self.persist();
        self.emit();
    }

    /// Fire a scheduled check when due per prefs (reads live state each call, so
    /// pref changes + the persisted `last_checked_at` are honored across restarts).
    fn scheduled_check_if_due(&self) {
        let due = {
            let rt = self.inner.rt.lock().expect("update rt");
            let s = &rt.state;
            if !s.config_valid
                || !s.prefs.auto_check
                || rt.staged_asset.is_some()
                || !matches!(s.activity, UpdateActivity::Idle)
            {
                false
            } else {
                match s.status.last_checked_at {
                    None => true,
                    Some(t) => now_secs().saturating_sub(t) >= s.prefs.interval.secs(),
                }
            }
        };
        if due {
            self.check();
        }
    }

    /// Spawn the background-check timer: wake every [`TIMER_TICK`], fire a
    /// scheduled check when due.
    pub fn spawn_timer(&self) {
        let ctrl = self.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(TIMER_TICK).await;
                ctrl.scheduled_check_if_due();
            }
        });
    }
}
