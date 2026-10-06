// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The observer binary entry point.
//!
//! Tray-first: no window is shown at launch. Before booting the Tauri runtime,
//! `main` dispatches on the agent-native CLI surface — `--dump-state` prints the
//! honest [`HealthDump`](observer_model::HealthDump) JSON and exits; `--healthz`
//! is the same payload for liveness; `--log-path` prints the persistent file-log
//! path without opening the writer. Everything else initializes the rotating file
//! log and falls through to the tray-resident app.

#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

mod app;
mod browser;
mod control;
mod exclusions;
mod health;
mod hotkey;
mod integration;
mod ipc;
mod lifecycle;
mod mic;
mod support;
mod tray;
mod update;
mod update_feed;
mod windows;

use std::process::ExitCode;

use velopack::VelopackApp;

fn main() -> ExitCode {
    // A browser's native-messaging launch is decided before anything else —
    // before Velopack's hooks, logging, the single-instance gate or a window:
    // it is the browser host relay (when this build has it) or it exits.
    let early_args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = browser::dispatch(&early_args) {
        return code;
    }

    let is_velo_hook = early_args
        .first()
        .is_some_and(|arg| arg.starts_with("--veloapp-"));
    let is_read_only = [
        "--dump-state",
        "--healthz",
        "--browser-status",
        "--dump-windows",
        "--log-path",
    ]
    .iter()
    .any(|flag| early_args.iter().any(|arg| arg == flag));
    let is_update_cli = ["--check-update", "--apply-update"]
        .iter()
        .any(|flag| early_args.iter().any(|arg| arg == flag));
    let is_integration = pl_transport_win::integration::is_selected(&early_args);
    let needs_owner_session = !is_velo_hook && !is_read_only && !is_update_cli;

    // Claim the same session mutex used by the GUI before adoption, startup
    // update work, integration writes, logging, or any owner-root seed.
    if needs_owner_session {
        match crate::lifecycle::acquire_single_instance() {
            platform_win::InstanceLock::Acquired => {}
            platform_win::InstanceLock::AlreadyRunning => {
                if is_integration {
                    eprintln!("integration refused: another app instance owns the profile");
                    return ExitCode::FAILURE;
                }
                let open_journal = early_args.iter().any(|arg| arg == "--open-journal");
                let view = early_args
                    .windows(2)
                    .find(|pair| pair[0] == "--open-view")
                    .and_then(|pair| observer_model::View::parse(&pair[1]));
                let acknowledged = if open_journal {
                    crate::control::signal_open_journal()
                } else if let Some(view) = view {
                    match view {
                        observer_model::View::Settings => crate::control::signal_surface(),
                        observer_model::View::About => crate::control::signal_surface_about(),
                    }
                } else if observer_model::launch_should_surface(&early_args) {
                    crate::control::signal_surface()
                } else {
                    true
                };
                return if acknowledged {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                };
            }
        }
        if let Err(error) = platform_win::owner_data::adopt_legacy_state() {
            eprintln!(
                "startup refused: owner data adoption failed; source data retained ({})",
                error.kind()
            );
            return ExitCode::FAILURE;
        }
    }

    // Neutralize `.betaId` and inspect startup staging before Velopack's own
    // startup hook can construct its updater. A failed check disables auto-apply
    // for this launch; the in-app and CLI paths report updater availability on
    // their own.
    let startup_apply_enabled = !is_velo_hook && !is_read_only && !is_update_cli && !is_integration;
    let startup_update = if startup_apply_enabled {
        update::prepare_startup_apply()
    } else {
        update::StartupApplyState::default()
    };

    // Startup may stop every process under `current\`; send browser hosts away
    // before Velopack applies the staged package.
    #[cfg(feature = "browser-host")]
    if startup_update.pending {
        browser::quiesce_before_apply();
    }

    // Velopack-aware entry — MUST run first after host mode. For the installer lifecycle args
    // (--veloapp-install / -updated / -obsolete / -uninstall) `run()` acts and
    // terminates the process. The uninstall fast-callback removes the per-user
    // autostart login item so no stale `Run` entry survives the app's removal —
    // only while the entry still names this executable, so it never deletes an
    // entry pointing at another copy (registration itself is ensured on every
    // normal launch by the copy that owns it, in the Tauri setup). For a normal
    // launch (no veloapp arg) `run()` is a no-op and falls through to the CLI
    // surface / GUI below.
    VelopackApp::build()
        // The explicit apply handler restarts with empty arguments. Startup's
        // automatic apply forwards argv, which would run --apply-update again
        // in the new process and exit because the package is already installed.
        .set_auto_apply_on_startup(startup_apply_enabled && startup_update.manager_ready)
        .on_before_uninstall_fast_callback(|_version| {
            if let Ok(exe) = std::env::current_exe() {
                let _ = platform_win::autostart::remove_login_item_if_matches(
                    platform_win::autostart::LOGIN_ITEM_NAME,
                    &exe,
                    &[observer_model::FROM_AUTOSTART_ARG],
                );
                #[cfg(feature = "browser-host")]
                browser::remove_registration(&exe);
            }
        })
        .run();

    let args: Vec<String> = std::env::args().skip(1).collect();

    // Agent-native CLI surface — handled before the GUI runtime boots.
    if args.iter().any(|a| a == "--dump-state" || a == "--healthz") {
        match health::dump_state_json() {
            Ok(json) => {
                println!("{json}");
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                eprintln!("failed to produce health dump: {e}");
                return ExitCode::FAILURE;
            }
        }
    }

    // The browser path's last published status (states and counts only).
    #[cfg(feature = "browser-host")]
    if args.iter().any(|a| a == "--browser-status") {
        return browser::status_cli();
    }

    // Headless check + stage of an update (readies it for --apply-update).
    if args.iter().any(|a| a == "--check-update") {
        return update::check_update_cli(&args);
    }

    // Headless apply of a staged update (the CLI analog of relaunch-to-install).
    if args.iter().any(|a| a == "--apply-update") {
        return update::apply_pending_cli(&args);
    }

    // Agent-native exclusion diagnostic: the windows the enumerator sees on the
    // primary monitor, the active rules, and the resulting verdict, as JSON. Must
    // run in the interactive session to see the owner's desktop windows.
    if args.iter().any(|a| a == "--dump-windows") {
        println!("{}", exclusions::dump_windows_json());
        return ExitCode::SUCCESS;
    }

    // Operator integration mode. The selection predicate and every decision live
    // in the gated `pl-transport-win` crate; this is dispatch only.
    if is_integration {
        return integration::dispatch(&args).unwrap_or(ExitCode::FAILURE);
    }

    if args.iter().any(|a| a == "--log-path") {
        use std::io::Write as _;

        let path = observer_log::active_log_path(&platform_win::existing_data_root().join("logs"));
        let absolute = if path.is_absolute() {
            path
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
                .join(path)
        };
        let _ = writeln!(std::io::stdout(), "{}", absolute.display());
        return ExitCode::SUCCESS;
    }

    let mut open_journal = false;
    let mut open_view: Option<observer_model::View> = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--open-journal" {
            open_journal = true;
        } else if args[i] == "--open-view" {
            match args
                .get(i + 1)
                .and_then(|name| observer_model::View::parse(name))
            {
                Some(view) => open_view = Some(view),
                None => {
                    eprintln!(
                        "--open-view: unknown view; valid: {}",
                        observer_model::View::valid_list()
                    );
                    open_view = None;
                }
            }
            i += 1;
        }
        i += 1;
    }

    let surface_on_launch = observer_model::launch_should_surface(&args);

    observer_log::init(
        &platform_win::logs_dir(),
        std::env::var("RUST_LOG").ok().as_deref(),
    );
    app::run(open_view, surface_on_launch, open_journal);
    ExitCode::SUCCESS
}
