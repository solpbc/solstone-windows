// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! On-demand windows: Settings and About.
//!
//! Windows are created when requested and destroyed on close; the process stays
//! tray-resident. None is auto-shown at launch. Settings panes: Status + Sources
//! (Wave 1); Pairing (Wave 2). The window roots carry AutomationIds from the
//! contract SoT (`observer_contract::settings::WINDOW_ROOT`,
//! `observer_contract::about::WINDOW_ROOT`).

/// Open (or focus) the Settings window. Skeleton — Wave-1 shell work builds the
/// WebviewWindow pointing at the Vite-built `ui/dist` Settings route.
pub fn open_settings(_app: &tauri::AppHandle) -> tauri::Result<()> {
    // TODO(shell): WebviewWindowBuilder for the Settings route; stamp
    // observer_contract::settings::WINDOW_ROOT.
    Ok(())
}

/// Open (or focus) the About window.
pub fn open_about(_app: &tauri::AppHandle) -> tauri::Result<()> {
    // TODO(shell): WebviewWindowBuilder for the About route; stamp
    // observer_contract::about::WINDOW_ROOT.
    Ok(())
}
