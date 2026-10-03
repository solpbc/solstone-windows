// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! On-demand windows: Settings, About, and the paired journal.
//!
//! Windows are created when requested and destroyed on close; the process stays
//! tray-resident. None is auto-shown at launch. Settings panes: Status + Sources
//! (Wave 1); Pairing (Wave 2). Our bundled window roots carry AutomationIds
//! from the contract SoT (`observer_contract::settings::WINDOW_ROOT`,
//! `observer_contract::about::WINDOW_ROOT`); the journal is external content.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use std::time::Instant;

use observer_log::{
    classify_journal_open_failure, strip_cap, usable_failure_reason, UsableFailureReason,
};
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
        .title("solstone settings")
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

/// Dispatch opening journal based on pairing phase.
pub async fn dispatch_open_journal<FSettings, FBridge, FutSettings, FutBridge>(
    phase: observer_model::PairingPhase,
    on_settings: FSettings,
    on_bridge: FBridge,
) -> Result<(), OpenJournalError>
where
    FSettings: FnOnce() -> FutSettings,
    FutSettings: std::future::Future<Output = Result<(), OpenJournalError>>,
    FBridge: FnOnce() -> FutBridge,
    FutBridge: std::future::Future<Output = Result<(), OpenJournalError>>,
{
    match observer_model::journal_open_choice(phase) {
        observer_model::JournalOpenChoice::Settings => on_settings().await,
        observer_model::JournalOpenChoice::Bridge => on_bridge().await,
    }
}

pub trait JournalSurface: Send + Sync {
    fn open_settings(&self) -> tauri::Result<()>;
    fn focus_journal_if_present(&self) -> bool;
    fn close_journal(&self);
}

impl JournalSurface for tauri::AppHandle {
    fn open_settings(&self) -> tauri::Result<()> {
        open_settings(self)
    }

    fn focus_journal_if_present(&self) -> bool {
        if let Some(window) = self.get_webview_window("journal") {
            window.set_focus().ok();
            true
        } else {
            false
        }
    }

    fn close_journal(&self) {
        if let Some(window) = self.get_webview_window("journal") {
            window.close().ok();
        }
    }
}

#[cfg(test)]
pub struct OpenPairProbe {
    pub open_holds: tokio::sync::Notify,
    pub release_open: tokio::sync::Notify,
    pub pair_at_lock: tokio::sync::Notify,
    pub release_pair: tokio::sync::Notify,
    pub pair_entered_lock: std::sync::atomic::AtomicBool,
}

