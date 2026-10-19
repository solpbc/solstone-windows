// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! App composition root.
//!
//! Wires the tray and the IPC command surface, ensures the per-user autostart
//! login item for the copy that owns it, then runs the tray-resident event
//! loop. No window is auto-shown; Settings/About are created on demand (see
//! [`crate::windows`]). The capture
//! engine is constructed here with the concrete platform sources injected at the
//! `observer-model` trait seam (`capture-wgc` / `capture-wasapi`), keeping the
//! engine itself Windows-agnostic.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use capture_engine::{CaptureEngine, EngineCommand, EngineConfig, Sources, SystemClock};
use observer_model::{
    control_client_exit_code, should_emit, AppPhase, HealthDump, PauseReason, SourceKind,
    SourceState, SyncSnapshot,
};
use pl_transport_win::service::SyncConfig;
use pl_transport_win::{CredentialAccess, UploaderSlot};
use tauri::{Emitter, Manager};
use tokio::sync::{mpsc, oneshot};

pub struct SetupBrowser {
    pub binding: String,
    pub handle: pl_transport_win::journal_bridge::JournalBridgeHandle,
}

pub struct AppState {
    pub commands: mpsc::UnboundedSender<EngineCommand>,
    pub health: Arc<Mutex<HealthDump>>,
    /// Wave-2 pairing/upload snapshot (shared with the engine + sync layer).
    pub sync: Arc<Mutex<SyncSnapshot>>,
    /// Startup-only raw Windows observation reused by About and native host.
    pub about_observation: observer_model::about::WindowsObservation,
    /// Static identity + paths the sync layer needs to pair/upload.
    pub sync_config: SyncConfig,
    pub _shutdown: Mutex<Option<oneshot::Sender<()>>>,
    pub engine_stopped: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    /// Single uploader lifecycle slot; also serializes pair/replace requests.
    pub uploader_slot: tokio::sync::Mutex<UploaderSlot>,
    /// The sole paired credential transport authority shared by upload and bridge.
    pub credential_access: tokio::sync::Mutex<Option<CredentialAccess>>,
    /// Serializes journal opens so parallel user triggers open one journal window,
    /// not two.
    pub journal_open_lock: Arc<tokio::sync::Mutex<()>>,
    /// The journal window's label per attempt, and the one that opened.
    pub journal_window: Mutex<journal_window::JournalWindowLabels>,
    /// Per-window loopback bridge backing the external journal window.
    pub journal_bridge: Arc<Mutex<Option<pl_transport_win::journal_bridge::JournalBridgeHandle>>>,
    pub setup_browser: Arc<Mutex<Option<SetupBrowser>>>,
    pub pairing_admission: std::sync::atomic::AtomicBool,
    /// Capture-exclusion rules controller (shared with the WGC screen source).
    pub exclusions: crate::exclusions::ExclusionController,
    pub setup: Arc<Mutex<observer_model::setup::SetupAuthority>>,
    pub proof: Arc<Mutex<capture_engine::ProofGate>>,
    #[cfg(test)]
    pub probe: Option<Arc<crate::windows::OpenPairProbe>>,
}

/// Drains engine shutdown and exits cleanly.
pub fn drain_and_exit(app: tauri::AppHandle, state: &AppState) {
    if let Ok(mut lock) = state._shutdown.lock() {
        if let Some(tx) = lock.take() {
            let _ = tx.send(());
        }
    }
    if let Ok(mut lock) = state.engine_stopped.lock() {
        if let Some(rx) = lock.take() {
            let _ = rx.recv_timeout(Duration::from_secs(3));
        }
    }
    app.exit(0);
}

