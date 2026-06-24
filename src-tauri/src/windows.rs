// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! On-demand windows: Settings and About.
//!
//! Windows are created when requested and destroyed on close; the process stays
//! tray-resident. None is auto-shown at launch. Settings panes: Status + Sources
//! (Wave 1); Pairing (Wave 2). The window roots carry AutomationIds from the
//! contract SoT (`observer_contract::settings::WINDOW_ROOT`,
//! `observer_contract::about::WINDOW_ROOT`).

use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

/// Open (or focus) the Settings window.
pub fn open_settings(app: &tauri::AppHandle) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window("settings") {
        window.set_focus()?;
        tracing::info!(
            target: "window",
            label = "settings",
            action = "focus_existing",
            "window open"
        );
        return Ok(());
    }

    WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("index.html".into()))
        .title("solstone — settings")
        .inner_size(420.0, 520.0)
        .visible(true)
        .build()?;
    tracing::info!(
        target: "window",
        label = "settings",
        action = "create",
        "window open"
    );
    Ok(())
}

/// Open (or focus) the About window.
pub fn open_about(app: &tauri::AppHandle) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window("about") {
        window.set_focus()?;
        tracing::info!(
            target: "window",
            label = "about",
            action = "focus_existing",
            "window open"
        );
        return Ok(());
    }

    WebviewWindowBuilder::new(app, "about", WebviewUrl::App("index.html".into()))
        .title("about solstone")
        .inner_size(360.0, 280.0)
        .visible(true)
        .build()?;
    tracing::info!(
        target: "window",
        label = "about",
        action = "create",
        "window open"
    );
    Ok(())
}
