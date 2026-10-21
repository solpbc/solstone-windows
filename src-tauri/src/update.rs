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
//! first-party feed host, with same-origin enforcement in the source. This client
//! always downloads the full asset and admits the detached signature before staging.
//! Velopack compares versions, checks the feed checksum, promotes the package,
//! and starts the installer. This module admits the cache before offering apply.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use observer_update::admission::{
    admit_cached_package, check_plain_filename, retire_asset, scan_authenticated_packages,
    Admission, AuthenticatedPackage,
};
use observer_update::{
    classify_update_plan, format_update_plan, reduce, CheckInterval, CheckOutcome,
    ReconciledUpdateStatus, UpdateActivity, UpdateAssetDescriptor, UpdateEvent, UpdateOutcome,
    UpdatePrefs, UpdateState, UpdateView,
};
use pl_transport_win::release_signature::ProductionReleaseVerifier;
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

/// Keep the manager-selected cache's staging id empty before any update check.
/// This includes portable and fallback caches. The source ignores the id on the wire.
fn neutralize_staging_id(packages_dir: &Path) -> std::io::Result<()> {
    let beta_id = packages_dir.join(".betaId");
    if let Some(dir) = beta_id.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            tracing::warn!(
                target: "update",
                step = "neutralize_staging_id_dir",
                error = %e,
                "updater staging id neutralization failed"
            );
            return Err(e);
        }
    }
    let reset = (|| -> std::io::Result<()> {
        match std::fs::remove_file(&beta_id) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        // Replace the directory entry without truncating a preexisting link target.
        std::fs::File::create_new(&beta_id)?;
        Ok(())
    })();
    if let Err(e) = reset {
        tracing::warn!(
            target: "update",
            step = "neutralize_staging_id_write",
            error = %e,
            "updater staging id neutralization failed"
        );
        return Err(e);
    }
    Ok(())
}

fn prepare_manager(manager: UpdateManager) -> Result<UpdateManager, velopack::Error> {
    neutralize_staging_id(&manager.get_packages_dir())?;
    Ok(manager)
}

