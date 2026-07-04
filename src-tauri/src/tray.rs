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
use observer_model::{classify_tray, AppPhase, HealthDump, PauseReason, SyncSnapshot, TrayVisual};
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

/// Install the tray icon + menu. Returns the handles `apply_state` re-renders:
/// the Pause submenu (enabled/disabled as a whole), and Resume.
pub fn init(
    app: &mut App,
    cmd_tx: mpsc::UnboundedSender<EngineCommand>,
) -> tauri::Result<(TrayIcon, Submenu<Wry>, MenuItem<Wry>)> {
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
    let mi_open_journal =
        MenuItemBuilder::with_id(observer_contract::tray::MENU_OPEN_JOURNAL, "Open Journal")
            .build(app)?;
    let mi_open_settings =
        MenuItemBuilder::with_id(observer_contract::tray::MENU_OPEN_SETTINGS, "Open Settings")
            .build(app)?;
    let mi_about =
        MenuItemBuilder::with_id(observer_contract::tray::MENU_ABOUT, "About").build(app)?;
    let mi_quit =
        MenuItemBuilder::with_id(observer_contract::tray::MENU_QUIT, "Quit").build(app)?;
    let sep_one = PredefinedMenuItem::separator(app)?;
    let sep_two = PredefinedMenuItem::separator(app)?;

    let menu = MenuBuilder::new(app)
        .item(&pause_submenu)
        .item(&mi_resume)
        .item(&sep_one)
        .item(&mi_open_journal)
        .item(&mi_open_settings)
        .item(&mi_about)
        .item(&sep_two)
        .item(&mi_quit)
        .build()?;

    let (visual, tooltip) = classify_tray(AppPhase::Starting, &SyncSnapshot::default(), None);

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
            observer_contract::tray::MENU_OPEN_JOURNAL => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = crate::windows::open_journal(&app).await;
                });
            }
            observer_contract::tray::MENU_OPEN_SETTINGS => {
                let app = app.clone();
                std::thread::spawn(move || {
                    let _ = crate::windows::open_settings(&app);
                });
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

    Ok((tray, pause_submenu, mi_resume))
}

pub fn apply_state(
    tray: &TrayIcon,
    pause_submenu: &Submenu<Wry>,
    mi_resume: &MenuItem<Wry>,
    dump: &HealthDump,
) {
    let phase = &dump.app_state;
    let (visual, tooltip) = classify_tray(dump.app_state, &dump.sync, dump.pause.as_ref());
    let _ = tray.set_icon(Some(icon_for(visual)));
    let _ = tray.set_tooltip(Some(tooltip));
    let _ = pause_submenu.set_enabled(matches!(phase, AppPhase::Starting | AppPhase::Observing));
    let _ = mi_resume.set_enabled(matches!(phase, AppPhase::Paused));
}

fn icon_for(visual: TrayVisual) -> Image<'static> {
    let bytes = match visual {
        TrayVisual::Full => include_bytes!("../icons/tray/full.ico").as_slice(),
        TrayVisual::Half => include_bytes!("../icons/tray/half.ico").as_slice(),
        TrayVisual::Cloud => include_bytes!("../icons/tray/paused.ico").as_slice(),
        TrayVisual::Error => include_bytes!("../icons/tray/error.ico").as_slice(),
        TrayVisual::Pending => include_bytes!("../icons/tray/pending.ico").as_slice(),
    };
    Image::from_bytes(bytes).expect("bundled tray icon is a valid ICO")
}
