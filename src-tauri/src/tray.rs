// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Tray icon and menu.
//!
//! The tray icon and tooltip are derived by
//! [`classify_tray`](observer_model::classify_tray) from the honest
//! [`HealthDump`](observer_model::HealthDump): app state plus sync state and
//! pause detail. Menu items carry ids from the contract SoT
//! (`observer_contract::tray::*`) so the FlaUI harness can find them on the native
//! UIA surface.
//!
//! Pause is a **submenu of durations** (15m / 30m / 1h / until I resume) to
//! macOS parity. A bounded pause auto-resumes when it elapses (the engine owns
//! the timer); the tray tooltip shows the live time remaining while paused.

use capture_engine::EngineCommand;
use observer_model::{
    classify_tray, owner_fault_detail, pause_enabled, permission_blocked, restart_enabled,
    resume_enabled, AppPhase, HealthDump, PauseReason, SourceKind, SyncSnapshot, TrayVisual,
};
use tauri::image::Image;
use tauri::menu::{
    MenuBuilder, MenuItem, MenuItemBuilder, PredefinedMenuItem, Submenu, SubmenuBuilder,
};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{App, Wry};
use tokio::sync::mpsc;

const PAUSE_15M_SECS: u64 = 15 * 60;
const PAUSE_30M_SECS: u64 = 30 * 60;
const PAUSE_1H_SECS: u64 = 60 * 60;

/// An operator pause command for the given duration (`None` = until I resume).
fn pause_for(duration_secs: Option<u64>) -> EngineCommand {
    EngineCommand::Pause {
        reason: PauseReason::Operator,
        duration_secs,
    }
}

#[cfg(feature = "browser-host")]
fn browsers_menu_text(status: Option<&browser_host::hub::BrowserStatus>) -> String {
    let Some(status) = status else {
        return "browsers \u{00B7} none connected now".to_string();
    };
    let connected_brands: Vec<&str> = status.connected.iter().map(|c| c.brand.as_str()).collect();
    match browser_host::stores::browsers_tray(status.capture, &connected_brands) {
        browser_host::stores::BrowsersTray::Names(names) => {
            let display_names: Vec<&str> = names
                .iter()
                .map(|b| match b.as_str() {
                    "chrome" => "Chrome",
                    "edge" => "Edge",
                    "firefox" => "Firefox",
                    other => other,
                })
                .collect();
            format!("browsers \u{00B7} {}", display_names.join(", "))
        }
        browser_host::stores::BrowsersTray::Paused => "browsers \u{00B7} paused".to_string(),
        browser_host::stores::BrowsersTray::Off => "browsers \u{00B7} off".to_string(),
        browser_host::stores::BrowsersTray::None => {
            "browsers \u{00B7} none connected now".to_string()
        }
    }
}

type TrayHandles = (
    TrayIcon,
    Submenu<Wry>,
    MenuItem<Wry>,
    MenuItem<Wry>,
    Option<MenuItem<Wry>>,
);

