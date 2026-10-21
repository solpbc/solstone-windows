// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Desktop notifications for the unpackaged app.
//!
//! Windows shows a toast only for an AppUserModelID it knows. The installer
//! sets the process id at startup; this module registers that id per user
//! under `HKCU\Software\Classes\AppUserModelId`, the documented route for an
//! unpackaged app (no elevation, no extra runtime), with the name and icon the
//! toast header shows, and removes it again on uninstall.
//!
//! Caller-directed: which state changes earn a toast is decided in
//! `observer_model::notice`.

use std::io;
use std::path::Path;

pub use crate::autostart::EnsureOutcome;

#[cfg_attr(not(windows), allow(dead_code))]
const AUMID_PARENT_SUBKEY: &str = r"Software\Classes\AppUserModelId";
#[cfg_attr(not(windows), allow(dead_code))]
const DISPLAY_NAME_VAL: &str = "DisplayName";
#[cfg_attr(not(windows), allow(dead_code))]
const ICON_URI_VAL: &str = "IconUri";
/// Every state toast carries the same tag, so a newer one replaces the older
/// in the notification center rather than piling up.
#[cfg_attr(not(windows), allow(dead_code))]
const STATE_TAG: &str = "state";

fn escape_xml(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

/// The toast document: a title line and a body line.
pub fn toast_xml(title: &str, body: &str) -> String {
    format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
        escape_xml(title),
        escape_xml(body)
    )
}

/// Register `aumid` for this user with `display_name` and the image file at
/// `icon`. Reads first and only writes when missing or stale.
#[cfg(windows)]
pub fn ensure_identity(aumid: &str, display_name: &str, icon: &Path) -> io::Result<EnsureOutcome> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let icon = icon.to_string_lossy().into_owned();
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (parent, _) = hkcu.create_subkey(AUMID_PARENT_SUBKEY)?;
    if let Ok(existing) = parent.open_subkey(aumid) {
        let current = |name: &str, want: &str| {
            existing.get_value::<String, _>(name).ok().as_deref() == Some(want)
        };
        if current(DISPLAY_NAME_VAL, display_name) && current(ICON_URI_VAL, &icon) {
            return Ok(EnsureOutcome::AlreadyCurrent);
        }
    }
    let (key, _) = parent.create_subkey(aumid)?;
    key.set_value(DISPLAY_NAME_VAL, &display_name)?;
    key.set_value(ICON_URI_VAL, &icon)?;
    Ok(EnsureOutcome::Registered)
}

/// Remove the registration for `aumid`. Idempotent: `Ok(false)` when absent.
#[cfg(windows)]
pub fn remove_identity(aumid: &str) -> io::Result<bool> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let parent = match hkcu.open_subkey(AUMID_PARENT_SUBKEY) {
        Ok(parent) => parent,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    match parent.delete_subkey_all(aumid) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Show a state toast under `aumid`. Blocking; call off the UI thread.
#[cfg(windows)]
pub fn show(aumid: &str, title: &str, body: &str) -> Result<(), String> {
    use windows::core::HSTRING;
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};

    let show = || -> windows::core::Result<()> {
        let doc = XmlDocument::new()?;
        doc.LoadXml(&HSTRING::from(toast_xml(title, body)))?;
        let toast = ToastNotification::CreateToastNotification(&doc)?;
        toast.SetTag(&HSTRING::from(STATE_TAG))?;
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(aumid))?.Show(&toast)
    };
    show().map_err(|error| error.to_string())
}

// Off-Windows honest no-op stubs so the platform crate compiles on Linux dev hosts.
#[cfg(not(windows))]
pub fn ensure_identity(
    _aumid: &str,
    _display_name: &str,
    _icon: &Path,
) -> io::Result<EnsureOutcome> {
    Ok(EnsureOutcome::AlreadyCurrent)
}

#[cfg(not(windows))]
pub fn remove_identity(_aumid: &str) -> io::Result<bool> {
    Ok(false)
}

#[cfg(not(windows))]
pub fn show(_aumid: &str, _title: &str, _body: &str) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toast_text_is_escaped_into_the_document() {
        let xml = toast_xml("a & b", "<c> \"d\" 'e'");
        assert!(xml.contains("<text>a &amp; b</text>"));
        assert!(xml.contains("<text>&lt;c&gt; &quot;d&quot; &apos;e&apos;</text>"));
    }
}
