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

/// Ask the engine to pause. `reason` is an owner-meaningful token.
#[tauri::command]
pub fn pause(state: tauri::State<'_, crate::app::AppState>, reason: String) -> Result<(), String> {
    let reason = match reason.as_str() {
        "session_locked" => PauseReason::SessionLocked,
        "system_suspending" => PauseReason::SystemSuspending,
        _ => PauseReason::Operator,
    };
    state
        .commands
        .send(EngineCommand::Pause(reason))
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
