// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Scheme ingress notices. Target bytes never enter diagnostics.

use observer_model::i18n::message;
pub fn show_failure() {
    platform_win::notice::show(
        &message("windows.ui.couldntopenyourjournal", &[]),
        &message("windows.ui.tryopeningthelinkagain", &[]),
    );
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
                    &message("windows.ui.thisjournallinkisntvalid", &[]),
                    &message("windows.ui.copythefulllinkfromyourchatandopenitagain", &[]),
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
