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

use observer_model::i18n::{message, Value};
use std::sync::{Arc, Mutex};

use capture_engine::EngineCommand;
use observer_model::{
    attention_source, classify_tray, menu_status_line, owner_fault_detail, permission_blocked,
    AppPhase, HealthDump, JournalLink, PairingPhase, PauseReason, SourceKind, SyncSnapshot,
    TrayVisual,
};
use observer_update::UpdateNotice;
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
    update: MenuItem<Wry>,
    status: MenuItem<Wry>,
    pause: Submenu<Wry>,
    resume: MenuItem<Wry>,
    journal: MenuItem<Wry>,
    check_updates: MenuItem<Wry>,
    attention_target: Arc<Mutex<String>>,
    attention_shown: bool,
    pause_shown: bool,
    resume_shown: bool,
    journal_shown: bool,
    update_shown: bool,
    check_updates_shown: bool,
    /// The icon's visual, and whether it carries the attention dot.
    icon_shown: Option<(TrayVisual, bool)>,
    tooltip_shown: Option<String>,
    status_shown: Option<String>,
    attention_text_shown: Option<String>,
    update_text_shown: Option<String>,
}

/// Install the tray icon and its native menu.
pub fn init(
    app: &mut App,
    cmd_tx: mpsc::UnboundedSender<EngineCommand>,
) -> tauri::Result<TrayMenu> {
    let mi_pause_15 = MenuItemBuilder::with_id(
        observer_contract::tray::MENU_PAUSE_15M,
        &message("windows.ui.value15minutes", &[]),
    )
    .build(app)?;
    let mi_pause_30 = MenuItemBuilder::with_id(
        observer_contract::tray::MENU_PAUSE_30M,
        &message("windows.ui.value30minutes", &[]),
    )
    .build(app)?;
    let mi_pause_1h = MenuItemBuilder::with_id(
        observer_contract::tray::MENU_PAUSE_1H,
        &message("windows.ui.value1hour", &[]),
    )
    .build(app)?;
    let mi_pause_indef = MenuItemBuilder::with_id(
        observer_contract::tray::MENU_PAUSE_INDEFINITE,
        &message("windows.ui.untiliresume", &[]),
    )
    .build(app)?;

    let pause = SubmenuBuilder::with_id(
        app,
        observer_contract::tray::MENU_PAUSE,
        &message("windows.desktop.shared.setup.wizardpausefailure", &[]),
    )
    .item(&mi_pause_15)
    .item(&mi_pause_30)
    .item(&mi_pause_1h)
    .item(&mi_pause_indef)
    .build()?;

    let resume = MenuItemBuilder::with_id(
        observer_contract::tray::MENU_RESUME,
        &message("windows.desktop.shared.setup.wizardpausefailure2", &[]),
    )
    .build(app)?;
    let attention = MenuItemBuilder::with_id(
        observer_contract::tray::MENU_ATTENTION,
        &message("windows.desktop.shared.copy.settingstabattentiona11y", &[]),
    )
    .build(app)?;
    let update = MenuItemBuilder::with_id(
        observer_contract::tray::MENU_UPDATE,
        &message("windows.tray.updateavailable", &[]),
    )
    .build(app)?;
    let check_updates = MenuItemBuilder::with_id(
        observer_contract::tray::MENU_CHECK_UPDATES,
        &message("windows.tray.checkforupdates", &[]),
    )
    .build(app)?;
    let status = MenuItemBuilder::with_id(
        observer_contract::tray::MENU_STATUS,
        &message("windows.ui.connecting", &[]),
    )
    .enabled(false)
    .build(app)?;
    let journal = MenuItemBuilder::with_id(
        observer_contract::tray::MENU_OPEN_JOURNAL,
        &message("windows.desktop.shared.copy.settingsjournalopen", &[]),
    )
    .build(app)?;
    let open_settings = MenuItemBuilder::with_id(
        observer_contract::tray::MENU_OPEN_SETTINGS,
        &message("windows.ui.opensettings", &[]),
    )
    .build(app)?;
    let about = MenuItemBuilder::with_id(
        observer_contract::tray::MENU_ABOUT,
        &message("windows.desktop.shared.app.aboutsolstone", &[]),
    )
    .build(app)?;
    let quit = MenuItemBuilder::with_id(
        observer_contract::tray::MENU_QUIT,
        &message("windows.ui.quitsolstone", &[]),
    )
    .build(app)?;

    // Status, pause, settings, about, check for updates, quit. Attention, the
    // update line, resume, and the journal are inserted when the state calls for
    // them. Pause and resume are never both here, nor the update line and the check.
    let menu = MenuBuilder::new(app)
        .item(&status)
        .item(&pause)
        .item(&open_settings)
        .item(&about)
        .item(&check_updates)
        .item(&quit)
        .build()?;

    let (visual, _starting) = classify_tray(
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
        .icon(icon_for(visual, false))
        // Windows keeps the first tooltip as the icon's name in the taskbar and
        // its settings, ahead of every later one, so it is the app's name.
        .tooltip("solstone")
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
            observer_contract::tray::MENU_UPDATE => {
                let app = app.clone();
                std::thread::spawn(move || {
                    let _ = crate::windows::open_settings_updates(&app);
                });
            }
            observer_contract::tray::MENU_CHECK_UPDATES => {
                use tauri::Manager;
                app.state::<crate::update::UpdateController>().check();
                let app = app.clone();
                std::thread::spawn(move || {
                    let _ = crate::windows::open_settings_updates(&app);
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
        update,
        status,
        pause,
        resume,
        journal,
        check_updates,
        attention_target,
        attention_shown: false,
        pause_shown: true,
        resume_shown: false,
        journal_shown: false,
        update_shown: false,
        check_updates_shown: true,
        icon_shown: None,
        tooltip_shown: None,
        status_shown: None,
        attention_text_shown: None,
        update_text_shown: None,
    })
}

/// The attention row is named for the owner. The page route is the source id.
fn attention_route(name: &str) -> &str {
    match name {
        "Obsidian" => "obsidian",
        "screenshots" => "screenshots",
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

/// Hand `value` to the tray only when it differs from what the tray already
/// shows. A value the tray refused is offered again on the next render.
fn show<T: PartialEq>(shown: &mut Option<T>, value: T, apply: impl FnOnce(&T) -> bool) {
    if shown.as_ref() == Some(&value) {
        return;
    }
    if apply(&value) {
        *shown = Some(value);
    }
}

/// The tray menu's update line for `notice`.
fn update_line(notice: UpdateNotice) -> String {
    match notice {
        UpdateNotice::Ready => message("windows.tray.updateready", &[]),
        UpdateNotice::Available => message("windows.tray.updateavailable", &[]),
        UpdateNotice::CheckFailed => message("windows.tray.updatecheckfailed", &[]),
    }
}

/// Re-render the tray from `dump` and the update `notice`; returns what it
/// shows, for the notifier. The icon carries its dot exactly when the menu
/// names an update, so the dot always has words.
pub fn apply_state(
    menu: &mut TrayMenu,
    dump: &HealthDump,
    notice: Option<UpdateNotice>,
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
    let tray = &menu.tray;
    show(
        &mut menu.icon_shown,
        (visual, notice.is_some()),
        |(visual, dot)| tray.set_icon(Some(icon_for(*visual, *dot))).is_ok(),
    );
    show(&mut menu.tooltip_shown, tooltip, |tooltip| {
        tray.set_tooltip(Some(tooltip.as_str())).is_ok()
    });
    let status = &menu.status;
    show(&mut menu.status_shown, menu_status_line(dump), |line| {
        status.set_text(line).is_ok()
    });

    #[cfg(feature = "browser-host")]
    let browsers = crate::browser::needs_attention();
    #[cfg(not(feature = "browser-host"))]
    let browsers = false;
    let source = attention_source(
        &dump.sources,
        |kind| dump.setup.is_enabled(kind),
        browsers,
        crate::obsidian::needs_attention(),
        crate::screenshots::needs_attention(),
        crate::folders::needs_attention(),
        dump.sync.imports_need_owner,
        dump.app_state == AppPhase::Paused,
    );
    if let Some(name) = source {
        let attention = &menu.attention;
        show(
            &mut menu.attention_text_shown,
            message(
                "windows.ui.arg0needsattention",
                &[(
                    "arg0",
                    Value::String(&(owner_attention_name(name)).to_string()),
                )],
            ),
            |text| attention.set_text(text).is_ok(),
        );
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

    if let Some(notice) = notice {
        let update = &menu.update;
        show(&mut menu.update_text_shown, update_line(notice), |text| {
            update.set_text(text).is_ok()
        });
    }
    let update_at = usize::from(menu.attention_shown);
    place(
        &menu.menu,
        &menu.update,
        &mut menu.update_shown,
        notice.is_some(),
        update_at,
    );
    let above_status = usize::from(menu.attention_shown) + usize::from(menu.update_shown);

    let paused = dump.app_state == AppPhase::Paused;
    let pause_at = above_status + 1;
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
    let journal_at = above_status + 3;
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

    // Under about, while no update line shows.
    let check_at = above_status + 4 + usize::from(menu.journal_shown);
    place(
        &menu.menu,
        &menu.check_updates,
        &mut menu.check_updates_shown,
        notice.is_none(),
        check_at,
    );
    (visual, blocked)
}

fn icon_for(visual: TrayVisual, dot: bool) -> Image<'static> {
    let bytes = match (visual, dot) {
        (TrayVisual::Healthy, false) => include_bytes!("../icons/tray/healthy.ico").as_slice(),
        (TrayVisual::Healthy, true) => include_bytes!("../icons/tray/healthy-dot.ico").as_slice(),
        (TrayVisual::Connecting, false) => {
            include_bytes!("../icons/tray/connecting.ico").as_slice()
        }
        (TrayVisual::Connecting, true) => {
            include_bytes!("../icons/tray/connecting-dot.ico").as_slice()
        }
        (TrayVisual::Paused | TrayVisual::Off, false) => {
            include_bytes!("../icons/tray/paused.ico").as_slice()
        }
        (TrayVisual::Paused | TrayVisual::Off, true) => {
            include_bytes!("../icons/tray/paused-dot.ico").as_slice()
        }
        (TrayVisual::Offline, false) => include_bytes!("../icons/tray/offline.ico").as_slice(),
        (TrayVisual::Offline, true) => include_bytes!("../icons/tray/offline-dot.ico").as_slice(),
        (TrayVisual::Error | TrayVisual::NeedsPermission, false) => {
            include_bytes!("../icons/tray/error.ico").as_slice()
        }
        (TrayVisual::Error | TrayVisual::NeedsPermission, true) => {
            include_bytes!("../icons/tray/error-dot.ico").as_slice()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unchanged_render_hands_the_tray_nothing() {
        let mut shown = None;
        let mut sets = 0;
        for _ in 0..100 {
            show(&mut shown, "observing".to_string(), |_| {
                sets += 1;
                true
            });
        }
        assert_eq!(sets, 1);
        show(&mut shown, "paused".to_string(), |_| {
            sets += 1;
            true
        });
        assert_eq!(sets, 2);
    }

    #[test]
    fn a_value_the_tray_refused_is_offered_again() {
        let mut shown = None;
        let mut sets = 0;
        show(&mut shown, TrayVisual::Healthy, |_| {
            sets += 1;
            false
        });
        show(&mut shown, TrayVisual::Healthy, |_| {
            sets += 1;
            true
        });
        show(&mut shown, TrayVisual::Healthy, |_| {
            sets += 1;
            true
        });
        assert_eq!(sets, 2);
        assert_eq!(shown, Some(TrayVisual::Healthy));
    }
}

fn owner_attention_name(name: &str) -> String {
    match name {
        "audio" => message("windows.ui.audio", &[]),
        "screen" => message("windows.ui.screen", &[]),
        "browsers" => message("windows.ui.browsers", &[]),
        "screenshots" => message("windows.ui.screenshots", &[]),
        "folders" => message("windows.folders.tiletitle", &[]),
        "import" => message("windows.ui.import", &[]),
        _ => name.to_string(),
    }
}