/// Install the tray icon + menu. Returns the handles `apply_state` re-renders:
/// the Pause submenu (enabled/disabled as a whole), Resume, Restart intake, and Browsers.
pub fn init(
    app: &mut App,
    cmd_tx: mpsc::UnboundedSender<EngineCommand>,
) -> tauri::Result<TrayHandles> {
    let mi_pause_15 =
        MenuItemBuilder::with_id(observer_contract::tray::MENU_PAUSE_15M, "For 15 minutes")
            .build(app)?;
    let mi_pause_30 =
        MenuItemBuilder::with_id(observer_contract::tray::MENU_PAUSE_30M, "For 30 minutes")
            .build(app)?;
    let mi_pause_1h =
        MenuItemBuilder::with_id(observer_contract::tray::MENU_PAUSE_1H, "For 1 hour")
            .build(app)?;
    let mi_pause_indef = MenuItemBuilder::with_id(
        observer_contract::tray::MENU_PAUSE_INDEFINITE,
        "Until I resume",
    )
    .build(app)?;

    let pause_submenu = SubmenuBuilder::with_id(app, observer_contract::tray::MENU_PAUSE, "Pause")
        .item(&mi_pause_15)
        .item(&mi_pause_30)
        .item(&mi_pause_1h)
        .item(&mi_pause_indef)
        .build()?;

    let mi_resume = MenuItemBuilder::with_id(observer_contract::tray::MENU_RESUME, "Resume")
        .enabled(false)
        .build(app)?;
    let mi_restart = MenuItemBuilder::with_id(
        observer_contract::tray::MENU_RESTART_OBSERVING,
        "Restart intake",
    )
    .enabled(false)
    .build(app)?;
    #[cfg(feature = "browser-host")]
    let mi_browsers = Some(
        MenuItemBuilder::with_id(
            observer_contract::tray::MENU_BROWSERS,
            browsers_menu_text(crate::browser::status().as_ref()),
        )
        .build(app)?,
    );
    #[cfg(not(feature = "browser-host"))]
    let mi_browsers: Option<MenuItem<Wry>> = None;
    let mi_open_journal =
        MenuItemBuilder::with_id(observer_contract::tray::MENU_OPEN_JOURNAL, "Open Journal")
            .build(app)?;
    let mi_open_settings =
        MenuItemBuilder::with_id(observer_contract::tray::MENU_OPEN_SETTINGS, "Open Settings")
            .build(app)?;
    let mi_get_help =
        MenuItemBuilder::with_id(observer_contract::tray::MENU_GET_HELP, "get help").build(app)?;
    let mi_report_problem = MenuItemBuilder::with_id(
        observer_contract::tray::MENU_REPORT_PROBLEM,
        "report a problem",
    )
    .build(app)?;
    let mi_about =
        MenuItemBuilder::with_id(observer_contract::tray::MENU_ABOUT, "About").build(app)?;
    let mi_quit =
        MenuItemBuilder::with_id(observer_contract::tray::MENU_QUIT, "Quit").build(app)?;
    let sep_one = PredefinedMenuItem::separator(app)?;
    let sep_two = PredefinedMenuItem::separator(app)?;

    let mut menu_builder = MenuBuilder::new(app)
        .item(&pause_submenu)
        .item(&mi_resume)
        .item(&mi_restart)
        .item(&sep_one)
        .item(&mi_open_journal);
    if let Some(ref mi_b) = mi_browsers {
        menu_builder = menu_builder.item(mi_b);
    }
    let menu = menu_builder
        .item(&mi_open_settings)
        .item(&mi_get_help)
        .item(&mi_report_problem)
        .item(&mi_about)
        .item(&sep_two)
        .item(&mi_quit)
        .build()?;

    let (visual, tooltip) = classify_tray(
        AppPhase::Starting,
        &SyncSnapshot::default(),
        None,
        None,
        None,
        false,
    );

    let tray = TrayIconBuilder::with_id(observer_contract::tray::ROOT)
        .menu(&menu)
        .icon(icon_for(visual))
        .tooltip(tooltip)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            observer_contract::tray::MENU_PAUSE_15M => {
                let _ = cmd_tx.send(pause_for(Some(PAUSE_15M_SECS)));
            }
            observer_contract::tray::MENU_PAUSE_30M => {
                let _ = cmd_tx.send(pause_for(Some(PAUSE_30M_SECS)));
            }
            observer_contract::tray::MENU_PAUSE_1H => {
                let _ = cmd_tx.send(pause_for(Some(PAUSE_1H_SECS)));
            }
            observer_contract::tray::MENU_PAUSE_INDEFINITE => {
                let _ = cmd_tx.send(pause_for(None));
            }
            observer_contract::tray::MENU_RESUME => {
                let _ = cmd_tx.send(EngineCommand::Resume);
            }
            observer_contract::tray::MENU_RESTART_OBSERVING => {
                let _ = cmd_tx.send(EngineCommand::Resume);
            }
            observer_contract::tray::MENU_BROWSERS => {
                let app = app.clone();
                std::thread::spawn(move || {
                    let _ = crate::windows::open_settings_sources(&app);
                });
            }
            observer_contract::tray::MENU_OPEN_JOURNAL => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    use tauri::Manager;
                    let state = app.state::<crate::app::AppState>();
                    open_journal_from_menu(&state, &app, Some(&app)).await;
                });
            }
            observer_contract::tray::MENU_OPEN_SETTINGS => {
                let app = app.clone();
                std::thread::spawn(move || {
                    let _ = crate::windows::open_settings(&app);
                });
            }
            observer_contract::tray::MENU_GET_HELP => {
                use tauri_plugin_opener::OpenerExt;
                let _ = app
                    .opener()
                    .open_url(crate::support::HELP_URL, None::<&str>);
            }
            observer_contract::tray::MENU_REPORT_PROBLEM => {
                use tauri::Manager;
                use tauri_plugin_opener::OpenerExt;
                let state = app.state::<crate::app::AppState>();
                if let Ok(dump) = state.health.lock().map(|health| health.clone()) {
                    let _ = app
                        .opener()
                        .open_url(crate::support::report_url(&dump), None::<&str>);
                };
            }
            observer_contract::tray::MENU_ABOUT => {
                let app = app.clone();
                std::thread::spawn(move || {
                    let _ = crate::windows::open_about(&app);
                });
            }
            observer_contract::tray::MENU_QUIT => app.exit(0),
            _ => {}
        })
        .build(app)?;

    Ok((tray, pause_submenu, mi_resume, mi_restart, mi_browsers))
}

