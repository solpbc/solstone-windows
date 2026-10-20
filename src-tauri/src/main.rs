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
mod logs;
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
    if early_args.iter().any(|a| a == "--desktop-url-helper") {
        let code = platform_win::desktop_activity::run_url_helper_cli();
        return ExitCode::from(code as u8);
    }
    if let Some(code) = browser::dispatch(&early_args) {
        return code;
    }

    // Startup or the CLI handler applies a downloaded update before GUI startup
    // (killing every process under `current\`): ask the running app to send its
    // browser hosts away first. Velopack's own hook invocations skip this.
    #[cfg(feature = "browser-host")]
    if !early_args
        .first()
        .is_some_and(|a| a.starts_with("--veloapp-"))
        && update::startup_apply_pending()
    {
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
        .set_auto_apply_on_startup(!early_args.iter().any(|a| a == "--apply-update"))
        .on_before_uninstall_fast_callback(|_version| {
            if let Ok(exe) = std::env::current_exe() {
                let _ = platform_win::autostart::remove_login_item_if_matches(
                    platform_win::autostart::LOGIN_ITEM_NAME,
                    &exe,
                    &[observer_model::FROM_AUTOSTART_ARG],
                );
                if let Ok(current) = platform_win::scheme::read_scheme_command() {
                    if observer_model::scheme::scheme_uninstall_should_remove(
                        current.as_deref(),
                        &exe,
                    ) {
                        let _ = platform_win::scheme::remove_scheme_handler();
                    }
                }
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
    if let Some(code) = integration::dispatch(&args) {
        return code;
    }

    if args.iter().any(|a| a == "--log-path") {
        use std::io::Write as _;

        let path = observer_log::active_log_path(&platform_win::logs_dir());
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

    if !observer_model::setup::SETUP_SEQUENCE_ENABLED
        && args
            .iter()
            .any(|arg| observer_model::setup::is_setup_ingress(arg))
    {
        eprintln!("setup continuation is unavailable in this build");
        return ExitCode::FAILURE;
    }

    let mut reload_setup = false;
    let data_root = platform_win::local_data_root();
    let state_path = data_root.join("pairing.json");
    let answer_path = pl_transport_win::answer::answer_path(&state_path);
    let paired_load = pl_transport_win::credential::PairedState::load(&state_path).ok();
    let is_paired = paired_load.as_ref().is_some_and(|p| p.is_paired());
    let ceremony_active = paired_load.as_ref().is_some_and(|p| {
        if p.is_paired() {
            let cred = p.credential.as_ref().unwrap();
            let binding =
                pl_transport_win::ack::JournalIdentity::from_credential(cred).client_cert_sha256;
            let confirmed = pl_transport_win::answer::read_answer(&answer_path)
                .ok()
                .flatten()
                .is_some_and(|a| a.confirmed == binding);
            !confirmed
        } else {
            false
        }
    });

    for arg in &args {
        let trimmed = arg.trim();
        if trimmed
            .get(..11)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("solstone://"))
        {
            if let Some(canonical) = pl_transport_win::pairing::normalize_desktop_link(trimmed) {
                reload_setup = true;
                let _ = pl_transport_win::draft::write_draft_if_allowed(
                    &data_root,
                    &canonical,
                    is_paired,
                    ceremony_active,
                );
            }
        } else if trimmed == observer_model::SETUP_HANDOFF_ARG {
            reload_setup = true;
        }
    }

    let surface_on_launch = observer_model::launch_should_surface(&args);

    observer_log::init(
        &platform_win::logs_dir(),
        std::env::var("RUST_LOG").ok().as_deref(),
    );
    app::run(open_view, surface_on_launch, open_journal, reload_setup);
    ExitCode::SUCCESS
}
