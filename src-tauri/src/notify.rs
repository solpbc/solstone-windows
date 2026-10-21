// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Desktop notifications: the identity they show under, and posting one.
//!
//! Which state changes earn a notification is decided by
//! [`TransitionNotifier`](observer_model::TransitionNotifier).

use observer_model::Notice;

const DISPLAY_NAME: &str = "solstone";
/// The image the notification header shows beside the name. Windows reads it
/// from a file, so it is written under the data root.
const ICON: &[u8] = include_bytes!("../icons/icon.png");
const ICON_FILE_NAME: &str = "notification-icon.png";

fn icon_path() -> std::path::PathBuf {
    platform_win::local_data_root().join(ICON_FILE_NAME)
}

/// Write the icon file unless it already holds these bytes.
fn ensure_icon(path: &std::path::Path) -> std::io::Result<()> {
    if std::fs::read(path).is_ok_and(|bytes| bytes == ICON) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, ICON)
}

/// The id notifications show under, registered for this user. `None` for a
/// build that was not installed, which has no such id.
pub(crate) fn identity() -> Option<String> {
    use velopack::locator::{auto_locate_app_manifest, LocationContext};

    let exe = std::env::current_exe().ok()?;
    let locator =
        auto_locate_app_manifest(LocationContext::FromSpecifiedAppExecutable(exe)).ok()?;
    let aumid = locator.get_app_user_model_id();
    let icon = icon_path();
    // Without the file Windows shows its generic glyph; the toast still works.
    if let Err(error) = ensure_icon(&icon) {
        tracing::warn!(target: "notify", error = %error, "notification icon write failed");
    }
    match platform_win::toast::ensure_identity(&aumid, DISPLAY_NAME, &icon) {
        Ok(_) => Some(aumid),
        Err(error) => {
            tracing::warn!(
                target: "notify",
                error = %error,
                "notification identity registration failed"
            );
            None
        }
    }
}

/// Remove this copy's registration and the icon file; the uninstall hook
/// calls it.
pub(crate) fn remove_identity(exe: &std::path::Path) {
    use velopack::locator::{auto_locate_app_manifest, LocationContext};

    if let Ok(locator) = auto_locate_app_manifest(LocationContext::FromSpecifiedAppExecutable(
        exe.to_path_buf(),
    )) {
        let _ = platform_win::toast::remove_identity(&locator.get_app_user_model_id());
    }
    let _ = std::fs::remove_file(icon_path());
}

/// Post `notice` without blocking the caller.
pub(crate) fn post(aumid: &str, notice: Notice) {
    let aumid = aumid.to_string();
    std::thread::spawn(move || {
        match platform_win::toast::show(&aumid, &notice.title(), &notice.body()) {
            Ok(()) => tracing::info!(target: "notify", notice = ?notice, "notification shown"),
            Err(error) => tracing::warn!(
                target: "notify",
                notice = ?notice,
                error = %error,
                "notification failed"
            ),
        }
    });
}
