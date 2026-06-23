// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The shell -> backend command surface.
//!
//! These are user **intents**. They ask the engine to do something; they never
//! set UI state. The webview cannot mint status — it has no input but the
//! `health://changed` event it subscribes to — so "status earned, never
//! asserted" holds by construction. Commands mirror the boundary contract:
//! `start_observing / pause / resume / get_health / open_settings / open_about`.

use capture_engine::EngineCommand;
use observer_model::HealthDump;
use observer_model::PauseReason;

/// Ask the engine to begin observing. The resulting phase is *computed* by the
/// reducer once sources go active — this command does not return "observing".
#[tauri::command]
pub fn start_observing(state: tauri::State<'_, crate::app::AppState>) -> Result<(), String> {
    state
        .commands
        .send(EngineCommand::Start)
        .map_err(|error| error.to_string())
}

/// Ask the engine to pause. `reason` is an owner-meaningful token;
/// `duration_secs` bounds an operator pause (auto-resume after it elapses) and is
/// `None` for an indefinite "until I resume" pause.
#[tauri::command]
pub fn pause(
    state: tauri::State<'_, crate::app::AppState>,
    reason: String,
    duration_secs: Option<u64>,
) -> Result<(), String> {
    let reason = match reason.as_str() {
        "session_locked" => PauseReason::SessionLocked,
        "system_suspending" => PauseReason::SystemSuspending,
        _ => PauseReason::Operator,
    };
    state
        .commands
        .send(EngineCommand::Pause {
            reason,
            duration_secs,
        })
        .map_err(|error| error.to_string())
}

/// Ask the engine to resume.
#[tauri::command]
pub fn resume(state: tauri::State<'_, crate::app::AppState>) -> Result<(), String> {
    state
        .commands
        .send(EngineCommand::Resume)
        .map_err(|error| error.to_string())
}

/// Return the current honest health snapshot (same payload as `--dump-state`
/// and `/healthz`).
#[tauri::command]
pub fn get_health(state: tauri::State<'_, crate::app::AppState>) -> Result<HealthDump, String> {
    state
        .health
        .lock()
        .map(|health| health.clone())
        .map_err(|_| "health mutex poisoned".to_string())
}

/// Open (create on demand) the Settings window.
#[tauri::command]
pub async fn open_settings(app: tauri::AppHandle) -> Result<(), String> {
    crate::windows::open_settings(&app).map_err(|e| e.to_string())
}

/// Open (create on demand) the About window.
#[tauri::command]
pub async fn open_about(app: tauri::AppHandle) -> Result<(), String> {
    crate::windows::open_about(&app).map_err(|e| e.to_string())
}

/// Pair this observer to a journal from a scanned/pasted pair-link, then start
/// uploading. The pairing handshake + registration run inline (so the UI sees
/// success/failure), then the upload + heartbeat loop is spawned for the process
/// lifetime. Outcome is also reflected through the health dump's pairing phase.
#[tauri::command]
pub async fn pair(
    state: tauri::State<'_, crate::app::AppState>,
    link: String,
) -> Result<(), String> {
    // Snapshot everything we need before the first await — never hold the State
    // borrow across it.
    let cfg = state.sync_config.clone();
    let sync = state.sync.clone();
    let health = state.health.clone();
    let (up_tx, up_rx) = tokio::sync::oneshot::channel();
    if let Ok(mut shutdowns) = state._sync_shutdowns.lock() {
        shutdowns.push(up_tx);
    }

    let paired = pl_transport_win::service::pair_and_register(&link, &cfg, sync.clone())
        .await
        .map_err(|e| e.to_string())?;

    tauri::async_runtime::spawn(async move {
        if let Err(error) =
            pl_transport_win::service::run_uploader(paired, cfg, health, sync, up_rx).await
        {
            eprintln!("uploader exited: {error}");
        }
    });
    Ok(())
}

// ── Capture-exclusion intents ─────────────────────────────────────────────────
// The owner's privacy controls. `set_exclusions` takes effect on the next
// captured frame (it writes the shared rules handle the WGC source reads) and
// persists across restart. Exclusion *activity* (frames redacted / dropped) is
// surfaced through the health dump, not here.

/// The current capture-exclusion rules (for the Settings initial render).
#[tauri::command]
pub fn get_exclusions(
    state: tauri::State<'_, crate::app::AppState>,
) -> observer_exclusion::ExclusionRules {
    state.exclusions.get()
}

/// Replace the capture-exclusion rules. Effective on the next captured frame and
/// persisted to `exclusions.json`.
#[tauri::command]
pub fn set_exclusions(
    state: tauri::State<'_, crate::app::AppState>,
    rules: observer_exclusion::ExclusionRules,
) {
    state.exclusions.set(rules);
}

/// The distinct running apps the owner can pick to exclude (exe + a friendly
/// label). Picking from this list keys exclusion on a real running process's
/// exe — robust identity, not free-text.
#[tauri::command]
pub fn list_running_apps() -> Vec<observer_exclusion::RunningApp> {
    capture_wgc::list_running_apps()
}

// ── Updater intents ──────────────────────────────────────────────────────────
// User intents for the in-app updater. Like the rest of the IPC surface these
// only *ask* the engine to act; update state is earned from the Velopack result
// and pushed back over `update://changed` — the webview never mints update state.

/// The current honest update snapshot (for the initial render on Settings open).
#[tauri::command]
pub fn update_get(
    ctrl: tauri::State<'_, crate::update::UpdateController>,
) -> observer_update::UpdateView {
    ctrl.view()
}

/// Start a manual update check (also serves the "retry" / "check again" controls).
#[tauri::command]
pub fn update_check_now(ctrl: tauri::State<'_, crate::update::UpdateController>) {
    ctrl.check();
}

/// Download the currently-available update.
#[tauri::command]
pub fn update_download(ctrl: tauri::State<'_, crate::update::UpdateController>) {
    ctrl.download();
}

/// Apply the staged update and relaunch into it.
#[tauri::command]
pub fn update_install(ctrl: tauri::State<'_, crate::update::UpdateController>) {
    ctrl.install();
}

/// Dismiss an available/failed block back to idle.
#[tauri::command]
pub fn update_dismiss(ctrl: tauri::State<'_, crate::update::UpdateController>) {
    ctrl.dismiss();
}

/// Persist the "check for updates automatically" toggle.
#[tauri::command]
pub fn update_set_auto_check(ctrl: tauri::State<'_, crate::update::UpdateController>, on: bool) {
    ctrl.set_auto_check(on);
}

/// Persist the "download updates in the background" toggle.
#[tauri::command]
pub fn update_set_auto_download(ctrl: tauri::State<'_, crate::update::UpdateController>, on: bool) {
    ctrl.set_auto_download(on);
}

/// Persist the check-frequency preference (`day` / `week` / `month`).
#[tauri::command]
pub fn update_set_interval(
    ctrl: tauri::State<'_, crate::update::UpdateController>,
    interval: String,
) {
    let iv = match interval.as_str() {
        "day" => observer_update::CheckInterval::Day,
        "month" => observer_update::CheckInterval::Month,
        _ => observer_update::CheckInterval::Week,
    };
    ctrl.set_interval(iv);
}
