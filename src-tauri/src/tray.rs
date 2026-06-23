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
//!
//! Pause is a **submenu of durations** (15m / 30m / 1h / until I resume) to
//! macOS parity. A bounded pause auto-resumes when it elapses (the engine owns
//! the timer); the tray tooltip shows the live time remaining while paused.

use capture_engine::EngineCommand;
use observer_model::{AppPhase, HealthDump, PauseReason, PauseSnapshot};
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
/// the Start item, the Pause submenu (enabled/disabled as a whole), and Resume.
pub fn init(
    app: &mut App,
    cmd_tx: mpsc::UnboundedSender<EngineCommand>,
) -> tauri::Result<(TrayIcon, MenuItem<Wry>, Submenu<Wry>, MenuItem<Wry>)> {
    let mi_start = MenuItemBuilder::with_id(observer_contract::tray::MENU_START, "Start")
        .enabled(false)
        .build(app)?;

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
        .item(&pause_submenu)
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
        .tooltip(tooltip_for(&AppPhase::Starting, None))
        .on_menu_event(move |app, event| match event.id().as_ref() {
            observer_contract::tray::MENU_START => {
                let _ = cmd_tx.send(EngineCommand::Start);
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

    Ok((tray, mi_start, pause_submenu, mi_resume))
}

pub fn apply_state(
    tray: &TrayIcon,
    mi_start: &MenuItem<Wry>,
    pause_submenu: &Submenu<Wry>,
    mi_resume: &MenuItem<Wry>,
    dump: &HealthDump,
) {
    let phase = &dump.app_state;
    let _ = tray.set_icon(Some(icon_for(phase)));
    let _ = tray.set_tooltip(Some(tooltip_for(phase, dump.pause.as_ref())));
    let _ = mi_start.set_enabled(matches!(phase, AppPhase::Idle));
    let _ = pause_submenu.set_enabled(matches!(phase, AppPhase::Starting | AppPhase::Observing));
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

fn tooltip_for(phase: &AppPhase, pause: Option<&PauseSnapshot>) -> String {
    match phase {
        AppPhase::Idle => "solstone — idle".to_string(),
        AppPhase::Starting => "solstone — starting".to_string(),
        AppPhase::Observing => "solstone — observing".to_string(),
        AppPhase::Paused => match pause.and_then(|p| p.seconds_remaining) {
            Some(secs) => format!("solstone — paused, {} left", format_remaining(secs)),
            None => "solstone — paused".to_string(),
        },
        AppPhase::Error => "solstone — attention needed".to_string(),
    }
}

/// Human countdown for the tray tooltip: "14 min", "1 hr 2 min", "less than a
/// minute". Whole-minute granularity matches the tooltip's once-a-second refresh.
fn format_remaining(secs: u64) -> String {
    let mins = secs / 60;
    if mins == 0 {
        "less than a minute".to_string()
    } else if mins < 60 {
        format!("{mins} min")
    } else {
        let (h, m) = (mins / 60, mins % 60);
        if m == 0 {
            format!("{h} hr")
        } else {
            format!("{h} hr {m} min")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remaining_formats_minutes_and_hours() {
        assert_eq!(format_remaining(0), "less than a minute");
        assert_eq!(format_remaining(59), "less than a minute");
        assert_eq!(format_remaining(60), "1 min");
        assert_eq!(format_remaining(14 * 60 + 30), "14 min");
        assert_eq!(format_remaining(60 * 60), "1 hr");
        assert_eq!(format_remaining(62 * 60), "1 hr 2 min");
    }
}