/// Construct the Velopack `UpdateManager` over the given feed URL. `None` when the app is
/// not Velopack-installed (e.g. a dev tree), surfaced honestly as `Unavailable`.
fn build_manager_with_feed(feed_url: &str) -> Option<UpdateManager> {
    let opts = UpdateOptions {
        // Explicit so feed resolution is deterministic (`releases.win.json`),
        // even though the build's default channel is already `win`.
        ExplicitChannel: Some(CHANNEL.to_string()),
        MaximumDeltasBeforeFallback: -1,
        ..Default::default()
    };
    match UpdateManager::new(R2FeedSource::new(feed_url), Some(opts), None)
        .and_then(prepare_manager)
    {
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

fn build_manager() -> Option<UpdateManager> {
    build_manager_with_feed(FEED_URL)
}

fn force_full_package(info: &mut UpdateInfo) {
    info.BaseRelease = None;
    info.DeltasToTarget.clear();
}

fn admit_named_package(
    dir: &Path,
    file_name: &str,
    expected_version: &str,
    verifier: &impl observer_update::admission::PackageVerifier,
) -> Admission {
    if check_plain_filename(file_name).is_err() {
        return Admission::Refuse;
    }
    match admit_cached_package(&dir.join(file_name), verifier) {
        Ok(package) if package.identity.version.to_string() == expected_version => Admission::Ready,
        Ok(_) => Admission::Refuse,
        Err(reason) => reason,
    }
}

fn stage_full_package(
    manager: &UpdateManager,
    info: &UpdateInfo,
    progress: Option<mpsc::Sender<i16>>,
) -> Result<VelopackAsset, String> {
    stage_full_package_with_verifier(manager, info, progress, &ProductionReleaseVerifier)
}

fn stage_full_package_with_verifier(
    manager: &UpdateManager,
    info: &UpdateInfo,
    progress: Option<mpsc::Sender<i16>>,
    verifier: &impl observer_update::admission::PackageVerifier,
) -> Result<VelopackAsset, String> {
    let packages = manager.get_packages_dir();
    let dir = packages.as_path();
    let mut forced = info.clone();
    force_full_package(&mut forced);

    let target = forced.TargetFullRelease.clone();
    let file_name = &target.FileName;
    check_plain_filename(file_name).map_err(str::to_owned)?;
    if !observer_update::admission::is_full_asset_type(&target.Type) {
        return Err("the update feed did not name a full package".to_string());
    }
    let version = &target.Version;
    let nupkg_path = dir.join(file_name);

    if nupkg_path.exists() {
        match admit_named_package(dir, file_name, version, verifier) {
            Admission::Ready => return Ok(target),
            Admission::Refuse | Admission::NotReady => {
                retire_asset(dir, file_name);
                return Err(format!(
                    "cached package for {file_name} was not admitted and was retired"
                ));
            }
        }
    }

    if let Err(e) = manager.download_updates(&forced, progress) {
        retire_asset(dir, file_name);
        return Err(format!("download_updates failed: {e}"));
    }

    match admit_named_package(dir, file_name, version, verifier) {
        Admission::Ready => Ok(target),
        Admission::Refuse => {
            retire_asset(dir, file_name);
            Err("package refused on re-admission".to_string())
        }
        Admission::NotReady => {
            Err("package not ready after sidecar write (sidecar missing or unreadable)".to_string())
        }
    }
}

pub struct AdmittedPackage {
    pub asset: VelopackAsset,
    pub package: AuthenticatedPackage,
}

fn manager_installed_version(manager: &UpdateManager) -> Option<semver::Version> {
    semver::Version::parse(&manager.get_current_version_as_string()).ok()
}

fn into_admitted(package: AuthenticatedPackage) -> Option<AdmittedPackage> {
    let name = package.path.file_name()?.to_str()?.to_string();
    let asset = VelopackAsset {
        PackageId: package.identity.id.clone(),
        Version: package.identity.version.to_string(),
        Type: "Full".to_string(),
        FileName: name,
        ..Default::default()
    };
    Some(AdmittedPackage { asset, package })
}

pub fn select_admitted(manager: &UpdateManager) -> Option<AdmittedPackage> {
    let installed = manager_installed_version(manager)?;
    scan_authenticated_packages(
        &manager.get_packages_dir(),
        &installed,
        &ProductionReleaseVerifier,
    )
    .selected
    .and_then(into_admitted)
}

#[derive(Debug)]
pub enum ApplyOutcome {
    NothingPending,
    Failed(String),
    Started,
}

fn package_still_held(admitted: &AdmittedPackage) -> bool {
    if !admitted
        .package
        .held
        .identity_still_at(&admitted.package.path)
    {
        return false;
    }
    #[cfg(windows)]
    if !pl_transport_win::release_signature::windows_path_matches_held(
        &admitted.package.path,
        &admitted.package.held.file,
    ) {
        return false;
    }
    true
}

fn spawn_admitted(
    manager: &UpdateManager,
    admitted: AdmittedPackage,
    quiesce: impl FnOnce(),
    resume: impl FnOnce(),
) -> Result<(), String> {
    if admitted.package.path != manager.get_packages_dir().join(&admitted.asset.FileName) {
        return Err("admitted package differs from the installer's package path".to_string());
    }
    if !package_still_held(&admitted) {
        return Err("staged package changed before apply".to_string());
    }
    quiesce();
    let result = if package_still_held(&admitted) {
        manager
            .wait_exit_then_apply_updates(&admitted.asset, false, true, Vec::<String>::new())
            .map_err(|e| format!("apply failed: {e}"))
    } else {
        Err("staged package changed during quiesce".to_string())
    };
    // The read handle lives through helper spawn, not the helper's later reopen after exit.
    drop(admitted);
    if result.is_err() {
        resume();
    }
    result
}

fn apply_from_cache(
    manager: &UpdateManager,
    verifier: &impl observer_update::admission::PackageVerifier,
    quiesce: impl FnOnce(),
    resume: impl FnOnce(),
) -> ApplyOutcome {
    let Some(installed) = manager_installed_version(manager) else {
        return ApplyOutcome::Failed("installed version is invalid".to_string());
    };
    let scan = scan_authenticated_packages(&manager.get_packages_dir(), &installed, verifier);
    let Some(admitted) = scan.selected.and_then(into_admitted) else {
        return if scan.rejected {
            ApplyOutcome::Failed("cached update could not be authenticated".to_string())
        } else {
            ApplyOutcome::NothingPending
        };
    };
    match spawn_admitted(manager, admitted, quiesce, resume) {
        Ok(()) => ApplyOutcome::Started,
        Err(error) => ApplyOutcome::Failed(error),
    }
}

pub fn apply_verified_pending(feed_url: &str) -> ApplyOutcome {
    let Some(manager) = build_manager_with_feed(feed_url) else {
        return ApplyOutcome::Failed(
            "updater unavailable (not installed via Velopack?)".to_string(),
        );
    };
    apply_from_cache(
        &manager,
        &ProductionReleaseVerifier,
        || {
            #[cfg(feature = "browser-host")]
            crate::browser::quiesce_before_apply();
        },
        || {
            #[cfg(feature = "browser-host")]
            crate::browser::resume_after_update();
        },
    )
}

pub fn startup_apply_verified() -> ApplyOutcome {
    apply_verified_pending(FEED_URL)
}

/// Extract `--update-feed <url>` from CLI args for `--check-update` and `--apply-update`.
/// Rejects missing, empty, equals-form (`--update-feed=...`), and query-string URLs.
fn parse_update_feed_arg(args: &[String]) -> Result<Option<&str>, &'static str> {
    let mut i = 0;
    while i < args.len() {
        if args[i].starts_with("--update-feed=") {
            return Err("--update-feed does not accept '=' form; pass '--update-feed <url>' as separate arguments");
        }
        if args[i] == "--update-feed" {
            let Some(val) = args.get(i + 1) else {
                return Err("--update-feed requires a URL argument");
            };
            let trimmed = val.trim();
            if trimmed.is_empty() {
                return Err("--update-feed URL cannot be empty");
            }
            if trimmed.contains('?') {
                return Err("--update-feed URL must not contain query strings");
            }
            return Ok(Some(trimmed));
        }
        i += 1;
    }
    Ok(None)
}

/// Headless check + stage of an update (`--check-update`) — readies an update
/// without the GUI: checks the feed, and if a newer version is available downloads
/// the full package only and admits the detached signature before staging it for
/// the next launch. Apply it with `--apply-update`. Neutralizes the staging id first
/// (Article 8), same as the GUI boot.
pub fn check_update_cli(args: &[String]) -> std::process::ExitCode {
    use std::process::ExitCode;
    let feed_override = match parse_update_feed_arg(args) {
        Ok(f) => f,
        Err(msg) => {
            eprintln!("--check-update: {msg}");
            return ExitCode::FAILURE;
        }
    };
    let feed_url = feed_override.unwrap_or(FEED_URL);
    let Some(manager) = build_manager_with_feed(feed_url) else {
        eprintln!("--check-update: updater unavailable (not installed via Velopack?)");
        return ExitCode::FAILURE;
    };
    match manager.check_for_updates() {
        Ok(UpdateCheck::UpdateAvailable(mut info)) => {
            force_full_package(&mut info);
            let base = info
                .BaseRelease
                .as_ref()
                .map(|a| UpdateAssetDescriptor::new(&a.FileName, a.Version.to_string(), &a.Type));
            let deltas = info
                .DeltasToTarget
                .iter()
                .map(|a| UpdateAssetDescriptor::new(&a.FileName, a.Version.to_string(), &a.Type))
                .collect();
            let target = UpdateAssetDescriptor::new(
                &info.TargetFullRelease.FileName,
                info.TargetFullRelease.Version.to_string(),
                &info.TargetFullRelease.Type,
            );
            let plan = classify_update_plan(base, deltas, target);
            println!("{}", format_update_plan(&plan));

            let v = info.TargetFullRelease.Version.clone();
            println!("--check-update: update available: {v}; downloading...");
            match stage_full_package(&manager, &info, None) {
                Ok(_staged) => {
                    println!("--check-update: downloaded + staged {v} (apply with --apply-update)");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("--check-update: download failed: {e}");
                    ExitCode::FAILURE
                }
            }
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

/// Headless apply of an admitted staged update — the CLI analog of the in-app
/// relaunch-to-install (`--apply-update`). Applies the verified pending package via
/// Velopack (which relaunches the app), or exits nonzero when nothing is staged.
pub fn apply_pending_cli(args: &[String]) -> std::process::ExitCode {
    use std::process::ExitCode;
    let feed_override = match parse_update_feed_arg(args) {
        Ok(f) => f,
        Err(msg) => {
            eprintln!("--apply-update: {msg}");
            return ExitCode::FAILURE;
        }
    };
    let feed_url = feed_override.unwrap_or(FEED_URL);
    match apply_verified_pending(feed_url) {
        ApplyOutcome::Started => ExitCode::SUCCESS,
        ApplyOutcome::NothingPending => {
            eprintln!("--apply-update: no staged update pending");
            ExitCode::FAILURE
        }
        ApplyOutcome::Failed(msg) => {
            eprintln!("--apply-update: {msg}");
            ExitCode::FAILURE
        }
    }
}

impl UpdateController {
    /// Build the controller: construct the `UpdateManager` (`None` if the app is
    /// not Velopack-installed), load persisted prefs+status, and rehydrate any
    /// staged asset only from the authenticated manager-selected cache.
    pub fn new(app: AppHandle, state_path: PathBuf) -> Self {
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
            let scan = manager_installed_version(m).map(|installed| {
                scan_authenticated_packages(
                    &m.get_packages_dir(),
                    &installed,
                    &ProductionReleaseVerifier,
                )
            });
            let rejected = scan.as_ref().is_some_and(|scan| scan.rejected);
            if let Some(admitted) = scan.and_then(|scan| scan.selected).and_then(into_admitted) {
                reduce(
                    &mut state,
                    UpdateEvent::Downloaded {
                        version: admitted.asset.Version.clone(),
                    },
                );
                staged_asset = Some(admitted.asset);
            } else {
                let outcome_staged_or_found = matches!(
                    state.status.last_check_outcome,
                    Some(UpdateOutcome::Staged) | Some(UpdateOutcome::Found)
                );
                if rejected || outcome_staged_or_found {
                    reduce(&mut state, UpdateEvent::CheckFailed { now: now_secs() });
                }
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
            Ok(UpdateCheck::UpdateAvailable(mut info)) => {
                force_full_package(&mut info);
                let base = info.BaseRelease.as_ref().map(|a| {
                    UpdateAssetDescriptor::new(&a.FileName, a.Version.to_string(), &a.Type)
                });
                let deltas = info
                    .DeltasToTarget
                    .iter()
                    .map(|a| {
                        UpdateAssetDescriptor::new(&a.FileName, a.Version.to_string(), &a.Type)
                    })
                    .collect();
                let target = UpdateAssetDescriptor::new(
                    &info.TargetFullRelease.FileName,
                    info.TargetFullRelease.Version.to_string(),
                    &info.TargetFullRelease.Type,
                );
                let plan = classify_update_plan(base, deltas, target);
                let version = info.TargetFullRelease.Version.clone();
                let notes = non_empty(&info.TargetFullRelease.NotesMarkdown);
                ctrl.inner.rt.lock().expect("update rt").available_info = Some(*info);
                tracing::info!(
                    target: "update",
                    operation = "check",
                    result = "available",
                    version = %version,
                    plan = %format_update_plan(&plan),
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

    /// Download the currently-available update (always downloads the full asset
    /// and admits the detached signature before staging).
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
            match stage_full_package(&manager, &info, Some(tx)) {
                Ok(staged) => {
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
                    ctrl.inner.rt.lock().expect("update rt").staged_asset = None;
                    ctrl.apply(UpdateEvent::CheckFailed { now: now_secs() });
                }
            }
            ctrl.end();
        });
    }

    /// Apply the staged update and relaunch into it. Does not return on success.
    pub fn install(&self) {
        if !self.try_begin() {
            return;
        }
        let manager = match &self.inner.manager {
            Some(m) => m.clone(),
            None => {
                self.end();
                return;
            }
        };

        let admitted = match select_admitted(&manager) {
            Some(a) => a,
            None => {
                self.inner.rt.lock().expect("update rt").staged_asset = None;
                self.apply(UpdateEvent::CheckFailed { now: now_secs() });
                self.end();
                return;
            }
        };

        self.signal(UpdateEvent::InstallStarted);
        tracing::info!(
            target: "update",
            operation = "apply_restart",
            result = "install_started",
            version = %admitted.asset.Version,
            "update apply result"
        );

        let ctrl = self.clone();
        std::thread::spawn(move || {
            let version = admitted.asset.Version.clone();
            let result = spawn_admitted(
                &manager,
                admitted,
                || {
                    #[cfg(feature = "browser-host")]
                    crate::browser::quiesce_for_update();
                },
                || {
                    #[cfg(feature = "browser-host")]
                    crate::browser::resume_after_update();
                },
            );
            match result {
                Ok(()) => std::process::exit(0),
                Err(error) => {
                    tracing::warn!(target: "update", operation = "apply_restart", error = %error, "update apply failed");
                    ctrl.apply(UpdateEvent::Downloaded { version });
                    ctrl.end();
                }
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

#[cfg(test)]
mod boundary_tests {
    use super::*;
    use pl_transport_win::release_signature::KeyedTestReleaseVerifier;
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::OnceLock;
    use std::time::Instant;
    use velopack::sources::UpdateSource;

    const PACKAGE: &[u8] = include_bytes!(
        "../../crates/pl-transport-win/tests/fixtures/update/fixture-2.0.34-full.nupkg"
    );
    const SIGNATURE: &str = include_str!(
        "../../crates/pl-transport-win/tests/fixtures/update/fixture-2.0.34-full.nupkg.minisig"
    );
    const PUBLIC: &str =
        include_str!("../../crates/pl-transport-win/tests/fixtures/update/fixture-pub.pub");
    const NAME: &str = "Solstone-2.0.34-full.nupkg";

    fn server(
        requests: usize,
        reply: impl Fn(&str) -> Vec<u8> + Send + 'static,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            for _ in 0..requests {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                Instant::now() < deadline,
                                "expected HTTP request did not arrive"
                            );
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => panic!("HTTP fixture failed: {error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = [0u8; 4096];
                let count = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..count]);
                stream.write_all(&reply(&request)).unwrap();
            }
        });
        (base, worker)
    }

    fn signed_server(requests: usize) -> (String, std::thread::JoinHandle<()>) {
        server(requests, |request| {
            let body = if request.lines().next().unwrap().contains(".minisig ") {
                SIGNATURE.as_bytes()
            } else {
                PACKAGE
            };
            let mut reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .into_bytes();
            reply.extend_from_slice(body);
            reply
        })
    }

    fn recorder() -> &'static Path {
        static RECORDER: OnceLock<PathBuf> = OnceLock::new();
        RECORDER.get_or_init(|| {
            let root = tempfile::tempdir().unwrap().keep();
            let source = root.join("recorder.rs");
            std::fs::write(&source, r#"fn main() {
                let destination = std::env::current_exe().unwrap().with_extension("argv");
                std::fs::write(destination, std::env::args().skip(1).collect::<Vec<_>>().join("\n")).unwrap();
            }"#).unwrap();
            let executable = root.join("recorder.exe");
            assert!(std::process::Command::new("rustc").args(["--edition=2024"]).arg(source).arg("-o").arg(&executable).status().unwrap().success());
            executable
        })
    }

    fn manager(root: &Path, base: &str, executable: bool) -> UpdateManager {
        manager_with_source(
            root,
            R2FeedSource::fixture(base.to_string(), PUBLIC),
            executable,
        )
    }

    fn manager_with_source(
        root: &Path,
        source: impl UpdateSource + 'static,
        executable: bool,
    ) -> UpdateManager {
        let packages = root.join("packages");
        let current = root.join("current");
        std::fs::create_dir_all(&packages).unwrap();
        std::fs::create_dir_all(&current).unwrap();
        let helper = root.join("Update.exe");
        if executable {
            std::fs::copy(recorder(), &helper).unwrap();
        } else {
            std::fs::write(&helper, b"incumbent helper").unwrap();
        }
        let manifest = current.join("sq.version");
        std::fs::write(&manifest, "<package><metadata><id>Solstone</id><version>2.0.33</version><channel>win</channel><mainExe>solstone-windows-app.exe</mainExe></metadata></package>").unwrap();
        UpdateManager::new(
            source,
            Some(UpdateOptions {
                ExplicitChannel: Some("win".to_string()),
                MaximumDeltasBeforeFallback: -1,
                ..Default::default()
            }),
            Some(velopack::locator::VelopackLocatorConfig {
                RootAppDir: root.to_path_buf(),
                UpdateExePath: helper,
                PackagesDir: packages,
                ManifestPath: manifest,
                CurrentBinaryDir: current,
                IsPortable: true,
            }),
        )
        .unwrap()
    }

    fn update_info() -> UpdateInfo {
        UpdateInfo {
            TargetFullRelease: VelopackAsset {
                PackageId: "Solstone".to_string(),
                Version: "2.0.34".to_string(),
                Type: "Full".to_string(),
                FileName: NAME.to_string(),
                Size: PACKAGE.len() as u64,
                SHA256: format!("{:X}", Sha256::digest(PACKAGE)),
                ..Default::default()
            },
            BaseRelease: None,
            DeltasToTarget: Vec::new(),
            IsDowngrade: false,
        }
    }

    fn pair(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), PACKAGE).unwrap();
        std::fs::write(dir.join(format!("{name}.minisig")), SIGNATURE).unwrap();
    }

    struct AfterDownload {
        source: R2FeedSource,
        corrupt: bool,
    }

    impl UpdateSource for AfterDownload {
        fn get_release_feed(
            &self,
            channel: &str,
            app: &velopack::bundle::Manifest,
            staged: &str,
        ) -> Result<velopack::VelopackAssetFeed, velopack::Error> {
            self.source.get_release_feed(channel, app, staged)
        }
        fn download_release_entry(
            &self,
            asset: &VelopackAsset,
            path: &Path,
            progress: Option<mpsc::Sender<i16>>,
        ) -> Result<(), velopack::Error> {
            self.source.download_release_entry(asset, path, progress)?;
            if self.corrupt {
                let mut bytes = std::fs::read(path)?;
                bytes[10] ^= 1;
                std::fs::write(path, bytes)?;
            }
            Ok(())
        }
        fn open_admitted_release_entry(
            &self,
            asset: &VelopackAsset,
            path: &Path,
        ) -> Result<Option<std::fs::File>, velopack::Error> {
            let guard = self.source.open_admitted_release_entry(asset, path)?;
            #[cfg(windows)]
            assert!(
                std::fs::OpenOptions::new().write(true).open(path).is_err(),
                "admitted file must deny concurrent writes through extraction"
            );
            Ok(guard)
        }
    }

    #[test]
    fn manager_reauthenticates_after_download_before_helper_extraction() {
        for corrupt in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let (base, worker) = signed_server(2);
            let source = AfterDownload {
                source: R2FeedSource::fixture(base, PUBLIC),
                corrupt,
            };
            let manager = manager_with_source(root.path(), source, false);
            let helper = std::fs::read(root.path().join("Update.exe")).unwrap();
            let mut info = update_info();
            if corrupt {
                let mut bytes = PACKAGE.to_vec();
                bytes[10] ^= 1;
                info.TargetFullRelease.SHA256 = format!("{:X}", Sha256::digest(bytes));
            }
            assert_eq!(manager.download_updates(&info, None).is_err(), corrupt);
            assert_eq!(
                std::fs::read(root.path().join("Update.exe")).unwrap(),
                helper
            );
            worker.join().unwrap();
        }
    }

    #[test]
    fn manager_check_ignores_unsigned_cached_delta_base() {
        let root = tempfile::tempdir().unwrap();
        let mut asset = update_info().TargetFullRelease;
        asset.Version = "2.0.35".to_string();
        asset.FileName = "Solstone-2.0.35-full.nupkg".to_string();
        let mut delta = asset.clone();
        delta.Type = "Delta".to_string();
        delta.FileName = "Solstone-2.0.35-delta.nupkg".to_string();
        let body = serde_json::to_vec(&serde_json::json!({"Assets": [asset, delta]})).unwrap();
        let (base, worker) = server(1, move |_| {
            let mut response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .into_bytes();
            response.extend_from_slice(&body);
            response
        });
        let manager = manager(root.path(), &base, false);
        std::fs::write(root.path().join("packages").join(NAME), PACKAGE).unwrap();
        let UpdateCheck::UpdateAvailable(info) = manager.check_for_updates().unwrap() else {
            panic!("new full release expected")
        };
        assert!(info.BaseRelease.is_none());
        assert!(info.DeltasToTarget.is_empty());
        worker.join().unwrap();
    }

    #[test]
    fn sidecar_persistence_failure_keeps_prior_pending_and_retry_readies_a_pair() {
        let root = tempfile::tempdir().unwrap();
        let (base, worker) = signed_server(4);
        let manager = manager(root.path(), &base, false);
        let packages = root.path().join("packages");
        let prior = "Solstone-prior-full.nupkg";
        pair(&packages, prior);
        let blocked = packages.join(format!("{NAME}.minisig"));
        std::fs::create_dir(&blocked).unwrap();
        let helper = std::fs::read(root.path().join("Update.exe")).unwrap();
        let key = KeyedTestReleaseVerifier(PUBLIC);
        assert!(stage_full_package_with_verifier(&manager, &update_info(), None, &key).is_err());
        assert!(admit_cached_package(&packages.join(prior), &key).is_ok());
        assert_eq!(
            std::fs::read(root.path().join("Update.exe")).unwrap(),
            helper
        );
        assert!(!packages.join(NAME).exists());
        std::fs::remove_dir(blocked).unwrap();
        assert!(stage_full_package_with_verifier(&manager, &update_info(), None, &key).is_ok());
        assert!(admit_cached_package(&packages.join(NAME), &key).is_ok());
        worker.join().unwrap();
    }

    #[test]
    fn staging_rejects_traversal_before_cleanup_and_authenticates_the_cache_shortcut() {
        let root = tempfile::tempdir().unwrap();
        let manager = manager(root.path(), "http://127.0.0.1:9", false);
        let packages = root.path().join("packages");
        let sentinel = root.path().join("pairing.json");
        std::fs::write(&sentinel, b"keep pairing").unwrap();
        let key = KeyedTestReleaseVerifier(PUBLIC);
        for name in ["../pairing.json", r"..\pairing.json"] {
            let mut info = update_info();
            info.TargetFullRelease.FileName = name.to_string();
            assert!(stage_full_package_with_verifier(&manager, &info, None, &key).is_err());
            assert_eq!(std::fs::read(&sentinel).unwrap(), b"keep pairing");
        }
        std::fs::write(packages.join(NAME), PACKAGE).unwrap();
        assert!(stage_full_package_with_verifier(&manager, &update_info(), None, &key).is_err());
        assert!(!packages.join(NAME).exists());
        pair(&packages, NAME);
        assert!(stage_full_package_with_verifier(&manager, &update_info(), None, &key).is_ok());
    }

    #[test]
    fn startup_and_cli_apply_refuse_before_quiesce_and_select_the_verified_package() {
        let root = tempfile::tempdir().unwrap();
        let manager = manager(root.path(), "http://127.0.0.1:9", true);
        let packages = root.path().join("packages");
        let higher = "Solstone-2.0.99-full.nupkg";
        std::fs::write(packages.join(higher), PACKAGE).unwrap();
        let key = KeyedTestReleaseVerifier(PUBLIC);
        let quiesced = std::cell::Cell::new(0);
        let resumed = std::cell::Cell::new(0);
        let mut short: Vec<_> = SIGNATURE.lines().collect();
        short[3] = "AA==";
        for sidecar in [
            None,
            Some("invalid signature".to_string()),
            Some(short.join("\n") + "\n"),
        ] {
            let signature = packages.join(format!("{higher}.minisig"));
            if let Some(text) = sidecar {
                std::fs::write(&signature, text).unwrap();
            } else if signature.exists() {
                std::fs::remove_file(signature).unwrap();
            }
            assert!(matches!(
                apply_from_cache(
                    &manager,
                    &key,
                    || quiesced.set(quiesced.get() + 1),
                    || resumed.set(resumed.get() + 1)
                ),
                ApplyOutcome::Failed(_)
            ));
            assert_eq!(quiesced.get(), 0);
            assert_eq!(resumed.get(), 0);
            assert!(!root.path().join("Update.argv").exists());
        }
        pair(&packages, NAME);
        assert!(matches!(
            apply_from_cache(
                &manager,
                &key,
                || quiesced.set(quiesced.get() + 1),
                || resumed.set(resumed.get() + 1)
            ),
            ApplyOutcome::Started
        ));
        assert_eq!(quiesced.get(), 1);
        assert_eq!(resumed.get(), 0);
        let argv = root.path().join("Update.argv");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !argv.exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        let args = std::fs::read_to_string(argv).unwrap();
        let args: Vec<_> = args.lines().collect();
        assert!(args
            .windows(2)
            .any(|args| args[0] == "--package" && Path::new(args[1]) == packages.join(NAME)));
        assert!(
            packages.join(higher).exists(),
            "the unsigned higher sibling still exists at helper handoff"
        );
    }

    #[test]
    fn admission_from_another_cache_refuses_before_quiesce_or_helper() {
        let root = tempfile::tempdir().unwrap();
        let other_cache = tempfile::tempdir().unwrap();
        let manager = manager(root.path(), "http://127.0.0.1:9", true);
        assert_eq!(manager.get_packages_dir(), root.path().join("packages"));
        let helper = std::fs::read(root.path().join("Update.exe")).unwrap();
        std::fs::hard_link(
            root.path().join("Update.exe"),
            manager.get_packages_dir().join(".betaId"),
        )
        .unwrap();
        neutralize_staging_id(&manager.get_packages_dir()).unwrap();
        assert_eq!(
            std::fs::read(root.path().join("Update.exe")).unwrap(),
            helper
        );
        assert_eq!(
            std::fs::read(manager.get_packages_dir().join(".betaId")).unwrap(),
            b""
        );
        pair(other_cache.path(), NAME);
        std::fs::write(
            manager.get_packages_dir().join(NAME),
            b"unsigned package at the helper's actual path",
        )
        .unwrap();
        let key = KeyedTestReleaseVerifier(PUBLIC);
        let admitted =
            into_admitted(admit_cached_package(&other_cache.path().join(NAME), &key).unwrap())
                .unwrap();
        let quiesced = std::cell::Cell::new(false);
        assert!(spawn_admitted(&manager, admitted, || quiesced.set(true), || {}).is_err());
        assert!(!quiesced.get());
        assert!(!root.path().join("Update.argv").exists());
        assert!(matches!(
            apply_from_cache(&manager, &key, || quiesced.set(true), || {}),
            ApplyOutcome::Failed(_)
        ));
        assert!(!quiesced.get());
    }

    #[test]
    fn occupied_staging_id_refuses_manager_preparation() {
        let root = tempfile::tempdir().unwrap();
        let manager = manager(root.path(), "http://127.0.0.1:9", false);
        let occupied = manager.get_packages_dir().join(".betaId");
        std::fs::create_dir(&occupied).unwrap();
        assert!(prepare_manager(manager.clone()).is_err());
        assert!(occupied.is_dir());
        assert_eq!(
            std::fs::read(root.path().join("Update.exe")).unwrap(),
            b"incumbent helper"
        );
        std::fs::remove_dir(occupied).unwrap();
        assert!(prepare_manager(manager).is_ok());
    }

    #[test]
    fn helper_spawn_failure_resumes_and_retains_the_authenticated_pair() {
        let root = tempfile::tempdir().unwrap();
        let manager = manager(root.path(), "http://127.0.0.1:9", false);
        let packages = root.path().join("packages");
        pair(&packages, NAME);
        let key = KeyedTestReleaseVerifier(PUBLIC);
        let quiesced = std::cell::Cell::new(false);
        let resumed = std::cell::Cell::new(false);
        assert!(matches!(
            apply_from_cache(&manager, &key, || quiesced.set(true), || resumed.set(true)),
            ApplyOutcome::Failed(_)
        ));
        assert!(quiesced.get() && resumed.get());
        assert!(admit_cached_package(&packages.join(NAME), &key).is_ok());
    }

    #[test]
    fn feed_redirect_never_contacts_the_destination() {
        let destination = TcpListener::bind("127.0.0.1:0").unwrap();
        destination.set_nonblocking(true).unwrap();
        let url = format!("http://{}/trap", destination.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                match destination.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 13\r\nConnection: close\r\n\r\n{\"Assets\":[]}");
                        return true;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(error) => panic!("redirect trap failed: {error}"),
                }
            }
            false
        });
        let (base, origin) = server(1, move |request| {
            assert!(request.starts_with("GET /releases.win.json "));
            format!("HTTP/1.1 302 Found\r\nLocation: {url}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes()
        });
        assert!(R2FeedSource::new(base)
            .get_release_feed("win", &Default::default(), "")
            .is_err());
        origin.join().unwrap();
        assert!(!worker.join().unwrap());
    }
}
