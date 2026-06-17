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

fn main() -> ExitCode {
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

    // Velopack lifecycle hooks (--veloapp-install/-update/-obsolete/-firstrun)
    // are handled by the packaging layer before the app proper; the app only
    // needs to be Velopack-aware. See packaging/hooks/.
    app::run();
    ExitCode::SUCCESS
}
