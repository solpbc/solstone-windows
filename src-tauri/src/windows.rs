// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! On-demand windows: Settings, About, and the paired journal.
//!
//! Windows are created when requested and destroyed on close; the process stays
//! tray-resident. None is auto-shown at launch. Settings panes: Status + Sources
//! (Wave 1); Pairing (Wave 2). Our bundled window roots carry AutomationIds
//! from the contract SoT (`observer_contract::settings::WINDOW_ROOT`,
//! `observer_contract::about::WINDOW_ROOT`); the journal is external content.

use tauri::webview::ScrollBarStyle;
use tauri::window::{Effect, EffectsBuilder};
use tauri::{Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

const WEBVIEW_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,OverscrollHistoryNavigation,msExperimentalScrolling --disable-pinch";

fn mica_effects() -> tauri::utils::config::WindowEffectsConfig {
    EffectsBuilder::new().effect(Effect::Mica).build()
}

pub enum OpenJournalError {
    Unpaired,
    OpenFailed,
}

impl OpenJournalError {
    pub fn token(&self) -> &'static str {
        match self {
            Self::Unpaired => "unpaired",
            Self::OpenFailed => "open_failed",
        }
    }
}

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
        .inner_size(820.0, 580.0)
        .min_inner_size(460.0, 480.0)
        .transparent(true)
        .effects(mica_effects())
        .scroll_bar_style(ScrollBarStyle::FluentOverlay)
        .additional_browser_args(WEBVIEW_ARGS)
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

/// Open (or focus) the paired journal window.
pub async fn open_journal(app: &tauri::AppHandle) -> Result<(), OpenJournalError> {
    if let Some(window) = app.get_webview_window("journal") {
        window.set_focus().ok();
        tracing::info!(
            target: "window",
            label = "journal",
            action = "focus_existing",
            "window open"
        );
        return Ok(());
    }

    let state_path = app
        .state::<crate::app::AppState>()
        .sync_config
        .state_path
        .clone();
    let paired = pl_transport_win::credential::PairedState::load(&state_path)
        .map_err(|_| OpenJournalError::Unpaired)?;

    let handle = match pl_transport_win::journal_bridge::start(&paired, state_path).await {
        Ok(handle) => handle,
        Err(pl_transport_win::journal_bridge::BridgeStartError::NotReady) => {
            return Err(OpenJournalError::Unpaired);
        }
        Err(
            pl_transport_win::journal_bridge::BridgeStartError::Bind(_)
            | pl_transport_win::journal_bridge::BridgeStartError::Client(_),
        ) => {
            tracing::warn!(
                target: "window",
                label = "journal",
                outcome = "open_failed",
                "window open"
            );
            return Err(OpenJournalError::OpenFailed);
        }
    };

    let url = handle.bootstrap_url();
    let state = app.state::<crate::app::AppState>();
    match state.journal_bridge.lock() {
        Ok(mut guard) => {
            if let Some(old) = guard.take() {
                old.begin_shutdown();
            }
            *guard = Some(handle);
        }
        Err(_) => {
            handle.begin_shutdown();
            tracing::warn!(
                target: "window",
                label = "journal",
                outcome = "open_failed",
                "window open"
            );
            return Err(OpenJournalError::OpenFailed);
        }
    }

    let window = match build_journal_window(app, &url) {
        Ok(window) => window,
        Err(_) => {
            if let Ok(mut guard) = state.journal_bridge.lock() {
                if let Some(handle) = guard.take() {
                    handle.begin_shutdown();
                }
            }
            tracing::warn!(
                target: "window",
                label = "journal",
                outcome = "open_failed",
                "window open"
            );
            return Err(OpenJournalError::OpenFailed);
        }
    };

    let teardown_app = app.clone();
    window.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::Destroyed) {
            if let Some(state) = teardown_app.try_state::<crate::app::AppState>() {
                if let Ok(mut guard) = state.journal_bridge.lock() {
                    if let Some(handle) = guard.take() {
                        handle.begin_shutdown();
                    }
                }
            }
        }
    });

    tracing::info!(
        target: "window",
        label = "journal",
        action = "create",
        "window open"
    );
    Ok(())
}

fn build_journal_window(app: &tauri::AppHandle, url: &str) -> tauri::Result<WebviewWindow> {
    let parsed: tauri::Url = url.parse().map_err(tauri::Error::InvalidUrl)?;
    WebviewWindowBuilder::new(app, "journal", WebviewUrl::External(parsed))
        .title("solstone — journal")
        .inner_size(1100.0, 800.0)
        .min_inner_size(640.0, 480.0)
        .additional_browser_args(WEBVIEW_ARGS)
        .visible(true)
        .build()
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
        .transparent(true)
        .effects(mica_effects())
        .scroll_bar_style(ScrollBarStyle::FluentOverlay)
        .additional_browser_args(WEBVIEW_ARGS)
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
