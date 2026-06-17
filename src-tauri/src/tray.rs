// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Tray icon and menu.
//!
//! The tray icon is a **pure function of `app_state`** (idle / starting /
//! observing / paused / error) — it is driven by the honest
//! [`HealthDump`](observer_model::HealthDump) the engine emits, never set
//! optimistically. Menu items carry ids from the contract SoT
//! (`observer_contract::tray::*`) so the FlaUI harness can find them on the
//! native UIA surface.

use capture_engine::EngineCommand;
use observer_model::{AppPhase, PauseReason};
use tauri::image::Image;
use tauri::menu::{MenuBuilder, MenuItem, MenuItemBuilder, PredefinedMenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{App, Wry};
use tokio::sync::mpsc;

/// Install the tray icon + menu.
pub fn init(
    app: &mut App,
    cmd_tx: mpsc::UnboundedSender<EngineCommand>,
) -> tauri::Result<(TrayIcon, MenuItem<Wry>, MenuItem<Wry>, MenuItem<Wry>)> {
    let mi_start = MenuItemBuilder::with_id(observer_contract::tray::MENU_START, "Start")
        .enabled(false)
        .build(app)?;
    let mi_pause = MenuItemBuilder::with_id(observer_contract::tray::MENU_PAUSE, "Pause")
        .enabled(true)
        .build(app)?;
    let mi_resume = MenuItemBuilder::with_id(observer_contract::tray::MENU_RESUME, "Resume")
        .enabled(false)
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
        .item(&mi_start)
        .item(&mi_pause)
        .item(&mi_resume)
        .item(&sep_one)
        .item(&mi_open_settings)
        .item(&mi_about)
        .item(&sep_two)
        .item(&mi_quit)
        .build()?;

    let tray = TrayIconBuilder::with_id(observer_contract::tray::ROOT)
        .menu(&menu)
        .icon(icon_for(&AppPhase::Starting))
        .tooltip(tooltip_for(&AppPhase::Starting))
        .on_menu_event(move |app, event| match event.id().as_ref() {
            observer_contract::tray::MENU_START => {
                let _ = cmd_tx.send(EngineCommand::Start);
            }
            observer_contract::tray::MENU_PAUSE => {
                let _ = cmd_tx.send(EngineCommand::Pause(PauseReason::Operator));
            }
            observer_contract::tray::MENU_RESUME => {
                let _ = cmd_tx.send(EngineCommand::Resume);
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

    Ok((tray, mi_start, mi_pause, mi_resume))
}

pub fn apply_state(
    tray: &TrayIcon,
    mi_start: &MenuItem<Wry>,
    mi_pause: &MenuItem<Wry>,
    mi_resume: &MenuItem<Wry>,
    phase: &AppPhase,
) {
    let _ = tray.set_icon(Some(icon_for(phase)));
    let _ = tray.set_tooltip(Some(tooltip_for(phase)));
    let _ = mi_start.set_enabled(matches!(phase, AppPhase::Idle));
    let _ = mi_pause.set_enabled(matches!(phase, AppPhase::Starting | AppPhase::Observing));
    let _ = mi_resume.set_enabled(matches!(phase, AppPhase::Paused));
}

fn icon_for(phase: &AppPhase) -> Image<'static> {
    let bytes = match phase {
        AppPhase::Idle => include_bytes!("../icons/idle.png").as_slice(),
        AppPhase::Starting => include_bytes!("../icons/starting.png").as_slice(),
        AppPhase::Observing => include_bytes!("../icons/observing.png").as_slice(),
        AppPhase::Paused => include_bytes!("../icons/paused.png").as_slice(),
        AppPhase::Error => include_bytes!("../icons/error.png").as_slice(),
    };
    Image::from_bytes(bytes).expect("bundled tray icon is a valid PNG")
}

fn tooltip_for(phase: &AppPhase) -> &'static str {
    match phase {
        AppPhase::Idle => "solstone — idle",
        AppPhase::Starting => "solstone — starting",
        AppPhase::Observing => "solstone — observing",
        AppPhase::Paused => "solstone — paused",
        AppPhase::Error => "solstone — attention needed",
    }
}