/// The observer's device label for the pairing CSR, best-effort.
pub(crate) fn observer_hostname() -> String {
    std::env::var("COMPUTERNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "windows-observer".to_string())
}

// Hostname is resampled on each connection-lifecycle trigger; no OS hostname notification / polling exists.
fn current_raw_facts() -> pl_transport_win::RawDeviceFacts {
    pl_transport_win::RawDeviceFacts {
        name: Some(observer_hostname()),
        platform: Some("windows".to_string()),
        device_type: None,
        app_id: Some("app.solstone.windows".to_string()),
        app_version: Some(env!("CARGO_PKG_VERSION").to_string()),
    }
}

/// Ensure the per-user login item for the copy that owns it. Run on every launch
/// (not gated on a one-shot install signal) and idempotent: it writes only when
/// the entry is missing or stale, so it self-heals an unregistered install.
/// Velopack's locator — the same one the updater relies on — decides which copy
/// this is:
///
/// - **installed** (Setup / winget): always owns the entry;
/// Evaluate and apply the pure start-at-login decision for this copy.
/// Returns true if applying the decision failed (so the health snapshot
/// honestly reports `login_apply_failed = true`).
pub(crate) fn apply_login_decision(choice: bool) -> bool {
    use velopack::locator::{auto_locate_app_manifest, LocationContext};

    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            tracing::warn!(
                target: "lifecycle",
                component = "autostart",
                outcome = "current_exe_failed",
                error = %error,
                "autostart evaluate"
            );
            return true;
        }
    };

    let copy_class =
        match auto_locate_app_manifest(LocationContext::FromSpecifiedAppExecutable(exe.clone())) {
            Ok(locator) if locator.get_is_portable() => observer_model::setup::CopyClass::Portable,
            Ok(_) => observer_model::setup::CopyClass::Installed,
            Err(_) => observer_model::setup::CopyClass::Dev,
        };

    let installed_owner_exists = match installed_login_item_owner() {
        Ok(opt) => Some(opt.is_some()),
        Err(error) => {
            tracing::warn!(
                target: "lifecycle",
                component = "autostart",
                outcome = "installed_owner_check_failed",
                error = %error,
                "autostart evaluate"
            );
            None
        }
    };

    let registry_state =
        match platform_win::autostart::login_item_command(platform_win::autostart::LOGIN_ITEM_NAME)
        {
            Ok(Some(cmd)) => observer_model::setup::RegistryReadState::Present(cmd),
            Ok(None) => observer_model::setup::RegistryReadState::Absent,
            Err(_) => observer_model::setup::RegistryReadState::Unreadable,
        };

    let my_command =
        platform_win::autostart::run_command(&exe, &[observer_model::FROM_AUTOSTART_ARG]);

    let decision = observer_model::setup::evaluate_login_decision(
        copy_class,
        Some(choice),
        &registry_state,
        &my_command,
        installed_owner_exists,
    );

    match decision {
        observer_model::setup::LoginDecision::Ensure => {
            match platform_win::autostart::ensure_login_item(
                platform_win::autostart::LOGIN_ITEM_NAME,
                &exe,
                &[observer_model::FROM_AUTOSTART_ARG],
            ) {
                Ok(platform_win::autostart::EnsureOutcome::Registered) => {
                    tracing::info!(
                        target: "lifecycle",
                        component = "autostart",
                        outcome = "registered",
                        "autostart ensure"
                    );
                    false
                }
                Ok(platform_win::autostart::EnsureOutcome::AlreadyCurrent) => {
                    tracing::info!(
                        target: "lifecycle",
                        component = "autostart",
                        outcome = "already_current",
                        "autostart ensure"
                    );
                    false
                }
                Err(error) => {
                    tracing::warn!(
                        target: "lifecycle",
                        component = "autostart",
                        outcome = "failed",
                        error = %error,
                        "autostart ensure"
                    );
                    true
                }
            }
        }
        observer_model::setup::LoginDecision::Remove => {
            match platform_win::autostart::remove_login_item_if_matches(
                platform_win::autostart::LOGIN_ITEM_NAME,
                &exe,
                &[observer_model::FROM_AUTOSTART_ARG],
            ) {
                Ok(removed) => {
                    tracing::info!(
                        target: "lifecycle",
                        component = "autostart",
                        outcome = if removed { "removed" } else { "already_removed" },
                        "autostart remove"
                    );
                    false
                }
                Err(error) => {
                    tracing::warn!(
                        target: "lifecycle",
                        component = "autostart",
                        outcome = "failed",
                        error = %error,
                        "autostart remove"
                    );
                    true
                }
            }
        }
        observer_model::setup::LoginDecision::Leave => {
            tracing::info!(
                target: "lifecycle",
                component = "autostart",
                outcome = "leave",
                "autostart leave"
            );
            false
        }
        observer_model::setup::LoginDecision::Failed => {
            tracing::warn!(
                target: "lifecycle",
                component = "autostart",
                outcome = "failed",
                "autostart decision failed"
            );
            true
        }
    }
}

fn apply_scheme_decision() {
    use velopack::locator::{auto_locate_app_manifest, LocationContext};

    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let copy_class =
        match auto_locate_app_manifest(LocationContext::FromSpecifiedAppExecutable(exe.clone())) {
            Ok(locator) if locator.get_is_portable() => observer_model::setup::CopyClass::Portable,
            Ok(_) => observer_model::setup::CopyClass::Installed,
            Err(_) => observer_model::setup::CopyClass::Dev,
        };

    let Ok(current) = platform_win::scheme::read_scheme_command() else {
        return;
    };

    match observer_model::scheme::scheme_launch_decision(copy_class, current.as_deref(), &exe) {
        observer_model::scheme::SchemeLaunchDecision::Ensure => {
            if let Some(cmd) = observer_model::scheme::scheme_open_command(&exe, "%1") {
                let _ = platform_win::scheme::ensure_scheme_handler(&cmd);
            }
        }
        observer_model::scheme::SchemeLaunchDecision::Leave => {}
    }
}

fn fill_accepted_progress(
    overlay: &mut observer_model::HandoffOverlay,
    progress: &pl_transport_win::handoff::SetupHandoffProgress,
    now_ms: u64,
) {
    overlay.handoff_id = Some(progress.handoff_id.clone());
    overlay.handoff_revision = Some(progress.accepted_unix_ms);
    overlay.handoff_expired = progress.is_expired(now_ms);
}

