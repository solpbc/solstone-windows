// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The observer binary entry point.
//!
//! Tray-first: no window is shown at launch. Before booting the Tauri runtime,
//! `main` dispatches on the agent-native CLI surface — `--dump-state` prints the
//! honest [`HealthDump`](observer_model::HealthDump) JSON and exits; `--healthz`
//! is the same payload for liveness. Everything else falls through to the
//! tray-resident app.

#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

mod app;
mod exclusions;
mod health;
mod hotkey;
mod ipc;
mod lifecycle;
mod tray;
mod update;
mod windows;

use std::process::ExitCode;

use velopack::VelopackApp;

fn main() -> ExitCode {
    // Velopack-aware entry — MUST run first. For the installer lifecycle args
    // (--veloapp-install / -updated / -obsolete / -uninstall) `run()` acts and
    // terminates the process. The uninstall fast-callback removes the per-user
    // autostart login item so no stale `Run` entry survives the app's removal
    // (registration itself is ensured idempotently on every normal launch, in the
    // Tauri setup). For a normal launch (no veloapp arg) `run()` is a no-op and
    // falls through to the CLI surface / GUI below.
    VelopackApp::build()
        .on_before_uninstall_fast_callback(|_version| {
            let _ = platform_win::autostart::remove_login_item(
                platform_win::autostart::LOGIN_ITEM_NAME,
            );
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

    // Headless check + stage of an update (readies it for --apply-update).
    if args.iter().any(|a| a == "--check-update") {
        return update::check_update_cli();
    }

    // Headless apply of a staged update (the CLI analog of relaunch-to-install).
    if args.iter().any(|a| a == "--apply-update") {
        return update::apply_pending_cli();
    }

    // Agent-native exclusion diagnostic: the windows the enumerator sees on the
    // primary monitor, the active rules, and the resulting verdict, as JSON. Must
    // run in the interactive session to see the owner's desktop windows.
    if args.iter().any(|a| a == "--dump-windows") {
        println!("{}", exclusions::dump_windows_json());
        return ExitCode::SUCCESS;
    }

    app::run();
    ExitCode::SUCCESS
}
