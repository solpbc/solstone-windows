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
mod health;
mod ipc;
mod lifecycle;
mod tray;
mod windows;

use std::process::ExitCode;

use velopack::VelopackApp;

fn main() -> ExitCode {
    // Velopack-aware entry — MUST run first. For the installer lifecycle args
    // (--veloapp-install / -updated / -obsolete / -uninstall) `run()` acts and
    // terminates the process. On the first launch after install it fires
    // `on_first_run` (triggered by the VELOPACK_FIRSTRUN env) to mark per-user
    // autostart registration, then control CONTINUES to the tray. For a normal
    // launch (no veloapp arg, no firstrun env) `run()` is a no-op and falls
    // through to the CLI surface / GUI below.
    VelopackApp::build()
        .on_first_run(|_version| crate::lifecycle::mark_first_run())
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

    app::run();
    ExitCode::SUCCESS
}
