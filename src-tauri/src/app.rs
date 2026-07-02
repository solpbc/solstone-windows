// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! App composition root.
//!
//! Wires the tray and the IPC command surface, ensures the per-user autostart
//! login item, then runs the tray-resident event loop. No window is auto-shown;
//! Settings/About are created on demand (see [`crate::windows`]). The capture
//! engine is constructed here with the concrete platform sources injected at the
//! `observer-model` trait seam (`capture-wgc` / `capture-wasapi`), keeping the
//! engine itself Windows-agnostic.

use std::io;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use capture_engine::{CaptureEngine, EngineCommand, EngineConfig, Sources, SystemClock};
use observer_model::{
    should_emit, AppPhase, HealthDump, PauseReason, SourceKind, SourceState, SyncSnapshot,
};
use observer_retention::RetentionConfig;
use pl_transport_win::credential::PairedState;
use pl_transport_win::service::{run_uploader, SyncConfig};
use tauri::{Emitter, Manager};
use tokio::sync::{mpsc, oneshot};

pub struct AppState {
    pub commands: mpsc::UnboundedSender<EngineCommand>,
    pub health: Arc<Mutex<HealthDump>>,
    /// Wave-2 pairing/upload snapshot (shared with the engine + sync layer).
    pub sync: Arc<Mutex<SyncSnapshot>>,
    /// Static identity + paths the sync layer needs to pair/upload.
    pub sync_config: SyncConfig,
    pub _shutdown: Mutex<Option<oneshot::Sender<()>>>,
    /// Shutdown senders for spawned uploader tasks; kept alive for the process
    /// lifetime so the uploaders run (dropping a sender would stop them).
    pub _sync_shutdowns: Mutex<Vec<oneshot::Sender<()>>>,
    /// Serializes journal opens so parallel user triggers do not race the single
    /// Tauri window label and surface a spurious duplicate-label failure.
    pub journal_open_lock: tokio::sync::Mutex<()>,
    /// Per-window loopback bridge backing the external journal window.
    pub journal_bridge: Mutex<Option<pl_transport_win::journal_bridge::JournalBridgeHandle>>,
    /// Capture-exclusion rules controller (shared with the WGC screen source).
    pub exclusions: crate::exclusions::ExclusionController,
}

