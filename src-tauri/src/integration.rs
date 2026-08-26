// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Thin dispatch for the operator integration mode.
//!
//! Following the `health.rs` / `exclusions.rs` precedent, nothing decision-bearing
//! lives here. The gated `pl-transport-win` crate owns argument parsing, the
//! result envelope, every operation, and the stdout/exit mapping — because no
//! repository gate compiles this crate, so logic placed here could not be tested.
//! This module supplies only the facts the binary alone knows: its profile
//! layout, its identity, and its build stamp.

use std::process::ExitCode;

use pl_transport_win::integration::{self, Environment};

/// Build the environment from the same derivation the GUI uses, so the mode runs
/// against the app's normal profile under its process environment.
///
/// `platform_win::local_data_root()` reads `LOCALAPPDATA` at call time, which is
/// what lets an operator point a run at a disposable profile.
fn environment() -> Environment {
    let host = crate::app::observer_hostname();
    Environment {
        state_path: platform_win::local_data_root().join("pairing.json"),
        segments_root: platform_win::segments_dir(),
        device_label: host,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        period_secs: capture_engine::EngineConfig::default().segment_secs,
        executable: std::env::current_exe().ok(),
        // Passed through raw; the gated crate decides what a valid commit is.
        source_commit: option_env!("SOLSTONE_SOURCE_COMMIT").map(str::to_string),
    }
}

/// Dispatch the mode when the arguments select it. `None` means this launch is
/// not an integration run and startup should continue untouched.
pub fn dispatch(args: &[String]) -> Option<ExitCode> {
    let selection = integration::selected(args)?;
    Some(ExitCode::from(integration::run_to_stdout(
        selection,
        &environment(),
    )))
}
