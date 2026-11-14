// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Scheme ingress notices. Target bytes never enter diagnostics.

pub fn show_failure() {
    platform_win::notice::show("couldn't open your journal.", "try opening the link again.");
}

pub async fn open(app: tauri::AppHandle) {
    use tauri::Manager;
    let state = app.state::<crate::app::AppState>();
    let root = state
        .sync_config
        .state_path
        .parent()
        .expect("per-user state root");
    match pl_transport_win::journal_target::take_invalid_notice(root) {
        Ok(true) => {
            std::thread::spawn(|| {
                platform_win::notice::show(
                    "this journal link isn't valid.",
                    "copy the full link from your chat and open it again.",
                )
            });
        }
        Ok(false) => crate::control::open_journal_from_control(&state, &app, Some(&app)).await,
        Err(error) => {
            tracing::warn!(target: "window", error = ?error.kind(), "journal link handoff unavailable");
            std::thread::spawn(show_failure);
        }
    }
}
