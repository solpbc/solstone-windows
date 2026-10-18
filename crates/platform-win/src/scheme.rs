// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Per-user URL scheme registration (`solstone://`).
//!
//! Owns reading, writing, and removing the URL protocol handler under
//! `HKCU\Software\Classes\solstone`. Deliberately per-user — no elevation,
//! no machine-wide HKLM entries.
//!
//! Caller-directed: this module contains no decision logic. The pure decisions
//! live in `observer_model::scheme`.

use std::io;

pub use crate::autostart::EnsureOutcome;

#[cfg_attr(not(windows), allow(dead_code))]
const CLASSES_SUBKEY: &str = r"Software\Classes";
#[cfg_attr(not(windows), allow(dead_code))]
const SCHEME_KEY_NAME: &str = "solstone";
#[cfg_attr(not(windows), allow(dead_code))]
const COMMAND_SUBKEY: &str = r"Software\Classes\solstone\shell\open\command";
#[cfg_attr(not(windows), allow(dead_code))]
const URL_PROTOCOL_VAL: &str = "URL Protocol";
#[cfg_attr(not(windows), allow(dead_code))]
const DEFAULT_DESC: &str = "URL:solstone";

/// Ensure the per-user scheme handler points at `desired_command`. Reads first and
/// only writes when missing or stale.
#[cfg(windows)]
pub fn ensure_scheme_handler(desired_command: &str) -> io::Result<EnsureOutcome> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (classes, _) = hkcu.create_subkey(CLASSES_SUBKEY)?;

    if let Ok(cmd_key) = classes.open_subkey(r"solstone\shell\open\command") {
        if let Ok(current) = cmd_key.get_value::<String, _>("") {
            if current == desired_command {
                return Ok(EnsureOutcome::AlreadyCurrent);
            }
        }
    }

    let (solstone, _) = classes.create_subkey(SCHEME_KEY_NAME)?;
    solstone.set_value("", &DEFAULT_DESC)?;
    solstone.set_value(URL_PROTOCOL_VAL, &"")?;
    let (cmd, _) = solstone.create_subkey(r"shell\open\command")?;
    cmd.set_value("", &desired_command)?;
    Ok(EnsureOutcome::Registered)
}

/// Read the current scheme open command, or `None` if no entry exists.
#[cfg(windows)]
pub fn read_scheme_command() -> io::Result<Option<String>> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let cmd = match hkcu.open_subkey(COMMAND_SUBKEY) {
        Ok(cmd) => cmd,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    match cmd.get_value::<String, _>("") {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Remove the per-user scheme handler subkey tree. Idempotent: returns `Ok(false)`
/// when absent.
#[cfg(windows)]
pub fn remove_scheme_handler() -> io::Result<bool> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let classes = match hkcu.open_subkey(CLASSES_SUBKEY) {
        Ok(classes) => classes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    match classes.delete_subkey_all(SCHEME_KEY_NAME) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

// Off-Windows honest no-op stubs so the platform crate compiles on Linux dev hosts.
#[cfg(not(windows))]
pub fn ensure_scheme_handler(_desired_command: &str) -> io::Result<EnsureOutcome> {
    Ok(EnsureOutcome::AlreadyCurrent)
}

#[cfg(not(windows))]
pub fn read_scheme_command() -> io::Result<Option<String>> {
    Ok(None)
}

#[cfg(not(windows))]
pub fn remove_scheme_handler() -> io::Result<bool> {
    Ok(false)
}
