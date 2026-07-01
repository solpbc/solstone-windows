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

/// Where the owner's gathered media lives on this PC, for the Status surface.
/// `bytes` is best-effort and currently always `None` — segments_dir holds
/// per-segment SUBDIRECTORIES, so a shallow read_dir sum is meaningless and a
/// deep walk is refused; the UI renders a size only when it is `Some`.
#[derive(serde::Serialize)]
pub struct StorageInfo {
    root: String,
    bytes: Option<u64>,
}

#[tauri::command]
pub fn storage_info() -> StorageInfo {
    StorageInfo {
        root: platform_win::segments_dir().to_string_lossy().into_owned(),
        bytes: None,
    }
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

/// Open (create-or-focus) the native journal window backed by the loopback bridge.
/// Refuses before creating any listener/window when not paired.
#[tauri::command]
pub async fn open_journal(app: tauri::AppHandle) -> Result<(), String> {
    crate::windows::open_journal(&app)
        .await
        .map_err(|e| e.token().to_string())
}

/// The Windows release-history page — the in-app "read the full notes online"
/// link's destination, the analog of macOS `UpdatesCopy.releaseNotesOnlineURL`.
const RELEASE_NOTES_URL: &str = "https://solstone.app/releases/windows";

/// Open the release-history page in the owner's default browser. The webview is a
/// sealed renderer with no navigation power (a raw link would only re-navigate
/// the Settings webview itself); this hands the one outbound affordance to the
/// OS. The URL is fixed here, never passed from the webview, so the renderer can
/// only ever open this exact first-party page — not an arbitrary URL.
#[tauri::command]
pub fn open_release_notes(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(RELEASE_NOTES_URL, None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn open_storage_folder(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let root = platform_win::segments_dir();
    let _ = std::fs::create_dir_all(&root);
    app.opener()
        .open_path(root.to_string_lossy().into_owned(), None::<&str>)
        .map_err(|e| e.to_string())
}

/// Persist a structurally minimal frontend error event.
#[tauri::command]
pub fn log_frontend_error(record: observer_log::FrontendErrorRecord) {
    tracing::error!(
        target: "frontend",
        kind = ?record.kind,
        origin = ?record.origin,
        line = record.line,
        column = record.column,
        "frontend error"
    );
}

/// Record that a view's frontend painted its contract window root. Honest-state:
/// only our own renderer can call this back, and only after stamping the contract
/// root, so `rendered` is *earned*. The view is derived from the calling window's
/// label (not passed by the frontend). Fire-and-forget; unknown labels are ignored.
#[tauri::command]
pub fn view_rendered(window: tauri::WebviewWindow, state: tauri::State<'_, crate::app::AppState>) {
    if let Some(view) = observer_model::View::parse(window.label()) {
        if let Ok(mut health) = state.health.lock() {
            health.views.insert(
                view.label().to_string(),
                observer_model::ViewRenderState::Rendered,
            );
        }
    }
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

    tracing::info!(
        target: "sync",
        pair_link = %observer_log::redact_pair_link(&link),
        "pairing attempt"
    );
    let paired = match pl_transport_win::service::pair_and_register(&link, &cfg, sync.clone()).await
    {
        Ok(paired) => {
            tracing::info!(target: "sync", outcome = "paired", "pairing result");
            paired
        }
        Err(error) => {
            let error = error.to_string();
            tracing::warn!(
                target: "sync",
                pair_link = %observer_log::redact_pair_link(&link),
                error = %observer_log::redact_secret("pairing-error", &error),
                "pairing result"
            );
            return Err(error);
        }
    };

    tracing::info!(
        target: "sync",
        source = "fresh_pair",
        "uploader started"
    );
    tauri::async_runtime::spawn(async move {
        if let Err(error) =
            pl_transport_win::service::run_uploader(paired, cfg, health, sync, up_rx).await
        {
            let error = error.to_string();
            tracing::warn!(
                target: "sync",
                source = "fresh_pair",
                error = %observer_log::redact_secret("uploader-error", &error),
                "uploader exited"
            );
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

// ── Global pause/resume hotkey ─────────────────────────────────────────────────
// The owner's configurable global hotkey. `set_hotkey` writes the desired config;
// the notification pump reconciles the OS registration on its next poll and writes
// back the honest outcome (Registered / ComboTaken / …), which `get_hotkey`
// surfaces — a taken combo is reported, never a silent no-op.

/// The current hotkey config + its live registration outcome (Settings render).
#[tauri::command]
pub fn get_hotkey(
    ctrl: tauri::State<'_, crate::hotkey::HotkeyController>,
) -> observer_hotkey::HotkeyView {
    ctrl.view()
}

/// Replace the global-hotkey config. Effective on the pump's next reconcile and
/// persisted to `hotkey.json`.
#[tauri::command]
pub fn set_hotkey(
    ctrl: tauri::State<'_, crate::hotkey::HotkeyController>,
    config: observer_hotkey::HotkeyConfig,
) {
    ctrl.set(config);
}

// ── Microphone controls ────────────────────────────────────────────────────────
// Owner device priority + per-device disable + input gain. `set_mic_config` writes
// the shared policy; the mic capture loop reconciles selection + gain on its next
// cadence and publishes the actually-open device id back, which `get_mic_config`
// surfaces as `active_id` — so "active" is earned, not guessed.

/// The current mic config + the actually-open device id (Settings render).
#[tauri::command]
pub fn get_mic_config(ctrl: tauri::State<'_, crate::mic::MicController>) -> observer_mic::MicView {
    ctrl.view()
}

/// Replace the mic config (priority / disabled / gain). Effective on the capture
/// loop's next reconcile and persisted to `mic.json`.
#[tauri::command]
pub fn set_mic_config(
    ctrl: tauri::State<'_, crate::mic::MicController>,
    config: observer_mic::MicConfig,
) {
    ctrl.set(config);
}

/// The live input devices the owner can prioritize / disable (id + friendly name).
#[tauri::command]
pub fn list_mic_devices() -> Vec<observer_mic::MicDeviceRef> {
    capture_wasapi::list_mic_devices()
}

// ── Cache retention ────────────────────────────────────────────────────────────
// How long confirmed-synced local segments are kept. `set_retention` writes the
// shared policy; the upload coordinator honors it on its next tick (delete on
// confirm for don't-keep, else retain + prune past the window).

/// The current cache-retention policy (Settings render).
#[tauri::command]
pub fn get_retention(
    ctrl: tauri::State<'_, crate::retention::RetentionController>,
) -> observer_retention::RetentionConfig {
    ctrl.get()
}

/// Replace the cache-retention policy. Effective on the coordinator's next tick
/// and persisted to `retention.json`.
#[tauri::command]
pub fn set_retention(
    ctrl: tauri::State<'_, crate::retention::RetentionController>,
    config: observer_retention::RetentionConfig,
) {
    ctrl.set(config);
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
