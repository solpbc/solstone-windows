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

use tauri::Manager;

/// Boot the tray-resident observer. Skeleton: registers the IPC handlers and the
/// autostart plugin; the engine wiring and tray menu are filled in by the
/// Wave-1 shell work.
pub fn run() {
    tauri::Builder::default()
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
            // TODO(shell): build the per-state tray icon + menu, construct
            // the capture engine with injected WGC/WASAPI sources, and start the
            // health event pump that emits `health://changed`.
            let _ = app.handle();
            crate::tray::init(app)?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the observer");
}
