// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The shell -> backend command surface.
//!
//! These are user **intents**. They ask the engine to do something; they never
//! set UI state. The webview cannot mint status — it has no input but the
//! `health://changed` event it subscribes to — so "status earned, never
//! asserted" holds by construction. Commands mirror the boundary contract:
//! `pause / resume / get_health / open_settings / open_about`.

use capture_engine::EngineCommand;
use observer_model::HealthDump;
use observer_model::PauseReason;
use pl_transport_win::credential::PairedState;
use tauri::Manager;

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
pub async fn open_journal(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::app::AppState>,
) -> Result<(), String> {
    crate::windows::open_journal(&state, &app, Some(&app))
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
/// uploading. The pairing handshake runs inline (so the UI sees success/failure),
/// then the upload loop is spawned for the process
/// lifetime. Outcome is also reflected through the health dump's pairing phase.
#[tauri::command]
pub async fn pair(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::app::AppState>,
    link: String,
) -> Result<(), String> {
    // Tauri builds and moves this future on the main thread before the runtime
    // polls it. With the handshake and the uploader hand-off inline it was large
    // enough to overflow that thread when the owner pressed pair. Boxing keeps the
    // future Tauri moves down to the arguments and one pointer.
    Box::pin(pair_session(&app, &state, link)).await
}

pub async fn pair_session<S: crate::windows::JournalSurface>(
    surface: &S,
    state: &crate::app::AppState,
    link: String,
) -> Result<(), String> {
    // Snapshot everything needed by the uploader; the slot lock below serializes
    // the pairing handshake and replacement so concurrent pair attempts cannot
    // spawn competing uploaders.
    let cfg = state.sync_config.clone();
    let sync = state.sync.clone();

    tracing::info!(
        target: "sync",
        pair_link = %observer_log::redact_pair_link(&link),
        "pairing attempt"
    );

    let journal_bridge = state.journal_bridge.clone();
    let setup_browser = state.setup_browser.clone();

    let shutdown_bridge = move || {
        let journal_bridge = journal_bridge.clone();
        let setup_browser = setup_browser.clone();
        async move {
            let bridge = journal_bridge
                .lock()
                .ok()
                .and_then(|mut bridge| bridge.take());
            if let Some(bridge) = bridge {
                bridge.shutdown_and_wait().await;
            }
            let s_bridge = setup_browser
                .lock()
                .ok()
                .and_then(|mut bridge| bridge.take());
            if let Some(s_bridge) = s_bridge {
                s_bridge.handle.shutdown_and_wait().await;
            }
        }
    };

    // Pair holds journal-open across the handshake, so an open waits behind pairing.
    #[cfg(test)]
    if let Some(probe) = &state.probe {
        probe.pair_at_lock.notify_one();
        probe.release_pair.notified().await;
        probe
            .pair_entered_lock
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
    let _journal_open_guard = state.journal_open_lock.lock().await;

    let close_journal_window = {
        let surface_ref = surface;
        move || {
            surface_ref.close_journal();
            async move {}
        }
    };

    let result = pl_transport_win::session::pair(
        &link,
        &cfg,
        sync,
        &state.uploader_slot,
        &state.credential_access,
        &state.pairing_admission,
        shutdown_bridge,
        close_journal_window,
    )
    .await
    .map(|_| ());

    match result {
        Ok(_) => {
            tracing::info!(target: "sync", outcome = "paired", "pairing result");
            Ok(())
        }
        Err(error) => {
            let error = error.to_string();
            tracing::warn!(
                target: "sync",
                pair_link = %observer_log::redact_pair_link(&link),
                error = %observer_log::redact_secret("pairing-error", &error),
                "pairing result"
            );
            Err(error)
        }
    }
}

/// Answer the confirmation gate for an awaiting pairing.
#[tauri::command]
pub async fn answer_pairing(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::app::AppState>,
    binding: String,
    action: String,
) -> Result<(), String> {
    answer_pairing_surface(&app, &state, binding, action).await
}

pub async fn answer_pairing_surface<S: crate::windows::JournalSurface>(
    surface: &S,
    state: &crate::app::AppState,
    binding: String,
    action: String,
) -> Result<(), String> {
    let act = match action.as_str() {
        "confirm" => pl_transport_win::session::PairingAction::Confirm,
        "reject" => pl_transport_win::session::PairingAction::Reject,
        "cancel" => pl_transport_win::session::PairingAction::Cancel,
        _ => return Err(format!("unknown pairing action: {action}")),
    };

    let cfg = state.sync_config.clone();
    let sync = state.sync.clone();

    let _journal_open_guard = state.journal_open_lock.lock().await;
    let effect = pl_transport_win::session::answer(
        act,
        &binding,
        &cfg,
        &sync,
        &state.credential_access,
        &state.uploader_slot,
    )
    .await
    .map_err(|e| e.to_string())?;

    if effect == pl_transport_win::session::AnswerEffect::Closed {
        #[cfg(test)]
        if let Some(probe) = &state.probe {
            probe.answer_closed.notify_one();
            probe.release_answer_cleanup.notified().await;
        }
        let bridge = state
            .journal_bridge
            .lock()
            .ok()
            .and_then(|mut bridge| bridge.take());
        if let Some(bridge) = bridge {
            bridge.shutdown_and_wait().await;
        }
        let s_bridge = state
            .setup_browser
            .lock()
            .ok()
            .and_then(|mut bridge| bridge.take());
        if let Some(s_bridge) = s_bridge {
            s_bridge.handle.shutdown_and_wait().await;
        }
        surface.close_journal();
    }

    Ok(())
}

async fn migration_client(
    state: &crate::app::AppState,
) -> Result<std::sync::Arc<pl_transport_win::ObserverClient>, pl_transport_win::TransportError> {
    if let Some(access) = state.credential_access.lock().await.as_ref() {
        return Ok(access.client_slot().load());
    }
    let paired = PairedState::load(&state.sync_config.state_path)?;
    let credential = paired
        .credential
        .ok_or(pl_transport_win::TransportError::NotPaired)?;
    pl_transport_win::ObserverClient::new(
        credential,
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    )
    .map(std::sync::Arc::new)
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationUiIssue {
    Offline,
    Unsupported,
    StorageUnavailable,
    DecisionRefused,
    TargetMissing,
    KeyRefused,
}

#[derive(Debug, serde::Serialize)]
pub struct MigrationUiSnapshot {
    migration: Option<pl_transport_win::migration::MigrationView>,
    issue: Option<MigrationUiIssue>,
}

fn migration_ui_issue(error: &pl_transport_win::TransportError) -> MigrationUiIssue {
    use pl_transport_win::TransportError as E;
    match error {
        E::ClientKeyProtectionRefused => MigrationUiIssue::KeyRefused,
        E::CredentialRecoveryRequired | E::CredentialMalformed | E::Io(_) | E::NotPaired => {
            MigrationUiIssue::StorageUnavailable
        }
        E::NoEndpoint | E::Tls(_) | E::Http(_) | E::Mux(_) => MigrationUiIssue::Offline,
        E::Rejected { status, body } => {
            let value: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
            let reason = value
                .get("reason_code")
                .or_else(|| value.get("reason"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            match reason {
                "migration_protocol_unsupported" => MigrationUiIssue::Unsupported,
                "paired_device_not_found" | "migration_target_conflict" => {
                    MigrationUiIssue::TargetMissing
                }
                _ if *status >= 500 => MigrationUiIssue::Offline,
                _ => MigrationUiIssue::DecisionRefused,
            }
        }
        _ => MigrationUiIssue::DecisionRefused,
    }
}

fn migration_ui_snapshot(
    path: &std::path::Path,
    issue: Option<MigrationUiIssue>,
) -> MigrationUiSnapshot {
    match pl_transport_win::migration::view(path) {
        Ok(migration) => MigrationUiSnapshot { migration, issue },
        Err(error) => MigrationUiSnapshot {
            migration: None,
            issue: Some(migration_ui_issue(&error)),
        },
    }
}

/// Reconcile any saved replacement decision with the journal and return durable owner state.
#[tauri::command]
pub async fn pairing_migration_state(
    state: tauri::State<'_, crate::app::AppState>,
) -> Result<MigrationUiSnapshot, String> {
    let state = state.inner();
    let path = &state.sync_config.state_path;
    let mut issue = None;
    let record = match pl_transport_win::migration::load(path) {
        Ok(record) => record,
        Err(error) => {
            return Ok(migration_ui_snapshot(
                path,
                Some(migration_ui_issue(&error)),
            ));
        }
    };
    if let Some(mut record) = record {
        if record
            .pending_decision
            .as_ref()
            .is_some_and(|decision| decision.result == "target_missing")
        {
            issue = Some(MigrationUiIssue::TargetMissing);
        } else if record
            .pending_decision
            .as_ref()
            .is_some_and(|decision| decision.result == "unknown")
        {
            match migration_client(state).await {
                Ok(client) => {
                    match pl_transport_win::migration::reconcile_saved_decision(
                        path,
                        &mut record,
                        client.as_ref(),
                    )
                    .await
                    {
                        Ok(pl_transport_win::migration::DecisionReconcile::TargetMissing) => {
                            issue = Some(MigrationUiIssue::TargetMissing);
                        }
                        Err(error) => issue = Some(migration_ui_issue(&error)),
                        Ok(_) => {}
                    }
                }
                Err(error) => issue = Some(migration_ui_issue(&error)),
            }
        }
    }
    let classify_result = pl_transport_win::migration::classify_admitted_fresh_pair_offer(
        path,
        |admitted| {
            let admitted = admitted.clone();
            let state_path = path.to_path_buf();
            async move {
                let client = {
                    let access_guard = state.credential_access.lock().await;
                    if let Some(access) = access_guard.as_ref() {
                        let client = access.client_slot().load();
                        if !pl_transport_win::migration::credential_matches_admitted_fresh_pair(
                            client.credential(),
                            &admitted,
                        )? {
                            return Ok(
                                pl_transport_win::migration::AdmittedFreshPairRead::Superseded,
                            );
                        }
                        Some(client)
                    } else {
                        None
                    }
                };
                let client = match client {
                    Some(client) => client,
                    None => {
                        let paired = PairedState::load(&state_path)?;
                        let credential = paired
                            .credential
                            .ok_or(pl_transport_win::TransportError::NotPaired)?;
                        if !pl_transport_win::migration::credential_matches_admitted_fresh_pair(
                            &credential,
                            &admitted,
                        )? {
                            return Ok(
                                pl_transport_win::migration::AdmittedFreshPairRead::Superseded,
                            );
                        }
                        std::sync::Arc::new(pl_transport_win::ObserverClient::new(
                            credential,
                            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                        )?)
                    }
                };
                match client.list_paired_devices().await {
                    Ok(devices) => Ok(pl_transport_win::migration::AdmittedFreshPairRead::Devices(
                        devices,
                    )),
                    Err(_) => Ok(pl_transport_win::migration::AdmittedFreshPairRead::Unavailable),
                }
            }
        },
        |record| pl_transport_win::migration::save(path, record),
    )
    .await;
    if let Err(error) = classify_result {
        issue = Some(migration_ui_issue(&error));
    }
    Ok(migration_ui_snapshot(path, issue))
}

/// Save an exact v1 decision before reconciling it with the journal.
#[tauri::command]
pub async fn pairing_migration_decide(
    state: tauri::State<'_, crate::app::AppState>,
    choice: pl_transport_win::migration::Choice,
    replaces_cid: Option<String>,
) -> Result<MigrationUiSnapshot, String> {
    let path = &state.sync_config.state_path;
    let mut record = pl_transport_win::migration::load(path)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "migration is unavailable".to_string())?;
    let paired = PairedState::load(path).map_err(|e| e.to_string())?;
    let credential = paired
        .credential
        .as_ref()
        .ok_or_else(|| "not paired".to_string())?;
    let binding =
        pl_transport_win::ack::JournalIdentity::from_credential(credential).client_cert_sha256;
    if record.binding != binding
        || record.pairing_generation
            != pl_transport_win::credential::pairing_generation(&credential.client_cert_pem)
    {
        return Err("migration owner changed".to_string());
    }
    pl_transport_win::migration::persist_decision(path, &mut record, choice, replaces_cid)
        .map_err(|e| e.to_string())?;
    let issue = if record
        .pending_decision
        .as_ref()
        .is_some_and(|decision| decision.result == "target_missing")
    {
        Some(MigrationUiIssue::TargetMissing)
    } else {
        match migration_client(&state).await {
            Ok(client) => match pl_transport_win::migration::reconcile_saved_decision(
                path,
                &mut record,
                client.as_ref(),
            )
            .await
            {
                Ok(pl_transport_win::migration::DecisionReconcile::TargetMissing) => {
                    Some(MigrationUiIssue::TargetMissing)
                }
                Err(error) => Some(migration_ui_issue(&error)),
                Ok(_) => None,
            },
            Err(error) => Some(migration_ui_issue(&error)),
        }
    };
    Ok(migration_ui_snapshot(path, issue))
}

/// Return other authorized devices with labels kept separate from their CIDs.
#[tauri::command]
pub async fn pairing_migration_devices(
    state: tauri::State<'_, crate::app::AppState>,
) -> Result<Vec<pl_transport_win::device_metadata::PairedDevice>, String> {
    let path = &state.sync_config.state_path;
    let paired = PairedState::load(path).map_err(|error| error.to_string())?;
    let credential = paired.credential.ok_or_else(|| "not paired".to_string())?;
    let current_cid = pl_transport_win::migration::credential_cid(&credential)
        .map_err(|error| error.to_string())?;
    let client = migration_client(&state)
        .await
        .map_err(|error| error.to_string())?;
    client
        .list_paired_devices()
        .await
        .map(|devices| pl_transport_win::migration::without_own_cid(devices, &current_cid))
        .map_err(|error| error.to_string())
}

/// Dismiss the saved fresh-pair offer without submitting a device decision.
#[tauri::command]
pub fn pairing_migration_offer_dismiss(
    state: tauri::State<'_, crate::app::AppState>,
    expected_binding: String,
    expected_generation: [u8; 32],
    expected_revision: u64,
) -> Result<(), String> {
    pl_transport_win::migration::dismiss_fresh_pair_offer(
        &state.sync_config.state_path,
        &expected_binding,
        expected_generation,
        expected_revision,
    )
    .map_err(|e| e.to_string())
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
/// reports whether the new rules were persisted to `exclusions.json`.
#[tauri::command]
pub fn set_exclusions(
    state: tauri::State<'_, crate::app::AppState>,
    rules: observer_exclusion::ExclusionRules,
) -> crate::exclusions::SetExclusionsOutcome {
    state.exclusions.set(rules)
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

/// The browser path's status for the Settings row: capture and delivery
/// states, connected browsers, and what is held (counts only, never text).
/// `None` in a build without the browser host.
#[tauri::command]
pub fn browser_status() -> Option<serde_json::Value> {
    #[cfg(feature = "browser-host")]
    {
        crate::browser::status().and_then(|s| serde_json::to_value(s).ok())
    }
    #[cfg(not(feature = "browser-host"))]
    {
        None
    }
}

/// The owner discards browser pages still waiting to be sent. Returns the
/// number of snapshotted periods that remain; `None` without the browser host.
#[tauri::command]
pub fn browser_discard_waiting() -> Option<serde_json::Value> {
    #[cfg(feature = "browser-host")]
    {
        crate::browser::discard_waiting().map(|left| serde_json::json!({"left": left}))
    }
    #[cfg(not(feature = "browser-host"))]
    {
        None
    }
}

#[tauri::command]
pub fn open_browser_store(app: tauri::AppHandle, brand: String) -> Result<bool, String> {
    #[cfg(feature = "browser-host")]
    {
        use tauri_plugin_opener::OpenerExt;
        let Some(entry) = browser_host::stores::get(&brand) else {
            return Err("unknown browser".to_string());
        };
        // Only a browser Windows says is installed is launched by path; by bare
        // name, a missing one brings up Windows' own "cannot find" dialog first.
        if let Some(exe) = platform_win::app_paths::registered_executable(entry.executable) {
            let exe = exe.to_string_lossy().into_owned();
            if app.opener().open_url(entry.url, Some(exe)).is_ok() {
                return Ok(true);
            }
        }
        if app.opener().open_url(entry.url, None::<&str>).is_ok() {
            return Ok(true);
        }
        Ok(false)
    }
    #[cfg(not(feature = "browser-host"))]
    {
        let _ = (app, brand);
        Err("unknown browser".to_string())
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WizardStepResult {
    pub saved: bool,
    pub proof_generation: u64,
}

pub(crate) fn publish_setup(state: &crate::app::AppState) {
    let projection = if let Ok(auth) = state.setup.lock() {
        auth.projection()
    } else {
        return;
    };
    if let Ok(mut health) = state.health.lock() {
        health.setup = projection;
    }
}

/// Enable or disable a single capture source.
#[tauri::command]
pub fn set_source_enabled(
    state: tauri::State<'_, crate::app::AppState>,
    kind: observer_model::SourceKind,
    enabled: bool,
) -> Result<(), String> {
    {
        let mut auth = state
            .setup
            .lock()
            .map_err(|_| "failed to acquire setup lock")?;
        let running = auth.set_source(kind, enabled);
        let _ = state.commands.send(EngineCommand::SetSourceEnabled {
            kind,
            enabled: running,
        });
    }
    publish_setup(&state);
    Ok(())
}

/// Set the start-at-login preference and evaluate the login item decision.
#[tauri::command]
pub fn set_start_at_login(
    state: tauri::State<'_, crate::app::AppState>,
    enabled: bool,
) -> Result<(), String> {
    {
        let mut auth = state
            .setup
            .lock()
            .map_err(|_| "failed to acquire setup lock")?;
        let saved = auth.set_start_at_login(enabled);
        let login_failed = if saved || !enabled {
            crate::app::apply_login_decision(enabled)
        } else {
            true
        };
        auth.set_login_apply_failed(login_failed || !saved);
    }
    publish_setup(&state);
    Ok(())
}

/// Advance or change the first-run wizard step.
#[tauri::command]
pub fn set_wizard_step(
    state: tauri::State<'_, crate::app::AppState>,
    step: observer_model::setup::WizardStep,
) -> Result<WizardStepResult, String> {
    let saved = if let Ok(mut auth) = state.setup.lock() {
        auth.set_wizard_step(step)
    } else {
        return Err("failed to acquire setup lock".to_string());
    };
    let proof_generation = if let Ok(mut proof) = state.proof.lock() {
        if saved {
            proof.close()
        } else {
            proof.generation()
        }
    } else {
        0
    };
    if saved {
        let _ = state.commands.send(EngineCommand::ClearPreviews);
    }
    publish_setup(&state);
    Ok(WizardStepResult {
        saved,
        proof_generation,
    })
}

/// Dismiss the first-run sources notice on Settings Home.
#[tauri::command]
pub fn dismiss_notice(state: tauri::State<'_, crate::app::AppState>) -> Result<bool, String> {
    let saved = if let Ok(mut auth) = state.setup.lock() {
        auth.dismiss_notice()
    } else {
        return Err("failed to acquire setup lock".to_string());
    };
    publish_setup(&state);
    Ok(saved)
}

/// Retry persisting unsaved source choices or setup settings.
#[tauri::command]
pub fn retry_save(state: tauri::State<'_, crate::app::AppState>) -> Result<(), String> {
    {
        let mut auth = state
            .setup
            .lock()
            .map_err(|_| "failed to acquire setup lock")?;
        let outcome = auth.retry();
        if let Some(enabled) = auth.pending_login() {
            let login_failed = if outcome.saved || !enabled {
                crate::app::apply_login_decision(enabled)
            } else {
                true
            };
            auth.set_login_apply_failed(login_failed || !outcome.saved);
        }
        for kind in outcome.started {
            let _ = state.commands.send(EngineCommand::SetSourceEnabled {
                kind,
                enabled: true,
            });
        }
    }
    publish_setup(&state);
    Ok(())
}

/// Open or close the proof consumer gate for a specific source.
#[tauri::command]
pub fn set_proof_consumer(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::app::AppState>,
    source: Option<observer_model::SourceKind>,
) -> Result<u64, String> {
    let eligible = source.is_none() || crate::windows::settings_proof_eligible(&app);
    let generation = {
        let mut proof = state.proof.lock().map_err(|_| "proof state unavailable")?;
        match (source, eligible) {
            (Some(k), true) => proof.open(k),
            _ => proof.close(),
        }
    };
    let _ = state.commands.send(EngineCommand::ClearPreviews);
    if eligible {
        Ok(generation)
    } else {
        Err("proof window is not active".into())
    }
}

#[tauri::command]
pub fn pairing_prefill() -> String {
    let data_root = platform_win::local_data_root();
    if let Some(link) = pl_transport_win::draft::read_draft(&data_root) {
        return link;
    }
    if let Ok(Some(progress)) = pl_transport_win::handoff::read_active_progress(&data_root) {
        return progress.pairing_link;
    }
    String::new()
}

#[tauri::command]
pub async fn connect_native_handoff(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::app::AppState>,
    handoff_id: String,
    revision: u64,
) -> Result<(), String> {
    Box::pin(connect_native_handoff_surface(
        &app, &state, handoff_id, revision,
    ))
    .await
}

pub async fn connect_native_handoff_surface<S: crate::windows::JournalSurface>(
    surface: &S,
    state: &crate::app::AppState,
    handoff_id: String,
    revision: u64,
) -> Result<(), String> {
    let cfg = state.sync_config.clone();
    let sync = state.sync.clone();
    let journal_bridge = state.journal_bridge.clone();
    let setup_browser = state.setup_browser.clone();
    let shutdown_bridge = move || {
        let journal_bridge = journal_bridge.clone();
        let setup_browser = setup_browser.clone();
        async move {
            let bridge = journal_bridge
                .lock()
                .ok()
                .and_then(|mut bridge| bridge.take());
            if let Some(bridge) = bridge {
                bridge.shutdown_and_wait().await;
            }
            let s_bridge = setup_browser
                .lock()
                .ok()
                .and_then(|mut bridge| bridge.take());
            if let Some(s_bridge) = s_bridge {
                s_bridge.handle.shutdown_and_wait().await;
            }
        }
    };
    let _journal_open_guard = state.journal_open_lock.lock().await;
    let close_journal_window = {
        let surface_ref = surface;
        move || {
            surface_ref.close_journal();
            async move {}
        }
    };
    pl_transport_win::session::connect_native_handoff(
        &handoff_id,
        revision,
        &cfg,
        sync,
        &state.uploader_slot,
        &state.credential_access,
        &state.pairing_admission,
        shutdown_bridge,
        close_journal_window,
    )
    .await
    .map_err(|error| setup_transport_error("connect", &error))
}

#[tauri::command]
pub fn use_deferred_link(state: tauri::State<'_, crate::app::AppState>) -> Result<String, String> {
    let data_root = platform_win::local_data_root();
    let link = pl_transport_win::draft::take_deferred(&data_root)
        .map_err(|error| {
            tracing::warn!(target: "setup", reason = ?error.kind(), "setup link write failed");
            "setup_link_use_failed".to_string()
        })?
        .ok_or_else(|| "setup_link_use_failed".to_string())?;
    crate::app::reload_setup(&state);
    Ok(link)
}

#[tauri::command]
pub fn dismiss_deferred_link(state: tauri::State<'_, crate::app::AppState>) -> Result<(), String> {
    let data_root = platform_win::local_data_root();
    if pl_transport_win::draft::read_deferred(&data_root).is_some() {
        pl_transport_win::draft::dismiss_deferred(&data_root).map_err(|error| {
            tracing::warn!(target: "setup", reason = ?error.kind(), "setup link dismiss failed");
            "setup_link_dismiss_failed".to_string()
        })?;
    } else {
        pl_transport_win::draft::dismiss_rejected(&data_root).map_err(|error| {
            tracing::warn!(target: "setup", reason = ?error.kind(), "setup link dismiss failed");
            "setup_link_dismiss_failed".to_string()
        })?;
    }
    crate::app::reload_setup(&state);
    Ok(())
}

#[tauri::command]
pub fn retry_setup_link(state: tauri::State<'_, crate::app::AppState>) -> Result<(), String> {
    crate::app::reload_setup(&state);
    let snapshot = state
        .setup
        .lock()
        .map_err(|_| "setup unavailable")?
        .projection();
    let reason = if snapshot.handoff_persist_failed {
        Some("handoff_persist_failed")
    } else if snapshot.mailbox_busy {
        Some("handoff_busy")
    } else if snapshot.native_handoff_rejected {
        Some("handoff_rejected")
    } else if snapshot.handoff_deferred {
        Some("handoff_deferred")
    } else {
        None
    };
    if let Some(reason) = reason {
        return Err(reason.to_string());
    }
    Ok(())
}

fn setup_transport_error(action: &str, error: &pl_transport_win::TransportError) -> String {
    tracing::warn!(target: "setup", action, reason = %pl_transport_win::transport_error_code(error), "setup action failed");
    format!("setup_{action}_failed")
}

#[tauri::command]
pub async fn finish_connecting(
    state: tauri::State<'_, crate::app::AppState>,
    binding: String,
) -> Result<(), String> {
    let _journal_open = state.journal_open_lock.lock().await;
    pl_transport_win::session::finish_connecting(
        &binding,
        &state.sync_config,
        state.sync.clone(),
        &state.uploader_slot,
        &state.credential_access,
        &state.pairing_admission,
    )
    .await
    .map_err(|error| setup_transport_error("finish", &error))
}

#[tauri::command]
pub async fn open_setup_journal(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::app::AppState>,
    binding: String,
) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    open_setup_journal_with(&state, &binding, |url| {
        app.opener()
            .open_url(url, None::<&str>)
            .map_err(|error| error.to_string())
    })
    .await.map_err(|error| {
        tracing::warn!(target: "setup", reason = "setup_browser_failed", "setup browser action failed");
        if error == "setup progress could not be saved" { "setup_browser_completion_failed".to_string() }
        else { "setup_browser_failed".to_string() }
    })
}

/// The production browser action and its native lifecycle tests share this
/// composition. The only injected seam is the operating system URL opener.
pub(crate) async fn open_setup_journal_with<F>(
    state: &crate::app::AppState,
    expected_binding: &str,
    open_url: F,
) -> Result<(), String>
where
    F: FnOnce(&str) -> Result<(), String>,
{
    let _journal_open = state.journal_open_lock.lock().await;
    let _slot = state.uploader_slot.lock().await;
    let access = state.credential_access.lock().await;
    let access = access
        .as_ref()
        .ok_or_else(|| "no credential access".to_string())?;
    let live_binding = access
        .client_slot()
        .load()
        .journal_identity()
        .client_cert_sha256;
    if expected_binding.is_empty() || live_binding != expected_binding {
        return Err("setup connection changed".into());
    }
    let pairing = state
        .sync
        .lock()
        .map_err(|_| "poisoned".to_string())?
        .pairing
        .clone();
    if pairing.phase != observer_model::PairingPhase::Paired || pairing.binding != live_binding {
        return Err("not paired".into());
    }
    let step = state
        .setup
        .lock()
        .map_err(|_| "setup unavailable".to_string())?
        .projection()
        .wizard;
    if !matches!(
        step,
        observer_model::setup::WizardStep::Connection | observer_model::setup::WizardStep::Done
    ) {
        return Err("source setup is unfinished".into());
    }
    let paired =
        PairedState::load(&state.sync_config.state_path).map_err(|error| error.to_string())?;
    if paired.retirement_intent.is_some()
        || paired
            .setup_activation
            .as_ref()
            .is_none_or(|activation| activation.binding != live_binding)
        || paired.credential.as_ref().is_none_or(|credential| {
            pl_transport_win::ack::JournalIdentity::from_credential(credential).client_cert_sha256
                != live_binding
        })
    {
        return Err("setup connection changed".into());
    }
    let bridge_live = state
        .setup_browser
        .lock()
        .map_err(|_| "browser state unavailable")?
        .as_ref()
        .is_some_and(|browser| browser.binding == live_binding);
    if !paired.setup_browser_opened && !bridge_live {
        let handle = pl_transport_win::journal_bridge::start_with_facts(access.clone())
            .await
            .map_err(|error| format!("{error:?}"))?;
        #[cfg(test)]
        if let Some(probe) = &state.probe {
            probe.setup_bridge_started.notify_one();
            probe.release_setup_browser.notified().await;
        }
        let still_paired = state
            .sync
            .lock()
            .map(|sync| {
                sync.pairing.phase == observer_model::PairingPhase::Paired
                    && sync.pairing.binding == live_binding
            })
            .unwrap_or(false);
        if !still_paired || access.client_slot().refusal_stop().is_some() {
            handle.shutdown_and_wait().await;
            return Err("setup connection changed".into());
        }
        let url = handle.bootstrap_url();
        tracing::info!(target: "window", label = "setup_journal",
            url = %observer_log::journal_open::strip_cap(&url), "opening setup journal in browser");
        if let Err(error) = open_url(&url) {
            handle.shutdown_and_wait().await;
            return Err(error);
        }
        let old = state
            .setup_browser
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .replace(crate::app::SetupBrowser {
                binding: live_binding.clone(),
                handle,
            });
        if let Some(old) = old {
            old.handle.begin_shutdown();
        }
    }
    PairedState::mark_setup_browser_opened(&state.sync_config.state_path, &live_binding)
        .map_err(|error| error.to_string())?;
    finish_setup_after_browser(state)?;
    state
        .sync
        .lock()
        .map_err(|_| "poisoned".to_string())?
        .pairing
        .setup_browser_ready = false;
    Ok(())
}

fn finish_setup_after_browser(state: &crate::app::AppState) -> Result<(), String> {
    let saved = {
        let mut setup = state
            .setup
            .lock()
            .map_err(|_| "setup unavailable".to_string())?;
        setup.projection().wizard == observer_model::setup::WizardStep::Done
            || setup.set_wizard_step(observer_model::setup::WizardStep::Done)
    };
    publish_setup(state);
    if !saved {
        return Err("setup progress could not be saved".into());
    }
    if let Ok(mut proof) = state.proof.lock() {
        proof.close();
    }
    let _ = state.commands.send(EngineCommand::ClearPreviews);
    Ok(())
}

/// Recover a crash after the durable browser-open record but before setup.json.
/// This never opens a browser or repeats the source steps.
pub(crate) fn recover_setup_browser_completion(state: &crate::app::AppState) {
    let Ok(paired) = PairedState::load(&state.sync_config.state_path) else {
        return;
    };
    let Some(credential) = paired.credential.as_ref() else {
        return;
    };
    let binding =
        pl_transport_win::ack::JournalIdentity::from_credential(credential).client_cert_sha256;
    let step = state
        .setup
        .lock()
        .ok()
        .map(|setup| setup.projection().wizard);
    if !paired.setup_browser_opened
        || paired.retirement_intent.is_some()
        || paired
            .setup_activation
            .as_ref()
            .is_none_or(|activation| activation.binding != binding)
        || *state.sync_config.confirmation.lock().unwrap() != binding
        || step != Some(observer_model::setup::WizardStep::Connection)
    {
        return;
    }
    let failed = finish_setup_after_browser(state).is_err();
    if let Ok(mut sync) = state.sync.lock() {
        sync.pairing.setup_browser_ready = failed;
    }
}
