// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! App composition root.
//!
//! Wires the tray, the IPC command surface, and the Velopack-aware autostart
//! plugin, then runs the tray-resident event loop. No window is auto-shown;
//! Settings/About are created on demand (see [`crate::windows`]). The capture
//! engine is constructed here with the concrete platform sources injected at the
//! `observer-model` trait seam (`capture-wgc` / `capture-wasapi`), keeping the
//! engine itself Windows-agnostic.

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use capture_engine::{CaptureEngine, EngineCommand, EngineConfig, Sources, SystemClock};
use observer_model::{AppPhase, HealthDump, PauseReason};
use tauri::{Emitter, Manager};
use tokio::sync::{mpsc, oneshot};

pub struct AppState {
    pub commands: mpsc::UnboundedSender<EngineCommand>,
    pub health: Arc<Mutex<HealthDump>>,
    pub _shutdown: Mutex<Option<oneshot::Sender<()>>>,
}

/// Boot the tray-resident observer.
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .invoke_handler(tauri::generate_handler![
            crate::ipc::start_observing,
            crate::ipc::pause,
            crate::ipc::resume,
            crate::ipc::get_health,
            crate::ipc::open_settings,
            crate::ipc::open_about,
        ])
        .setup(|app| {
            if crate::lifecycle::acquire_single_instance()
                == platform_win::InstanceLock::AlreadyRunning
            {
                app.handle().exit(0);
                return Ok(());
            }

            let sources = Sources {
                screen: Box::new(capture_wgc::WgcScreenSource::new()),
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
            let watch_rx = engine.health_watch();
            let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
            let (shutdown_tx, shutdown_rx) = oneshot::channel();

            app.manage(AppState {
                commands: cmd_tx.clone(),
                health: health.clone(),
                _shutdown: Mutex::new(Some(shutdown_tx)),
            });

            let (tray, mi_start, mi_pause, mi_resume) = crate::tray::init(app, cmd_tx.clone())?;

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
                let mi_pause = mi_pause.clone();
                let mi_resume = mi_resume.clone();
                let mut rx = watch_rx;
                async move {
                    loop {
                        let dump = rx.borrow_and_update().clone();
                        crate::tray::apply_state(
                            &tray,
                            &mi_start,
                            &mi_pause,
                            &mi_resume,
                            &dump.app_state,
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
                    let terminal = HealthDump {
                        app_state: AppPhase::Error,
                        sources,
                        frame_rate: None,
                        segment_dir: None,
                        segment_seconds_remaining: None,
                        engine_ready: false,
                        version: env!("CARGO_PKG_VERSION").to_string(),
                    };
                    if let Ok(mut health) = health.lock() {
                        *health = terminal.clone();
                    }
                    crate::tray::apply_state(
                        &tray,
                        &mi_start,
                        &mi_pause,
                        &mi_resume,
                        &AppPhase::Error,
                    );
                    let _ = app.emit("health://changed", &terminal);
                }
            });

            std::thread::spawn({
                let cmd_tx = cmd_tx.clone();
                move || {
                    let mut pump = platform_win::NotificationPump::new();
                    loop {
                        for notification in pump.poll() {
                            let command = match notification {
                                platform_win::SystemNotification::SessionLocked => {
                                    Some(EngineCommand::Pause(PauseReason::SessionLocked))
                                }
                                platform_win::SystemNotification::Suspending => {
                                    Some(EngineCommand::Pause(PauseReason::SystemSuspending))
                                }
                                platform_win::SystemNotification::SessionUnlocked
                                | platform_win::SystemNotification::Resumed => {
                                    Some(EngineCommand::Resume)
                                }
                                platform_win::SystemNotification::DisplayChanged => {
                                    Some(EngineCommand::DisplayChanged)
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
        if let tauri::RunEvent::ExitRequested { code, api } = event {
            if code.is_none() {
                api.prevent_exit();
            }
        }
    });
}