pub(crate) fn update_handoff_overlay(
    data_root: &std::path::Path,
    sync: &Arc<Mutex<observer_model::SyncSnapshot>>,
    setup: &Arc<Mutex<observer_model::setup::SetupAuthority>>,
    pairing_admission: &std::sync::atomic::AtomicBool,
) {
    if observer_model::setup::SETUP_SEQUENCE_ENABLED {
        update_handoff_overlay_enabled(data_root, sync, setup, pairing_admission);
    }
}

pub(crate) fn update_handoff_overlay_enabled(
    data_root: &std::path::Path,
    sync: &Arc<Mutex<observer_model::SyncSnapshot>>,
    setup: &Arc<Mutex<observer_model::setup::SetupAuthority>>,
    pairing_admission: &std::sync::atomic::AtomicBool,
) {
    let outcome = pl_transport_win::handoff::process_mailbox_live(
        data_root,
        || {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64
        },
        || {
            pairing_admission.load(std::sync::atomic::Ordering::Acquire)
                || matches!(
                    sync.lock().unwrap().pairing.phase,
                    observer_model::PairingPhase::Pairing
                        | observer_model::PairingPhase::AwaitingConfirmation
                )
        },
    );
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let progress = pl_transport_win::handoff::read_active_progress(data_root);
    let mut overlay = observer_model::HandoffOverlay {
        has_deferred_link: pl_transport_win::draft::read_deferred(data_root).is_some(),
        handoff_rejected: pl_transport_win::draft::deferred_rejected(data_root),
        handoff_persist_failed: progress.is_err(),
        ..observer_model::HandoffOverlay::default()
    };
    let readable = progress
        .as_ref()
        .ok()
        .and_then(|progress| progress.as_ref());
    match &outcome {
        Err(_) => {
            overlay.handoff_persist_failed = true;
            if let Some(progress) = readable {
                fill_accepted_progress(&mut overlay, progress, now_ms);
            }
        }
        Ok(pl_transport_win::handoff::MailboxOutcome::Busy) => {
            overlay.mailbox_busy = true;
            if let Some(progress) = readable {
                fill_accepted_progress(&mut overlay, progress, now_ms);
            }
        }
        Ok(pl_transport_win::handoff::MailboxOutcome::Rejected) => {
            overlay.native_handoff_rejected = true;
            if let Some(progress) = readable {
                fill_accepted_progress(&mut overlay, progress, now_ms);
            }
        }
        Ok(pl_transport_win::handoff::MailboxOutcome::Deferred) => {
            overlay.handoff_deferred = true;
            if let Some(progress) = readable {
                fill_accepted_progress(&mut overlay, progress, now_ms);
            }
        }
        Ok(
            pl_transport_win::handoff::MailboxOutcome::Accepted
            | pl_transport_win::handoff::MailboxOutcome::Replay
            | pl_transport_win::handoff::MailboxOutcome::Absent,
        ) => {
            if let Some(progress) = readable {
                fill_accepted_progress(&mut overlay, progress, now_ms);
            }
        }
    }
    if let Some(progress) = readable {
        // This is a renderer hint only: the action revalidates admission and
        // credential authority. A completed H1 must not hide a distinct H2,
        // while unreadable or inconsistent saved authority stays fail-closed.
        match pl_transport_win::PairedState::load(
            &data_root.join(pl_transport_win::credential::PAIRED_STATE_FILE_NAME),
        ) {
            Ok(paired) if paired.retirement_intent.is_none() => {
                match paired.setup_activation.as_ref() {
                    None => overlay.handoff_new_connection = true,
                    Some(activation) => {
                        let bound = paired.credential.as_ref().is_some_and(|credential| {
                            pl_transport_win::ack::JournalIdentity::from_credential(credential)
                                .client_cert_sha256
                                == activation.binding
                        });
                        if bound {
                            overlay.handoff_new_connection =
                                activation.handoff_id != progress.handoff_id;
                            // The handoff this pairing came from, once its journal has
                            // opened, is finished: nothing is left to continue.
                            if !overlay.handoff_new_connection && paired.setup_browser_opened {
                                overlay.handoff_id = None;
                                overlay.handoff_revision = None;
                                overlay.handoff_expired = false;
                            }
                        } else {
                            overlay.handoff_persist_failed = true;
                        }
                    }
                }
            }
            Ok(_) => {}
            Err(_) => overlay.handoff_persist_failed = true,
        }
    }
    setup.lock().unwrap().set_handoff_overlay(overlay);
}

pub(crate) fn reload_setup(state: &AppState) {
    let data_root = platform_win::local_data_root();
    update_handoff_overlay(
        &data_root,
        &state.sync,
        &state.setup,
        &state.pairing_admission,
    );
}

/// This executable, when it is the installed (Setup) copy: the only copy that
/// writes the browsers' native-messaging registration, so a portable or dev
/// build never points a browser at itself.
#[cfg(feature = "browser-host")]
fn installed_copy_exe() -> Option<std::path::PathBuf> {
    use velopack::locator::{auto_locate_app_manifest, LocationContext};

    let exe = std::env::current_exe().ok()?;
    let locator =
        auto_locate_app_manifest(LocationContext::FromSpecifiedAppExecutable(exe.clone())).ok()?;
    (!locator.get_is_portable()).then_some(exe)
}

