// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Tray icon and menu.
//!
//! The tray icon is a **pure function of `app_state`** (idle / observing /
//! paused / error) — it is driven by the honest [`HealthDump`] the engine emits,
//! never set optimistically. Menu items: Start / Pause / Resume · Open Settings ·
//! About · Quit. Each carries an AutomationId from the contract SoT
//! (`observer_contract::tray::*`) so the FlaUI harness can find it on the native
//! UIA surface.

use tauri::App;

/// Install the tray icon + menu. Skeleton — the Wave-1 shell work builds the
/// per-state icons and wires the menu actions to the IPC commands.
pub fn init(_app: &mut App) -> tauri::Result<()> {
    // TODO(shell): TrayIconBuilder with per-state icons keyed off app_state;
    // menu items stamped with observer_contract::tray::MENU_* AutomationIds.
    Ok(())
}