/// The observer's hostname for registration, best-effort.
fn observer_hostname() -> String {
    std::env::var("COMPUTERNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "windows-observer".to_string())
}

/// Build the sync config from the per-user data layout + the engine's rotation
/// period (so the uploader derives the same segment keys the writer sealed).
fn build_sync_config(retention: Arc<RwLock<RetentionConfig>>) -> SyncConfig {
    let host = observer_hostname();
    SyncConfig {
        platform: "windows".to_string(),
        hostname: host.clone(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        stream_type: "desktop".to_string(),
        device_label: host,
        period_secs: EngineConfig::default().segment_secs,
        state_path: platform_win::local_data_root().join("pairing.json"),
        segments_root: platform_win::segments_dir(),
        retention,
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

/// Boot the tray-resident observer.
pub fn run(
    open_view: Option<observer_model::View>,
    surface_on_launch: bool,
    open_journal_on_launch: bool,
) {
    tracing::info!(
        target: "lifecycle",
        version = env!("CARGO_PKG_VERSION"),
        build = if cfg!(debug_assertions) { "debug" } else { "release" },
        "boot"
    );

    let app = tauri::Builder::default()
        // Backend-only: opens the owner's default browser for `open_release_notes`.
        // No opener:* permission is added to the webview capability set, so the
        // renderer cannot call the plugin directly or name a URL — its sole
        // outbound reach is the fixed-URL command below.
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            crate::ipc::start_observing,
            crate::ipc::pause,
            crate::ipc::resume,
            crate::ipc::get_health,
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
            crate::ipc::get_retention,
            crate::ipc::set_retention,
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
        ])
        .setup(move |app| {
            match crate::lifecycle::acquire_single_instance() {
                platform_win::InstanceLock::AlreadyRunning => {
                    tracing::info!(
                        target: "lifecycle",
                        outcome = "already_running",
                        "single instance"
                    );
                    if open_journal_on_launch {
                        let _ = crate::control::signal_open_journal();
                    } else if surface_on_launch {
                        let _ = crate::control::signal_surface();
                    }
                    app.handle().exit(0);
                    return Ok(());
                }
                platform_win::InstanceLock::Acquired => {
                    tracing::info!(
                        target: "lifecycle",
                        outcome = "acquired",
                        "single instance"
                    );
                }
            }

            // Ensure the per-user autostart login item so the tray-resident
            // observer relaunches at the next login. Run on every launch (not
            // gated on a one-shot install signal) and idempotent: it writes only
            // when the entry is missing or stale, so it self-heals an unregistered
            // install and re-points the entry if the executable path moved. A
            // failure is logged, never fatal.
            match std::env::current_exe() {
                Ok(exe) => match platform_win::autostart::ensure_login_item(
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
                    }
                    Ok(platform_win::autostart::EnsureOutcome::AlreadyCurrent) => {
                        tracing::info!(
                            target: "lifecycle",
                            component = "autostart",
                            outcome = "already_current",
                            "autostart ensure"
                        );
                    }
                    Err(error) => tracing::warn!(
                        target: "lifecycle",
                        component = "autostart",
                        outcome = "failed",
                        error = %error,
                        "autostart ensure"
                    ),
                },
                Err(error) => tracing::warn!(
                    target: "lifecycle",
                    component = "autostart",
                    outcome = "current_exe_failed",
                    error = %error,
                    "autostart ensure"
                ),
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

            // Cache retention: load the persisted policy and share the handle with
            // the upload coordinator (via SyncConfig) so it deletes or retains
            // confirmed segments per the owner's window.
            let retention = crate::retention::RetentionController::new(
                platform_win::local_data_root().join("retention.json"),
            );

            let sources = Sources {
                screen: Box::new(capture_wgc::WgcScreenSource::new(exclusions.rules_handle())),
                screen_encoder: Box::new(capture_screen_encode::MfScreenEncoder::new()),
                system_audio: Box::new(capture_wasapi::WasapiSystemAudioSource::new()),
                mic: Box::new(capture_wasapi::WasapiMicSource::new(
                    mic.config_handle(),
                    mic.active_handle(),
                )),
            };
            let mut recovery = platform_win::LocalRecoveryFs::default();
            let segment_fs = platform_win::LocalSegmentFs::default();
            tracing::info!(target: "engine", operation = "start", "engine start");
            let (mut engine, _outcomes) = CaptureEngine::new(
                sources,
                EngineConfig::default(),
                &mut recovery,
                segment_fs,
                Box::new(SystemClock),
            )?;
            engine
                .start()
                .map_err(|error| io::Error::other(format!("engine start failed: {error:?}")))?;
            tracing::info!(target: "engine", outcome = "started", "engine start");

            let health = engine.health_handle();
            let sync = engine.sync_handle();
            let watch_rx = engine.health_watch();
            let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
            let (shutdown_tx, shutdown_rx) = oneshot::channel();

            // Resume an existing pairing: if a credential is on disk, start the
            // upload + heartbeat loop now. A fresh pairing is started by the
            // `pair` IPC command instead.
            let sync_config = build_sync_config(retention.config_handle());
            let mut sync_shutdowns: Vec<oneshot::Sender<()>> = Vec::new();
            match PairedState::load(&sync_config.state_path) {
                Ok(paired) if paired.is_paired() => {
                    let (up_tx, up_rx) = oneshot::channel();
                    sync_shutdowns.push(up_tx);
                    let cfg = sync_config.clone();
                    let health_for_sync = health.clone();
                    let sync_for_sync = sync.clone();
                    tracing::info!(
                        target: "sync",
                        source = "resume",
                        "uploader started"
                    );
                    tauri::async_runtime::spawn(async move {
                        if let Err(error) =
                            run_uploader(paired, cfg, health_for_sync, sync_for_sync, up_rx).await
                        {
                            let error = error.to_string();
                            tracing::warn!(
                                target: "sync",
                                source = "resume",
                                error = %observer_log::redact_secret("uploader-error", &error),
                                "uploader exited"
                            );
                        }
                    });
                }
                Ok(_) => {}
                Err(error) => {
                    let error = error.to_string();
                    tracing::warn!(
                        target: "sync",
                        error = %observer_log::redact_secret("pairing-load-error", &error),
                        "pairing state load failed"
                    );
                }
            }

            app.manage(AppState {
                commands: cmd_tx.clone(),
                health: health.clone(),
                sync: sync.clone(),
                sync_config,
                _shutdown: Mutex::new(Some(shutdown_tx)),
                _sync_shutdowns: Mutex::new(sync_shutdowns),
                journal_open_lock: tokio::sync::Mutex::new(()),
                journal_bridge: Mutex::new(None),
                exclusions,
            });

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
            app.manage(retention.clone());

            let (tray, mi_start, pause_submenu, mi_resume) =
                crate::tray::init(app, cmd_tx.clone())?;

            tauri::async_runtime::spawn(async move {
                let _ = engine.run(shutdown_rx, cmd_rx).await;
            });

            tauri::async_runtime::spawn({
                let health = health.clone();
                async move {
                    match tokio::net::TcpListener::bind(("127.0.0.1", crate::health::HEALTH_PORT))
                        .await
                    {
                        Ok(listener) => {
                            let _ = capture_engine::serve_health(listener, health).await;
                        }
                        Err(error) => {
                            tracing::error!(
                                target: "health",
                                port = crate::health::HEALTH_PORT,
                                error = %error,
                                "health server bind failed"
                            );
                        }
                    }
                }
            });

            tauri::async_runtime::spawn({
                let app = app.handle().clone();
                async move {
                    match tokio::net::TcpListener::bind((
                        "127.0.0.1",
                        crate::control::CONTROL_PORT,
                    ))
                    .await
                    {
                        Ok(listener) => {
                            crate::control::serve(app, listener).await;
                        }
                        Err(error) => {
                            tracing::error!(
                                target: "control",
                                port = crate::control::CONTROL_PORT,
                                error = %error,
                                "control server bind failed"
                            );
                        }
                    }
                }
            });

            tauri::async_runtime::spawn({
                let app = app.handle().clone();
                let health = health.clone();
                let tray = tray.clone();
                let mi_start = mi_start.clone();
                let pause_submenu = pause_submenu.clone();
                let mi_resume = mi_resume.clone();
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
                            &mi_start,
                            &pause_submenu,
                            &mi_resume,
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
                        pause: None,
                        views,
                    };
                    log_health_transitions(previous.as_ref(), &terminal);
                    if let Ok(mut health) = health.lock() {
                        *health = terminal.clone();
                    }
                    crate::tray::apply_state(
                        &tray,
                        &mi_start,
                        &pause_submenu,
                        &mi_resume,
                        &terminal,
                    );
                    let _ = app.emit("health://changed", &terminal);
                }
            });

            std::thread::spawn({
                let cmd_tx = cmd_tx.clone();
                let hotkey_desired = hotkey.desired_handle();
                let hotkey_outcome = hotkey.outcome_handle();
                move || {
                    let mut pump =
                        platform_win::NotificationPump::with_hotkey(hotkey_desired, hotkey_outcome);
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
                                platform_win::SystemNotification::SessionUnlocked
                                | platform_win::SystemNotification::Resumed => {
                                    Some(EngineCommand::Resume)
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
                    if let Err(error) = crate::windows::open_journal(&handle).await {
                        tracing::warn!(
                            target: "window",
                            label = "journal",
                            error = error.token(),
                            "open-journal failed"
                        );
                    }
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
