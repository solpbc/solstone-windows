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

use std::sync::{Arc, Mutex};

use capture_engine::EngineCommand;
use observer_model::{
    attention_source, classify_tray, menu_status_line, owner_fault_detail, permission_blocked,
    AppPhase, HealthDump, JournalLink, PairingPhase, PauseReason, SourceKind, SyncSnapshot,
    TrayVisual,
};
use tauri::image::Image;
use tauri::menu::{
    IsMenuItem, Menu, MenuBuilder, MenuItem, MenuItemBuilder, Submenu, SubmenuBuilder,
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

pub struct TrayMenu {
    tray: TrayIcon,
    menu: Menu<Wry>,
    attention: MenuItem<Wry>,
    status: MenuItem<Wry>,
    pause: Submenu<Wry>,
    resume: MenuItem<Wry>,
    journal: MenuItem<Wry>,
    attention_target: Arc<Mutex<String>>,
    attention_shown: bool,
    pause_shown: bool,
    resume_shown: bool,
    journal_shown: bool,
}

/// Install the tray icon and its native menu.
pub fn init(
    app: &mut App,
    cmd_tx: mpsc::UnboundedSender<EngineCommand>,
) -> tauri::Result<TrayMenu> {
    let mi_pause_15 =
        MenuItemBuilder::with_id(observer_contract::tray::MENU_PAUSE_15M, "15 minutes")
            .build(app)?;
    let mi_pause_30 =
        MenuItemBuilder::with_id(observer_contract::tray::MENU_PAUSE_30M, "30 minutes")
            .build(app)?;
    let mi_pause_1h =
        MenuItemBuilder::with_id(observer_contract::tray::MENU_PAUSE_1H, "1 hour").build(app)?;
    let mi_pause_indef = MenuItemBuilder::with_id(
        observer_contract::tray::MENU_PAUSE_INDEFINITE,
        "until I resume",
    )
    .build(app)?;

    let pause = SubmenuBuilder::with_id(app, observer_contract::tray::MENU_PAUSE, "pause")
        .item(&mi_pause_15)
        .item(&mi_pause_30)
        .item(&mi_pause_1h)
        .item(&mi_pause_indef)
        .build()?;

    let resume =
        MenuItemBuilder::with_id(observer_contract::tray::MENU_RESUME, "resume").build(app)?;
    let attention =
        MenuItemBuilder::with_id(observer_contract::tray::MENU_ATTENTION, "needs attention")
            .build(app)?;
    let status = MenuItemBuilder::with_id(observer_contract::tray::MENU_STATUS, "connecting…")
        .enabled(false)
        .build(app)?;
    let journal =
        MenuItemBuilder::with_id(observer_contract::tray::MENU_OPEN_JOURNAL, "open journal")
            .build(app)?;
    let open_settings =
        MenuItemBuilder::with_id(observer_contract::tray::MENU_OPEN_SETTINGS, "open settings")
            .build(app)?;
    let about = MenuItemBuilder::with_id(observer_contract::tray::MENU_ABOUT, "about solstone")
        .build(app)?;
    let quit =
        MenuItemBuilder::with_id(observer_contract::tray::MENU_QUIT, "quit solstone").build(app)?;

    // Status, pause, settings, about, quit. Attention, resume, and the journal
    // are inserted when the state calls for them. Pause and resume are never both here.
    let menu = MenuBuilder::new(app)
        .item(&status)
        .item(&pause)
        .item(&open_settings)
        .item(&about)
        .item(&quit)
        .build()?;

    let (visual, tooltip) = classify_tray(
        AppPhase::Starting,
        &SyncSnapshot::default(),
        None,
        None,
        None,
        false,
    );

    let attention_target = Arc::new(Mutex::new(String::new()));
    let attention_for_event = Arc::clone(&attention_target);
    let tray = TrayIconBuilder::with_id(observer_contract::tray::ROOT)
        .menu(&menu)
        .icon(icon_for(visual))
        .tooltip(tooltip)
        .show_menu_on_left_click(true)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            observer_contract::tray::MENU_ATTENTION => {
                let source = attention_for_event
                    .lock()
                    .map(|slot| slot.clone())
                    .unwrap_or_default();
                let app = app.clone();
                std::thread::spawn(move || {
                    let _ = crate::windows::open_settings_source(&app, &source);
                });
            }
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
            observer_contract::tray::MENU_QUIT => crate::lifecycle::quit(app),
            _ => {}
        })
        .build(app)?;

    Ok(TrayMenu {
        tray,
        menu,
        attention,
        status,
        pause,
        resume,
        journal,
        attention_target,
        attention_shown: false,
        pause_shown: true,
        resume_shown: false,
        journal_shown: false,
    })
}

/// The attention row is named for the owner. The page route is the source id.
fn attention_route(name: &str) -> &str {
    match name {
        "Obsidian" => "obsidian",
        other => other,
    }
}

fn place(menu: &Menu<Wry>, item: &impl IsMenuItem<Wry>, shown: &mut bool, want: bool, at: usize) {
    if *shown == want {
        return;
    }
    let result = if want {
        menu.insert(item, at)
    } else {
        menu.remove(item)
    };
    if result.is_ok() {
        *shown = want;
    }
}

/// Re-render the tray from `dump`; returns what it shows, for the notifier.
pub fn apply_state(menu: &mut TrayMenu, dump: &HealthDump) -> (TrayVisual, Option<SourceKind>) {
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
    let _ = menu.tray.set_icon(Some(icon_for(visual)));
    let _ = menu.tray.set_tooltip(Some(tooltip));
    let _ = menu.status.set_text(menu_status_line(dump));

    #[cfg(feature = "browser-host")]
    let browsers = crate::browser::needs_attention();
    #[cfg(not(feature = "browser-host"))]
    let browsers = false;
    let source = attention_source(
        &dump.sources,
        |kind| dump.setup.is_enabled(kind),
        browsers,
        crate::obsidian::needs_attention(),
    );
    if let Some(name) = source {
        let _ = menu.attention.set_text(format!("{name} needs attention"));
        if let Ok(mut slot) = menu.attention_target.lock() {
            *slot = attention_route(name).to_string();
        }
    }
    place(
        &menu.menu,
        &menu.attention,
        &mut menu.attention_shown,
        source.is_some(),
        0,
    );

    let paused = dump.app_state == AppPhase::Paused;
    let pause_at = usize::from(menu.attention_shown) + 1;
    if paused {
        place(
            &menu.menu,
            &menu.pause,
            &mut menu.pause_shown,
            false,
            pause_at,
        );
        place(
            &menu.menu,
            &menu.resume,
            &mut menu.resume_shown,
            true,
            pause_at,
        );
    } else {
        place(
            &menu.menu,
            &menu.resume,
            &mut menu.resume_shown,
            false,
            pause_at,
        );
        place(
            &menu.menu,
            &menu.pause,
            &mut menu.pause_shown,
            true,
            pause_at,
        );
    }

    let paired = dump.sync.pairing.phase == PairingPhase::Paired;
    let journal_at = usize::from(menu.attention_shown) + 3;
    place(
        &menu.menu,
        &menu.journal,
        &mut menu.journal_shown,
        paired,
        journal_at,
    );
    if paired {
        let reachable = matches!(dump.sync.journal_link, Some(JournalLink::Connected));
        let _ = menu.journal.set_enabled(reachable);
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
