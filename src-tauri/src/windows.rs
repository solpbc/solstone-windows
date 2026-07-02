// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! On-demand windows: Settings, About, and the paired journal.
//!
//! Windows are created when requested and destroyed on close; the process stays
//! tray-resident. None is auto-shown at launch. Settings panes: Status + Sources
//! (Wave 1); Pairing (Wave 2). Our bundled window roots carry AutomationIds
//! from the contract SoT (`observer_contract::settings::WINDOW_ROOT`,
//! `observer_contract::about::WINDOW_ROOT`); the journal is external content.

use std::sync::Arc;
use std::time::Duration;

use tauri::webview::{PageLoadEvent, ScrollBarStyle};
use tauri::window::{Effect, EffectsBuilder};
use tauri::{Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tokio::sync::{oneshot, Notify};

const WEBVIEW_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,OverscrollHistoryNavigation,msExperimentalScrolling --disable-pinch";
// Generous by design: first WebView2 startup plus the loopback bridge and PL/TLS
// dial can be slow. A delayed real success is preferable to a spurious failure.
const JOURNAL_READY_TIMEOUT: Duration = Duration::from_secs(45);

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
    let state = app.state::<crate::app::AppState>();
    let _open_guard = state.journal_open_lock.lock().await;

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

    let state_path = state.sync_config.state_path.clone();
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
    tracing::info!(
        target: "window",
        label = "journal",
        bridge_port = handle.port(),
        "journal bridge started"
    );
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

    let page_loaded = Arc::new(Notify::new());
    let window =
        match build_journal_window_on_main_thread(app, url.clone(), page_loaded.clone()).await {
            Ok(window) => window,
            Err(_) => {
                shutdown_journal_bridge(&state);
                log_journal_open_failed();
                return Err(OpenJournalError::OpenFailed);
            }
        };
    log_journal_window_state(&window, "built");
    let navigated = tokio::time::timeout(JOURNAL_READY_TIMEOUT, page_loaded.notified())
        .await
        .is_ok();
    let usable = journal_window_is_usable(&window);
    if !navigated || !usable {
        tracing::warn!(
            target: "window",
            label = "journal",
            navigated,
            usable,
            "journal readiness failed"
        );
        log_journal_window_state(&window, "readiness_failed");
        window.close().ok();
        shutdown_journal_bridge(&state);
        log_journal_open_failed();
        return Err(OpenJournalError::OpenFailed);
    }

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

fn shutdown_journal_bridge(state: &crate::app::AppState) {
    if let Ok(mut guard) = state.journal_bridge.lock() {
        if let Some(handle) = guard.take() {
            handle.begin_shutdown();
        }
    }
}

fn log_journal_open_failed() {
    tracing::warn!(
        target: "window",
        label = "journal",
        outcome = "open_failed",
        "window open"
    );
}

fn journal_window_is_usable(window: &WebviewWindow) -> bool {
    let Ok(true) = window.is_visible() else {
        return false;
    };
    let Ok(false) = window.is_minimized() else {
        return false;
    };
    let Ok(inner) = window.inner_size() else {
        return false;
    };
    let Ok(outer) = window.outer_size() else {
        return false;
    };

    inner.width > 0 && inner.height > 0 && outer.width > 0 && outer.height > 0
}

fn log_journal_window_state(window: &WebviewWindow, stage: &'static str) {
    let inner = window.inner_size().ok();
    let outer = window.outer_size().ok();
    let (inner_width, inner_height) = inner
        .map(|size| (Some(size.width), Some(size.height)))
        .unwrap_or((None, None));
    let (outer_width, outer_height) = outer
        .map(|size| (Some(size.width), Some(size.height)))
        .unwrap_or((None, None));
    tracing::info!(
        target: "window",
        label = "journal",
        stage,
        visible = ?window.is_visible().ok(),
        minimized = ?window.is_minimized().ok(),
        inner_width,
        inner_height,
        outer_width,
        outer_height,
        "journal window state"
    );
}

// INVARIANT: `open_journal` must always run off the Tauri main thread. The tray
// path spawns it onto `tauri::async_runtime`, and the IPC path is an async
// command. Calling it from setup/main thread would deadlock while waiting for
// this main-thread closure; the `--open-journal` single-instance control verb
// also dispatches onto the async runtime before it calls this function.
async fn build_journal_window_on_main_thread(
    app: &tauri::AppHandle,
    url: String,
    page_loaded: Arc<Notify>,
) -> tauri::Result<WebviewWindow> {
    let (tx, rx) = oneshot::channel();
    let app_for_main = app.clone();
    app.run_on_main_thread(move || {
        let res = build_journal_window(&app_for_main, &url, page_loaded);
        let _ = tx.send(res);
    })?;

    rx.await.map_err(|_| tauri::Error::FailedToReceiveMessage)?
}

fn build_journal_window(
    app: &tauri::AppHandle,
    url: &str,
    page_loaded: Arc<Notify>,
) -> tauri::Result<WebviewWindow> {
    let parsed: tauri::Url = url.parse().map_err(tauri::Error::InvalidUrl)?;
    let expected_port = parsed.port();
    let window = WebviewWindowBuilder::new(app, "journal", WebviewUrl::External(parsed))
        .title("solstone — journal")
        .inner_size(1100.0, 800.0)
        .min_inner_size(640.0, 480.0)
        .additional_browser_args(WEBVIEW_ARGS)
        .visible(false)
        .on_navigation(move |url| {
            let allowed = url.scheme() == "http"
                && url.host_str() == Some("127.0.0.1")
                && url.port() == expected_port;
            if allowed {
                tracing::info!(
                    target: "window",
                    label = "journal",
                    scheme = url.scheme(),
                    host = url.host_str().unwrap_or(""),
                    port = url.port().unwrap_or(0),
                    "journal navigation"
                );
            } else {
                tracing::warn!(
                    target: "window",
                    label = "journal",
                    scheme = url.scheme(),
                    host = url.host_str().unwrap_or(""),
                    port = url.port().unwrap_or(0),
                    "blocked journal navigation"
                );
            }
            allowed
        })
        .on_page_load(move |_window, payload| {
            tracing::info!(
                target: "window",
                label = "journal",
                event = match payload.event() {
                    PageLoadEvent::Started => "started",
                    PageLoadEvent::Finished => "finished",
                },
                "journal page load"
            );
            if payload.event() == PageLoadEvent::Finished {
                page_loaded.notify_one();
            }
        })
        .build()?;

    window.set_size(tauri::Size::Logical(tauri::LogicalSize::new(1100.0, 800.0)))?;
    window.center().ok();
    window.show()?;
    window.set_focus().ok();
    Ok(window)
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