#[cfg(test)]
impl OpenPairProbe {
    pub fn new() -> Self {
        Self {
            open_holds: tokio::sync::Notify::new(),
            release_open: tokio::sync::Notify::new(),
            pair_at_lock: tokio::sync::Notify::new(),
            release_pair: tokio::sync::Notify::new(),
            pair_entered_lock: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

/// Open (or focus) the paired journal window.
pub async fn open_journal<S: JournalSurface>(
    state: &crate::app::AppState,
    surface: &S,
    app: Option<&tauri::AppHandle>,
) -> Result<(), OpenJournalError> {
    let phase = state
        .sync
        .lock()
        .map(|s| s.pairing.phase)
        .unwrap_or(observer_model::PairingPhase::NotPaired);

    dispatch_open_journal(
        phase,
        || async {
            let _ = surface.open_settings();
            Ok(())
        },
        || async {
            let _open_guard = state.journal_open_lock.lock().await;

            if surface.focus_journal_if_present() {
                tracing::info!(
                    target: "window",
                    label = "journal",
                    action = "focus_existing",
                    "window open"
                );
                return Ok(());
            }

            #[cfg(test)]
            if phase == observer_model::PairingPhase::Paired {
                if let Some(probe) = &state.probe {
                    probe.open_holds.notify_one();
                    probe.release_open.notified().await;
                }
            }

            let access = state
                .credential_access
                .lock()
                .await
                .clone()
                .ok_or(OpenJournalError::Unpaired)?;

            let handle = match pl_transport_win::journal_bridge::start_with_facts(access).await {
                Ok(handle) => handle,
                Err(pl_transport_win::journal_bridge::BridgeStartError::AwaitingConfirmation) => {
                    let _ = surface.open_settings();
                    return Ok(());
                }
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

            let app = match app {
                Some(app) => app,
                None => return Ok(()),
            };

            let page_loaded = Arc::new(Notify::new());
            let page_load_started = Arc::new(AtomicBool::new(false));
            let window = match build_journal_window_on_main_thread(
                app,
                url.clone(),
                page_loaded.clone(),
                page_load_started.clone(),
            )
            .await
            {
                Ok(window) => window,
                Err(error) => {
                    tracing::warn!(
                        target: "window",
                        label = "journal",
                        error = %error,
                        "journal window construction failed"
                    );
                    shutdown_journal_bridge(state);
                    log_journal_open_failed();
                    return Err(OpenJournalError::OpenFailed);
                }
            };
            log_journal_window_state(&window, "built");
            let started = Instant::now();
            let navigated = tokio::time::timeout(JOURNAL_READY_TIMEOUT, page_loaded.notified())
                .await
                .is_ok();
            let elapsed_ms = started.elapsed().as_millis() as u64;
            let usable_reason = journal_window_is_usable(&window);
            let usable = usable_reason.is_none();
            if !navigated || !usable {
                let bridge_contacted = state
                    .journal_bridge
                    .lock()
                    .ok()
                    .and_then(|guard| guard.as_ref().map(|handle| handle.contacted()))
                    .unwrap_or(false);
                let page_started = page_load_started.load(Ordering::Relaxed);
                let mode = classify_journal_open_failure(navigated, page_started, bridge_contacted, usable);
                let url = window
                    .url()
                    .map(|u| strip_cap(u.as_str()))
                    .unwrap_or_else(|_| "url_error".to_string());
                tracing::warn!(
                    target: "window",
                    label = "journal",
                    mode = mode.token(),
                    usable_failure_reason = usable_reason.as_ref().map(UsableFailureReason::token).unwrap_or("none"),
                    bridge_contacted,
                    page_load_started = page_started,
                    navigated,
                    usable,
                    url = %url,
                    elapsed_ms,
                    "journal open failed"
                );
                log_journal_window_state(&window, "readiness_failed");
                window.close().ok();
                shutdown_journal_bridge(state);
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
        },
    )
    .await
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

fn journal_window_is_usable(window: &WebviewWindow) -> Option<UsableFailureReason> {
    usable_failure_reason(
        window.is_visible().ok(),
        window.is_minimized().ok(),
        window
            .inner_size()
            .ok()
            .map(|size| (size.width, size.height)),
        window
            .outer_size()
            .ok()
            .map(|size| (size.width, size.height)),
    )
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
    page_load_started: Arc<AtomicBool>,
) -> tauri::Result<WebviewWindow> {
    let (tx, rx) = oneshot::channel();
    let app_for_main = app.clone();
    app.run_on_main_thread(move || {
        let res = build_journal_window(&app_for_main, &url, page_loaded, page_load_started);
        let _ = tx.send(res);
    })?;

    rx.await.map_err(|_| tauri::Error::FailedToReceiveMessage)?
}

fn build_journal_window(
    app: &tauri::AppHandle,
    url: &str,
    page_loaded: Arc<Notify>,
    page_load_started: Arc<AtomicBool>,
) -> tauri::Result<WebviewWindow> {
    let parsed: tauri::Url = url.parse().map_err(tauri::Error::InvalidUrl)?;
    let origin = journal_window::JournalOrigin::from_url(&parsed)
        .ok_or(tauri::Error::InvalidWebviewUrl("journal bootstrap origin"))?;
    #[cfg(windows)]
    let bootstrap_url = parsed.clone();
    // wry Navigates during build, before with_webview; construct on about:blank,
    // then navigate the bootstrap URL only after comprehensive filtering is installed.
    let placeholder = tauri::Url::parse("about:blank").expect("valid placeholder URL");
    let builder = WebviewWindowBuilder::new(app, "journal", WebviewUrl::External(placeholder))
        .title("your journal")
        .inner_size(1100.0, 800.0)
        .min_inner_size(640.0, 480.0)
        .additional_browser_args(WEBVIEW_ARGS)
        .visible(false);
    let builder = match journal_window::bundled_initialization_script() {
        Ok(script) => builder.initialization_script(script),
        Err(error) => {
            tracing::warn!(
                target: "window",
                label = "journal",
                token = error.token(),
                "journal host contract"
            );
            builder
        }
    };
    let navigation_origin = origin.clone();
    let navigation_app = app.clone();
    let new_window_origin = origin.clone();
    let new_window_app = app.clone();
    let initiating_window = Arc::new(OnceLock::<WebviewWindow>::new());
    let new_window_source = Arc::clone(&initiating_window);
    let page_load_origin = origin.clone();
    let builder = builder
        // Locked Tauri 2.11.2 (`tauri-runtime-wry` 2.11.2) parses the URI before
        // these callbacks. A main-frame navigation whose URI fails `Url::parse` is
        // allowed (`unwrap_or(true)`) and never reaches `on_navigation`. A new-window
        // URI that fails `Url::parse` is denied before `on_new_window`. This repair
        // does not change that bound.
        .on_navigation(move |url| {
            let decision = journal_window::decide(
                &navigation_origin,
                journal_window::RequestKind::MainFrame,
                url,
            );
            let decision = journal_window::apply(
                decision,
                url,
                |destination| {
                    use tauri_plugin_opener::OpenerExt;
                    match navigation_app
                        .opener()
                        .open_url(destination.as_str(), None::<&str>)
                    {
                        Ok(()) => Ok(()),
                        Err(_) => {
                            log_journal_handoff(destination, "open_failed");
                            Err(())
                        }
                    }
                },
                |_| Ok(()),
            );
            if decision.allows_main_frame() {
                tracing::info!(
                    target: "window",
                    label = "journal",
                    scheme = url.scheme(),
                    host = url.host_str().unwrap_or(""),
                    port = ?url.port_or_known_default(),
                    "journal navigation"
                );
            }
            decision.allows_main_frame()
        })
        .on_new_window(move |url, _features| {
            let decision = journal_window::decide(
                &new_window_origin,
                journal_window::RequestKind::NewWindow,
                &url,
            );
            let _ = journal_window::apply(
                decision,
                &url,
                |destination| {
                    use tauri_plugin_opener::OpenerExt;
                    match new_window_app
                        .opener()
                        .open_url(destination.as_str(), None::<&str>)
                    {
                        Ok(()) => Ok(()),
                        Err(_) => {
                            log_journal_handoff(destination, "open_failed");
                            Err(())
                        }
                    }
                },
                |destination| {
                    let Some(window) = new_window_source.get().cloned() else {
                        log_journal_handoff(destination, "closing");
                        return Err(());
                    };
                    let destination = destination.clone();
                    let log_destination = destination.clone();
                    std::thread::spawn(move || {
                        if window.navigate(destination).is_err() {
                            log_journal_handoff(&log_destination, "closing");
                        }
                    });
                    Ok(())
                },
            );
            tauri::webview::NewWindowResponse::Deny
        })
        .on_page_load(move |_window, payload| {
            let kind = match payload.event() {
                PageLoadEvent::Started => journal_window::PageLoadKind::Started,
                PageLoadEvent::Finished => journal_window::PageLoadKind::Finished,
            };
            let effect =
                journal_window::page_load_effect(&page_load_origin, kind, payload.url().as_str());
            if effect.page_started {
                page_load_started.store(true, Ordering::Relaxed);
            }
            tracing::info!(
                target: "window",
                label = "journal",
                event = match payload.event() {
                    PageLoadEvent::Started => "started",
                    PageLoadEvent::Finished => "finished",
                },
                "journal page load"
            );
            if effect.ready {
                page_loaded.notify_one();
            }
        });

    #[cfg(not(windows))]
    {
        let effect =
            journal_window::filter_install_effect(journal_window::FilterInstall::MissingInterface);
        debug_assert!(effect.fail_closed && !effect.navigate);
        drop(builder);
        return Err(tauri::Error::InvalidWebviewUrl("journal document filter"));
    }

    #[cfg(windows)]
    {
        let window = builder.build()?;
        if initiating_window.set(window.clone()).is_err() {
            window.close().ok();
            return Err(tauri::Error::InvalidWebviewUrl("journal document filter"));
        }
        let installed = Arc::new(std::sync::Mutex::new(None));
        let installed_for_callback = Arc::clone(&installed);
        let window_for_callback = window.clone();
        let with_webview_result = window.with_webview(move |webview| {
            let terminate_window = window_for_callback.clone();
            let install = platform_win::journal_document_filter::install_document_filter(
                webview.controller(),
                webview.environment(),
                origin,
                move |_fault| {
                    terminate_window.close().ok();
                },
            );
            *installed_for_callback
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(install);
        });
        if with_webview_result.is_err() {
            window.close().ok();
            return Err(tauri::Error::InvalidWebviewUrl("journal document filter"));
        }

        let installed = installed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let window_still_present = app.get_webview_window("journal").is_some();
        let effect = journal_window::deferred_install_effect(window_still_present, installed);
        if !window_still_present {
            window.close().ok();
            return Err(tauri::Error::InvalidWebviewUrl("journal document filter"));
        }
        if effect.navigate {
            if window.navigate(bootstrap_url).is_err() {
                window.close().ok();
                return Err(tauri::Error::InvalidWebviewUrl("journal document filter"));
            }
        } else {
            window.close().ok();
            return Err(tauri::Error::InvalidWebviewUrl("journal document filter"));
        }

        if let Err(error) =
            window.set_size(tauri::Size::Logical(tauri::LogicalSize::new(1100.0, 800.0)))
        {
            window.close().ok();
            return Err(error);
        }
        window.center().ok();
        if let Err(error) = window.show() {
            window.close().ok();
            return Err(error);
        }
        window.set_focus().ok();
        tracing::info!(
            target: "window",
            label = "journal",
            visible_after_show = ?window.is_visible().ok(),
            "journal window shown"
        );
        Ok(window)
    }
}

fn log_journal_handoff(url: &tauri::Url, outcome: &'static str) {
    tracing::warn!(
        target: "window",
        label = "journal",
        outcome,
        scheme = url.scheme(),
        host = url.host_str().unwrap_or(""),
        port = ?url.port_or_known_default(),
        "journal handoff"
    );
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
