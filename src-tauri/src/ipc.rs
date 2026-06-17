// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The shell -> backend command surface.
//!
//! These are user **intents**. They ask the engine to do something; they never
//! set UI state. The webview cannot mint status — it has no input but the
//! `health://changed` event it subscribes to — so "status earned, never
//! asserted" holds by construction. Commands mirror the boundary contract:
//! `start_observing / pause / resume / get_health / open_settings / open_about`.

use observer_model::HealthDump;

/// Ask the engine to begin observing. The resulting phase is *computed* by the
/// reducer once sources go active — this command does not return "observing".
#[tauri::command]
pub fn start_observing() -> Result<(), String> {
    // TODO(shell): forward AppEvent::RequestedStart to the engine task.
    Ok(())
}

/// Ask the engine to pause. `reason` is an owner-meaningful token.
#[tauri::command]
pub fn pause(reason: String) -> Result<(), String> {
    let _ = reason;
    // TODO(shell): forward AppEvent::RequestedPause(..).
    Ok(())
}

/// Ask the engine to resume.
#[tauri::command]
pub fn resume() -> Result<(), String> {
    // TODO(shell): forward AppEvent::RequestedResume.
    Ok(())
}

/// Return the current honest health snapshot (same payload as `--dump-state`
/// and `/healthz`).
#[tauri::command]
pub fn get_health() -> Result<HealthDump, String> {
    crate::health::current_dump().map_err(|e| e.to_string())
}

/// Open (create on demand) the Settings window.
#[tauri::command]
pub fn open_settings(app: tauri::AppHandle) -> Result<(), String> {
    crate::windows::open_settings(&app).map_err(|e| e.to_string())
}

/// Open (create on demand) the About window.
#[tauri::command]
pub fn open_about(app: tauri::AppHandle) -> Result<(), String> {
    crate::windows::open_about(&app).map_err(|e| e.to_string())
}