/// The installed (non-portable) copy the login item currently names, if that
/// executable still exists. An unreadable entry is an error, never "no owner",
/// so a portable copy does not overwrite what it could not read.
fn installed_login_item_owner() -> std::io::Result<Option<std::path::PathBuf>> {
    use velopack::locator::{auto_locate_app_manifest, LocationContext};

    let Some(command) =
        platform_win::autostart::login_item_command(platform_win::autostart::LOGIN_ITEM_NAME)?
    else {
        return Ok(None);
    };
    let Some(exe) = platform_win::autostart::login_item_executable(&command) else {
        return Ok(None);
    };
    let installed = exe.is_file()
        && auto_locate_app_manifest(LocationContext::FromSpecifiedAppExecutable(exe.clone()))
            .is_ok_and(|locator| !locator.get_is_portable());
    Ok(installed.then_some(exe))
}

/// Build the sync config from the per-user data layout + the engine's rotation
/// period (so the uploader derives the same segment keys the writer sealed).
fn build_sync_config() -> SyncConfig {
    let host = observer_hostname();
    let journal_version_path = platform_win::local_data_root().join("journal-version.json");
    let state_path =
        platform_win::local_data_root().join(pl_transport_win::credential::PAIRED_STATE_FILE_NAME);
    SyncConfig {
        device_label: host,
        period_secs: EngineConfig::default().segment_secs,
        state_path,
        segments_root: platform_win::segments_dir(),
        local_offset: Arc::new(platform_win::WindowsLocalOffset),
        journal_version: Arc::new(pl_transport_win::JournalVersionController::new(
            journal_version_path,
        )),
        facts_fn: Arc::new(current_raw_facts),
        confirmation: Arc::new(Mutex::new(String::new())),
        tombstone: Arc::new(Mutex::new(None)),
        #[cfg(feature = "awaiting-hold")]
        awaiting_hold: None,
    }
}

fn app_phase_label(phase: AppPhase) -> &'static str {
    phase.into()
}

fn source_kind_label(kind: SourceKind) -> &'static str {
    kind.into()
}

fn source_state_label(state: &SourceState) -> &'static str {
    match state {
        SourceState::Active => "active",
        SourceState::Inactive => "inactive",
        SourceState::NoInputDevice => "no_input_device",
        SourceState::Faulted { .. } => "faulted",
    }
}

fn log_health_transitions(previous: Option<&HealthDump>, current: &HealthDump) {
    let Some(previous) = previous else {
        return;
    };

    if previous.app_state != current.app_state {
        tracing::info!(
            target: "health",
            from = app_phase_label(previous.app_state),
            to = app_phase_label(current.app_state),
            "app phase changed"
        );
    }

    for source in &current.sources {
        let previous_state = previous
            .sources
            .iter()
            .find(|previous| previous.kind == source.kind)
            .map(|previous| &previous.state);
        if previous_state == Some(&source.state) {
            continue;
        }

        let from = previous_state.map(source_state_label).unwrap_or("missing");
        let to = source_state_label(&source.state);
        match &source.state {
            SourceState::Faulted { reason, .. } => tracing::warn!(
                target: "health",
                source = source_kind_label(source.kind),
                from,
                to,
                reason = ?reason,
                "source state changed"
            ),
            _ => tracing::info!(
                target: "health",
                source = source_kind_label(source.kind),
                from,
                to,
                "source state changed"
            ),
        }
    }
}

// Keep opening in Rust. The plugin's default script cancels new-window anchor navigation
// before the Journal's native callbacks, then calls an IPC permission that the
// remote dashboard does not have. The shell's fixed-URL commands also use Rust.
pub(crate) fn native_opener_plugin() -> impl tauri::plugin::Plugin<tauri::Wry> {
    tauri_plugin_opener::Builder::new()
        .open_js_links_on_click(false)
        .build()
}

