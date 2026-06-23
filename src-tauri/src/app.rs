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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use capture_engine::{CaptureEngine, EngineCommand, EngineConfig, Sources, SystemClock};
use observer_model::{AppPhase, HealthDump, PauseReason, SyncSnapshot};
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
fn build_sync_config() -> SyncConfig {
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
    }
}

/// Boot the tray-resident observer.
pub fn run() {
    let app = tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            crate::ipc::start_observing,
            crate::ipc::pause,
            crate::ipc::resume,
            crate::ipc::get_health,
            crate::ipc::open_settings,
            crate::ipc::open_about,
            crate::ipc::pair,
            crate::ipc::get_exclusions,
            crate::ipc::set_exclusions,
            crate::ipc::list_running_apps,
            crate::ipc::get_hotkey,
            crate::ipc::set_hotkey,
            crate::ipc::update_get,
            crate::ipc::update_check_now,
            crate::ipc::update_download,
            crate::ipc::update_install,
            crate::ipc::update_dismiss,
            crate::ipc::update_set_auto_check,
            crate::ipc::update_set_auto_download,
            crate::ipc::update_set_interval,
        ])
        .setup(|app| {
            if crate::lifecycle::acquire_single_instance()
                == platform_win::InstanceLock::AlreadyRunning
            {
                app.handle().exit(0);
                return Ok(());
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
                    &[],
                ) {
                    Ok(platform_win::autostart::EnsureOutcome::Registered) => {
                        eprintln!("autostart: registered login item");
                    }
                    Ok(platform_win::autostart::EnsureOutcome::AlreadyCurrent) => {}
                    Err(error) => eprintln!("autostart: registration failed: {error}"),
                },
                Err(error) => eprintln!("autostart: could not resolve current exe: {error}"),
            }

            // Capture-exclusion rules: load persisted owner policy and share the
            // handle with the WGC source so edits take effect on the next frame.
            let exclusions = crate::exclusions::ExclusionController::new(
                platform_win::local_data_root().join("exclusions.json"),
            );

            let sources = Sources {
                screen: Box::new(capture_wgc::WgcScreenSource::new(exclusions.rules_handle())),
                screen_encoder: Box::new(capture_screen_encode::MfScreenEncoder::new()),
                system_audio: Box::new(capture_wasapi::WasapiSystemAudioSource::new()),
                mic: Box::new(capture_wasapi::WasapiMicSource::new()),
            };
            let mut recovery = platform_win::LocalRecoveryFs::default();
            let segment_fs = platform_win::LocalSegmentFs::default();
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

            let health = engine.health_handle();
            let sync = engine.sync_handle();
            let watch_rx = engine.health_watch();
            let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
            let (shutdown_tx, shutdown_rx) = oneshot::channel();

            // Resume an existing pairing: if a credential is on disk, start the
            // upload + heartbeat loop now. A fresh pairing is started by the
            // `pair` IPC command instead.
            let sync_config = build_sync_config();
            let mut sync_shutdowns: Vec<oneshot::Sender<()>> = Vec::new();
            match PairedState::load(&sync_config.state_path) {
                Ok(paired) if paired.is_paired() => {
                    let (up_tx, up_rx) = oneshot::channel();
                    sync_shutdowns.push(up_tx);
                    let cfg = sync_config.clone();
                    let health_for_sync = health.clone();
                    let sync_for_sync = sync.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(error) =
                            run_uploader(paired, cfg, health_for_sync, sync_for_sync, up_rx).await
                        {
                            eprintln!("uploader exited: {error}");
                        }
                    });
                }
                Ok(_) => {}
                Err(error) => eprintln!("failed to load pairing state: {error}"),
            }

            app.manage(AppState {
                commands: cmd_tx.clone(),
                health: health.clone(),
                sync: sync.clone(),
                sync_config,
                _shutdown: Mutex::new(Some(shutdown_tx)),
                _sync_shutdowns: Mutex::new(sync_shutdowns),
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
                            eprintln!("health server bind failed: {error}");
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
                    loop {
                        let dump = rx.borrow_and_update().clone();
                        crate::tray::apply_state(
                            &tray,
                            &mi_start,
                            &pause_submenu,
                            &mi_resume,
                            &dump,
                        );
                        let _ = app.emit("health://changed", &dump);
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
                    };
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