/// Re-render the tray from `dump`; returns what it shows, for the notifier.
pub fn apply_state(
    tray: &TrayIcon,
    pause_submenu: &Submenu<Wry>,
    mi_resume: &MenuItem<Wry>,
    mi_restart: &MenuItem<Wry>,
    browsers: Option<&MenuItem<Wry>>,
    dump: &HealthDump,
) -> (TrayVisual, Option<SourceKind>) {
    let fault_detail = owner_fault_detail(&dump.sources, dump.storage.as_ref(), |kind| {
        dump.setup.is_enabled(kind)
    });
    let blocked = permission_blocked(&dump.sources, |kind| dump.setup.is_enabled(kind));
    let deliberate_off = dump.setup.deliberate_off();
    let (visual, tooltip) = classify_tray(
        dump.app_state,
        &dump.sync,
        dump.pause.as_ref(),
        fault_detail.as_deref(),
        blocked,
        deliberate_off,
    );
    let _ = tray.set_icon(Some(icon_for(visual)));
    let _ = tray.set_tooltip(Some(tooltip));
    let _ = pause_submenu.set_enabled(pause_enabled(dump.app_state));
    let _ = mi_resume.set_enabled(resume_enabled(dump.app_state));
    let _ = mi_restart.set_enabled(restart_enabled(dump.app_state));
    #[cfg(feature = "browser-host")]
    if let Some(item) = browsers {
        let _ = item.set_text(browsers_menu_text(crate::browser::status().as_ref()));
    }
    #[cfg(not(feature = "browser-host"))]
    {
        let _ = browsers;
    }
    (visual, blocked)
}

fn icon_for(visual: TrayVisual) -> Image<'static> {
    let bytes = match visual {
        TrayVisual::Healthy => include_bytes!("../icons/tray/healthy.ico").as_slice(),
        TrayVisual::Connecting => include_bytes!("../icons/tray/connecting.ico").as_slice(),
        TrayVisual::Paused | TrayVisual::Off => {
            include_bytes!("../icons/tray/paused.ico").as_slice()
        }
        TrayVisual::Offline => include_bytes!("../icons/tray/offline.ico").as_slice(),
        TrayVisual::Error | TrayVisual::NeedsPermission => {
            include_bytes!("../icons/tray/error.ico").as_slice()
        }
    };
    Image::from_bytes(bytes).expect("bundled tray icon is a valid ICO")
}

pub(crate) async fn open_journal_from_menu<S: crate::windows::JournalSurface>(
    state: &crate::app::AppState,
    surface: &S,
    app: Option<&tauri::AppHandle>,
) {
    let _ = crate::windows::open_journal(state, surface, app).await;
}