/// Boot the tray-resident observer.
pub fn run(
    open_view: Option<observer_model::View>,
    surface_on_launch: bool,
    open_journal_on_launch: bool,
    reload_setup: bool,
) {
    tracing::info!(
        target: "lifecycle",
        version = env!("CARGO_PKG_VERSION"),
        build = if cfg!(debug_assertions) { "debug" } else { "release" },
        "boot"
    );

    let app = tauri::Builder::default()
        .manage(crate::logs::LogExports::default())
        // Backend-only browser/folder access. Bundled renderers use fixed-destination
        // commands; the Journal uses native origin-based navigation callbacks.
        // No opener:* permission is granted to either webview.
        .plugin(native_opener_plugin())
        .invoke_handler(tauri::generate_handler![
            crate::ipc::quit_and_drain,
            crate::ipc::pause,
            crate::ipc::resume,
            crate::ipc::get_health,
            crate::logs::read_recent_logs,
            crate::logs::save_recent_logs,
            crate::ipc::open_settings,
            crate::ipc::open_about,
            crate::ipc::open_journal,
            crate::ipc::log_frontend_error,
            crate::ipc::view_rendered,
            crate::ipc::pair,
            crate::ipc::get_exclusions,
            crate::ipc::set_exclusions,
            crate::ipc::list_running_apps,
            crate::ipc::get_hotkey,
            crate::ipc::set_hotkey,
            crate::ipc::get_mic_config,
            crate::ipc::set_mic_config,
            crate::ipc::list_mic_devices,
            crate::ipc::update_get,
            crate::ipc::update_check_now,
            crate::ipc::update_download,
            crate::ipc::update_install,
            crate::ipc::update_dismiss,
            crate::ipc::update_set_auto_check,
            crate::ipc::update_set_auto_download,
            crate::ipc::update_set_interval,
            crate::ipc::open_release_notes,
            crate::ipc::storage_info,
            crate::ipc::open_storage_folder,
            crate::ipc::answer_pairing,
            crate::ipc::pairing_migration_state,
            crate::ipc::pairing_migration_decide,
            crate::ipc::pairing_migration_devices,
            crate::ipc::pairing_migration_offer_dismiss,
            crate::ipc::browser_status,
            crate::ipc::open_browser_store,
            crate::ipc::browser_discard_waiting,
            crate::ipc::set_source_enabled,
            crate::ipc::set_desktop_activity_window_titles,
            crate::ipc::set_desktop_activity_page_addresses,
            crate::ipc::set_start_at_login,
            crate::ipc::set_wizard_step,
            crate::ipc::dismiss_notice,
            crate::ipc::retry_save,
            crate::ipc::set_proof_consumer,
            crate::ipc::pairing_prefill,
            crate::ipc::finish_connecting,
            crate::ipc::open_setup_journal,
            crate::ipc::retry_setup_link,
            crate::ipc::connect_native_handoff,
            crate::ipc::use_deferred_link,
            crate::ipc::dismiss_deferred_link,
        ])
        .setup(move |app| {
            match crate::lifecycle::acquire_single_instance() {
                platform_win::InstanceLock::AlreadyRunning => {
                    tracing::info!(
                        target: "lifecycle",
                        outcome = "already_running",
                        "single instance"
                    );
                    let acknowledged = if reload_setup {
                        crate::control::signal_reload_setup()
                    } else if open_journal_on_launch {
                        crate::control::signal_open_journal()
                    } else if let Some(view) = open_view {
                        // `--open-view` has to mean the same thing whether or not
                        // the app is already running. This branch used to fall
                        // through to the surface verb, so `--open-view about`
                        // against a live instance opened Settings and reported
                        // nothing wrong -- the flag lied rather than failed.
                        match view {
                            observer_model::View::Settings => crate::control::signal_surface(),
                            observer_model::View::About => crate::control::signal_surface_about(),
                        }
                    } else if surface_on_launch {
                        crate::control::signal_surface()
                    } else {
                        true
                    };
                    // The pinned Wry runtime turns RequestExit(code) into
                    // ControlFlow::Exit, losing a nonzero acknowledgement
                    // result. This instance owns no engine or capture tasks:
                    // clean up Tauri before returning the exact client result.
                    app.handle().cleanup_before_exit();
                    std::process::exit(control_client_exit_code(acknowledged));
                }
                platform_win::InstanceLock::Acquired => {
                    tracing::info!(
                        target: "lifecycle",
                        outcome = "acquired",
                        "single instance"
                    );
                }
            }

            // Only the owning instance may classify setup or change login registration.
            let data_root = platform_win::local_data_root();
            let load_outcome = observer_model::setup::load_for_startup(
                &data_root, observer_model::setup::SETUP_SEQUENCE_ENABLED,
            );
            let config = match load_outcome {
                observer_model::setup::LoadOutcome::Abort => {
                    tracing::warn!(
                        target: "lifecycle",
                        component = "setup",
                        outcome = "abort",
                        "setup classification aborted"
                    );
                    return Ok(());
                }
                observer_model::setup::LoadOutcome::Fresh(cfg)
                | observer_model::setup::LoadOutcome::Legacy(cfg)
                | observer_model::setup::LoadOutcome::Valid(cfg)
                | observer_model::setup::LoadOutcome::FailedClosed(cfg)
                | observer_model::setup::LoadOutcome::LegacyUnsaved(cfg) => cfg,
            };

            let login_apply_failed = apply_login_decision(config.start_at_login);
            if observer_model::setup::SETUP_SEQUENCE_ENABLED {
                apply_scheme_decision();
            }

            // Capture-exclusion rules: load persisted owner policy and share the
            // handle with the WGC source so edits take effect on the next frame.
            let exclusions = crate::exclusions::ExclusionController::new(
                platform_win::local_data_root().join("exclusions.json"),
            );

            // Microphone controls: load persisted device priority/disable/gain and
            // share the handles with the WASAPI mic source (it reconciles the
            // selected device + gain live and publishes the open device id back).
            let mic =
                crate::mic::MicController::new(platform_win::local_data_root().join("mic.json"));

            let sources = Sources {
                screen: Box::new(capture_wgc::WgcScreenSource::new(exclusions.rules_handle())),
                screen_encoder: Box::new(capture_screen_encode::MfScreenEncoder::new()),
                system_audio: Box::new(capture_wasapi::WasapiSystemAudioSource::new()),
                mic: Box::new(capture_wasapi::WasapiMicSource::new(
                    mic.config_handle(),
                    mic.active_handle(),
                )),
                desktop_activity: Box::new(platform_win::DesktopActivityProbe::new()),
            };
            let engine_config = EngineConfig::default();
            let mut recovery =
                platform_win::LocalRecoveryFs::new(platform_win::segments_dir(), engine_config.segment_secs);
            let segment_fs = platform_win::LocalSegmentFs::default();
            tracing::info!(target: "engine", operation = "start", "engine start");
            let (mut engine, _outcomes) = CaptureEngine::new(
                sources,
                engine_config,
                &mut recovery,
                segment_fs,
                Box::new(SystemClock),
            )?;
            let health = engine.health_handle();
            let sync = engine.sync_handle();
            let about_observation = platform_win::about_observation::windows_about_observation();
            let about_app_line = observer_model::about::render_windows_app_line(
                env!("CARGO_PKG_VERSION"),
                &about_observation,
            );
            if let Ok(mut snapshot) = sync.lock() {
                snapshot.about_app_line = about_app_line;
                if snapshot.journal_display_line.is_empty() {
                    snapshot.journal_display_line = observer_model::about::unknown_journal_line();
                }
                snapshot.about_block = observer_model::about::compose_about_block(
                    &snapshot.about_app_line,
                    &snapshot.journal_display_line,
                );
            }
            if observer_model::setup::SETUP_SEQUENCE_ENABLED {
                engine.set_setup_root(data_root.clone());
            }
            let mut setup_snapshot = config.to_snapshot();
            setup_snapshot.login_apply_failed = login_apply_failed;
            engine.set_source_choices(setup_snapshot);

            // The owner's pause outlives a restart: restore it before any source
            // can start, so a held pause captures nothing at launch.
            engine.hold_owner_pause_in(Box::new(capture_engine::FilePauseStore::new(
                platform_win::local_data_root().join("pause.txt"),
            )));
            engine.set_exclusion_rules_handle(exclusions.rules_handle());
            engine.set_exclusion_availability_handle(exclusions.availability_handle());
            engine.start();
            tracing::info!(target: "engine", outcome = "started", "engine start");

            let watch_rx = engine.health_watch();
            let preview_rx = engine.preview_watch();
            tauri::async_runtime::spawn({
                let app = app.handle().clone();
                let mut rx = preview_rx;
                async move {
                    while rx.changed().await.is_ok() {
                        let snapshot = rx.borrow_and_update().clone();
                        if !crate::windows::settings_proof_eligible(&app) {
                            crate::windows::revoke_settings_proof(&app, false);
                            continue;
                        }
                        let _ = app.emit_to("settings", "preview://changed", &snapshot);
                    }
                }
            });
            let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
            let (shutdown_tx, shutdown_rx) = oneshot::channel();
            let (engine_stopped_tx, engine_stopped_rx) = std::sync::mpsc::channel();

            // Resume an existing pairing: if a credential is on disk, start the
            // upload loop now. A fresh pairing is started by the
            // `pair` IPC command instead.
            let sync_config = build_sync_config();
            let mut slot = UploaderSlot::new();
            let credential_access = tauri::async_runtime::block_on(
                pl_transport_win::service::launch_resume(&sync_config, &sync, &mut slot),
            );
            let pairing_admission = std::sync::atomic::AtomicBool::new(false);
            update_handoff_overlay(&data_root, &sync, &engine.setup_authority(), &pairing_admission);

            app.manage(AppState {
                commands: cmd_tx.clone(),
                health: health.clone(),
                sync: sync.clone(),
                setup: engine.setup_authority(),
                proof: engine.proof_gate(),
                about_observation: about_observation.clone(),
                sync_config,
                _shutdown: Mutex::new(Some(shutdown_tx)),
                engine_stopped: Mutex::new(Some(engine_stopped_rx)),
                uploader_slot: tokio::sync::Mutex::new(slot),
                credential_access: tokio::sync::Mutex::new(credential_access),
                journal_open_lock: Arc::new(tokio::sync::Mutex::new(())),
                journal_window: Mutex::new(Default::default()),
                journal_bridge: Arc::new(Mutex::new(None)),
                setup_browser: Arc::new(Mutex::new(None)),
                pairing_admission,
                exclusions,
                #[cfg(test)]
                probe: None,
            });
            if observer_model::setup::SETUP_SEQUENCE_ENABLED {
                crate::ipc::recover_setup_browser_completion(&app.state::<AppState>());
            }

            // The browser extension's native host (compiled in only with the
            // `browser-host` feature): endpoint, custody, gate and delivery.
            #[cfg(feature = "browser-host")]
            crate::browser::start(app.handle().clone(), installed_copy_exe());

            // In-app updater: construct the Velopack-backed controller (honest
            // state earned from the feed, persisted next to pairing.json),
            // rehydrate any staged-pending-restart update, and start the owned
            // background-check timer. Manager construction fails cleanly off a
            // Velopack install (dev tree) -> surfaced as the honest "unavailable".
            let updater = crate::update::UpdateController::new(
                app.handle().clone(),
                platform_win::local_data_root().join("update.json"),
            );
            app.manage(updater.clone());
            updater.spawn_timer();

            // Global pause/resume hotkey: load the owner's persisted combo and
            // share its handles with the notification pump, which owns the Win32
            // registration (it must live on the pump's message-loop thread) and
            // reports the honest outcome back for Settings to render.
            let hotkey = crate::hotkey::HotkeyController::new(
                platform_win::local_data_root().join("hotkey.json"),
            );
            app.manage(hotkey.clone());
            app.manage(mic.clone());

            let (tray, pause_submenu, mi_resume, mi_restart, mi_browsers) =
                crate::tray::init(app, cmd_tx.clone())?;

            tauri::async_runtime::spawn(async move {
                let exit = engine.run(shutdown_rx, cmd_rx).await;
                tracing::info!(target: "engine", reason = ?exit, "capture engine loop exited");
                let _ = engine_stopped_tx.send(());
            });

            tauri::async_runtime::spawn({
                let health = health.clone();
                async move {
                    match tokio::net::TcpListener::bind((
                        observer_contract::LOOPBACK_HOST,
                        observer_contract::HEALTH_PORT,
                    ))
                    .await
                    {
                        Ok(listener) => {
                            if let Err(error) = capture_engine::serve_health(listener, health.clone()).await
                            {
                                tracing::error!(
                                    target: "health",
                                    error = %error,
                                    "health server exited"
                                );
                                crate::health::record_listener_fault(
                                    &health,
                                    crate::health::ListenerSlot::Health,
                                    format!("serve: {error}"),
                                );
                            }
                        }
                        Err(error) => {
                            tracing::error!(
                                target: "health",
                                port = observer_contract::HEALTH_PORT,
                                error = %error,
                                "health server bind failed"
                            );
                            crate::health::record_listener_fault(
                                &health,
                                crate::health::ListenerSlot::Health,
                                format!("bind: {error}"),
                            );
                        }
                    }
                }
            });

            tauri::async_runtime::spawn({
                let app = app.handle().clone();
                let health = health.clone();
                async move {
                    match tokio::net::TcpListener::bind((
                        observer_contract::LOOPBACK_HOST,
                        observer_contract::CONTROL_PORT,
                    ))
                    .await
                    {
                        Ok(listener) => {
                            if let Err(error) = crate::control::serve(app, listener).await {
                                tracing::error!(
                                    target: "control",
                                    error = %error,
                                    "control server exited"
                                );
                                crate::health::record_listener_fault(
                                    &health,
                                    crate::health::ListenerSlot::Control,
                                    format!("serve: {error}"),
                                );
                            }
                        }
                        Err(error) => {
                            tracing::error!(
                                target: "control",
                                port = observer_contract::CONTROL_PORT,
                                error = %error,
                                "control server bind failed"
                            );
                            crate::health::record_listener_fault(
                                &health,
                                crate::health::ListenerSlot::Control,
                                format!("bind: {error}"),
                            );
                        }
                    }
                }
            });

            tauri::async_runtime::spawn({
                let app = app.handle().clone();
                let health = health.clone();
                let tray = tray.clone();
                let pause_submenu = pause_submenu.clone();
                let mi_resume = mi_resume.clone();
                let mi_restart = mi_restart.clone();
                let mi_browsers = mi_browsers.clone();
                let mut rx = watch_rx;
                async move {
                    let mut previous: Option<HealthDump> = None;
                    loop {
                        let dump = rx.borrow_and_update().clone();
                        log_health_transitions(previous.as_ref(), &dump);
                        let changed = previous
                            .as_ref()
                            .map_or(true, |prev| should_emit(prev, &dump));
                        previous = Some(dump.clone());
                        crate::tray::apply_state(
                            &tray,
                            &pause_submenu,
                            &mi_resume,
                            &mi_restart,
                            mi_browsers.as_ref(),
                            &dump,
                        );
                        if changed {
                            let _ = app.emit("health://changed", &dump);
                        }
                        if rx.changed().await.is_err() {
                            break;
                        }
                    }

                    let sources = health
                        .lock()
                        .map(|health| health.sources.clone())
                        .unwrap_or_default();
                    let sync = health
                        .lock()
                        .map(|health| health.sync.clone())
                        .unwrap_or_default();
                    let exclusions = health
                        .lock()
                        .ok()
                        .and_then(|health| health.exclusions.clone());
                    let views = health
                        .lock()
                        .map(|health| health.views.clone())
                        .unwrap_or_default();
                    let pump_degraded = health
                        .lock()
                        .map(|health| health.pump_degraded)
                        .unwrap_or_default();
                    let listener_faults = health
                        .lock()
                        .map(|health| health.listener_faults.clone())
                        .unwrap_or_default();
                    let setup = health
                        .lock()
                        .map(|health| health.setup.clone())
                        .unwrap_or_default();
                    let terminal = HealthDump {
                        app_state: AppPhase::Error,
                        sources,
                        frame_rate: None,
                        segment_dir: None,
                        segment_seconds_remaining: None,
                        engine_ready: false,
                        version: env!("CARGO_PKG_VERSION").to_string(),
                        sync,
                        screen_encoder: None,
                        exclusions,
                        storage: None,
                        pause: None,
                        views,
                        pump_degraded,
                        listener_faults,
                        setup,
                    };
                    log_health_transitions(previous.as_ref(), &terminal);
                    if let Ok(mut health) = health.lock() {
                        *health = terminal.clone();
                    }
                    crate::tray::apply_state(
                        &tray,
                        &pause_submenu,
                        &mi_resume,
                        &mi_restart,
                        mi_browsers.as_ref(),
                        &terminal,
                    );
                    let _ = app.emit("health://changed", &terminal);
                }
            });

            std::thread::spawn({
                let cmd_tx = cmd_tx.clone();
                let health = health.clone();
                let hotkey_desired = hotkey.desired_handle();
                let hotkey_outcome = hotkey.outcome_handle();
                move || {
                    let mut pump =
                        platform_win::NotificationPump::with_hotkey(hotkey_desired, hotkey_outcome);
                    platform_win::mark_pump_registration_finished(!pump.is_degraded());
                    if pump.is_degraded() {
                        platform_win::set_pump_degraded(true);
                    }
                    if let Ok(mut h) = health.lock() {
                        h.pump_degraded = pump.is_degraded();
                    }
                    loop {
                        for notification in pump.poll() {
                            let command = match notification {
                                platform_win::SystemNotification::SessionLocked => {
                                    Some(EngineCommand::Pause {
                                        reason: PauseReason::SessionLocked,
                                        duration_secs: None,
                                    })
                                }
                                platform_win::SystemNotification::Suspending => {
                                    Some(EngineCommand::Pause {
                                        reason: PauseReason::SystemSuspending,
                                        duration_secs: None,
                                    })
                                }
                                // Ends the lock/suspend pause only; an owner's
                                // pause holds until the owner resumes.
                                platform_win::SystemNotification::SessionUnlocked
                                | platform_win::SystemNotification::Resumed => {
                                    if platform_win::is_session_eligible() {
                                        Some(EngineCommand::SystemResume)
                                    } else {
                                        Some(EngineCommand::Pause {
                                            reason: PauseReason::SessionLocked,
                                            duration_secs: None,
                                        })
                                    }
                                }
                                platform_win::SystemNotification::DisplayChanged => {
                                    Some(EngineCommand::DisplayChanged)
                                }
                                // The owner pressed the global hotkey -> toggle.
                                platform_win::SystemNotification::HotkeyPressed => {
                                    Some(EngineCommand::TogglePause)
                                }
                            };
                            if let Some(command) = command {
                                let _ = cmd_tx.send(command);
                            }
                        }
                        std::thread::sleep(Duration::from_millis(250));
                    }
                }
            });

            // Honor `--open-view` only in the mutex-holding instance (we're past the
            // AlreadyRunning early-return). No explicit view surfaces Settings only
            // for user-visible launches; autostart stays tray-first.
            let handle = app.handle();
            if open_journal_on_launch {
                let handle = handle.clone();
                tauri::async_runtime::spawn(async move {
                    let state = handle.state::<crate::app::AppState>();
                    open_journal_from_launch(&state, &handle, Some(&handle)).await;
                });
            } else {
                match open_view {
                Some(view) => {
                    let result = match view {
                        observer_model::View::Settings => crate::windows::open_settings(handle),
                        observer_model::View::About => crate::windows::open_about(handle),
                    };
                    if let Err(error) = result {
                        tracing::warn!(target: "window", view = view.label(), error = %error, "open-view failed");
                    }
                }
                None if surface_on_launch => {
                    if let Err(error) = crate::windows::open_settings(handle) {
                        tracing::warn!(target: "window", view = "settings", error = %error, "launch-surface failed");
                    }
                }
                None => {}
                }
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building the observer");

    app.run(|_, event| {
        if let tauri::RunEvent::ExitRequested { code, api, .. } = event {
            if code.is_none() {
                api.prevent_exit();
            }
        }
    });
}

pub(crate) async fn open_journal_from_launch<S: crate::windows::JournalSurface>(
    state: &crate::app::AppState,
    surface: &S,
    app: Option<&tauri::AppHandle>,
) {
    if let Err(error) = crate::windows::open_journal(state, surface, app).await {
        tracing::warn!(
            target: "window",
            label = "journal",
            error = error.token(),
            "open-journal failed"
        );
    }
}
