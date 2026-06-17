// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Per-user autostart (relaunch-at-login) registration.
//!
//! The tray-resident observer is meant to come back after a reboot. This module
//! owns the one mechanism that makes that happen: a single named value under the
//! per-user `HKCU\…\CurrentVersion\Run` key. It is deliberately per-user — no
//! admin, no machine-wide `HKLM` entry, no scheduled task — so the login item
//! lands the observer in the same interactive Session 1 it already runs in.
//!
//! Two properties matter:
//!
//! - **Idempotent.** A single named value means re-registering overwrites in
//!   place — there is never a second, duplicate entry across updates. Removal on
//!   uninstall deletes that one value.
//! - **Ensured on launch, not on a one-shot signal.** Registration is meant to be
//!   called on every normal startup, guarded by a read so it only writes when the
//!   entry is missing or stale. Tying registration to a single post-install
//!   callback leaves the observer silently unregistered whenever the first launch
//!   after install isn't the installer-spawned one; ensuring on launch is
//!   self-healing and also re-points the entry if the executable path moves.
//!
//! The executable path is quoted so a profile path containing spaces (e.g.
//! `C:\Users\Jane Doe\…`) is parsed by the shell as a single token.

use std::io;
use std::path::Path;

/// The per-user autostart key. `HKCU` only — never `HKLM`, never elevated.
#[cfg_attr(not(windows), allow(dead_code))]
const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// The `Run` value name for the observer's login item. Stable across versions so
/// re-registration overwrites the same entry (one item, no duplicates).
pub const LOGIN_ITEM_NAME: &str = "Solstone";

/// Result of [`ensure_login_item`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnsureOutcome {
    /// The entry was absent or stale and has now been written.
    Registered,
    /// The entry already matched the desired command; nothing changed.
    AlreadyCurrent,
}

/// The `Run` command string for an executable launched with optional args. The
/// executable path is quoted; args are appended verbatim (the observer passes
/// none today).
#[cfg_attr(not(windows), allow(dead_code))]
fn run_command(exe: &Path, args: &[&str]) -> String {
    let mut command = format!("\"{}\"", exe.display());
    for arg in args {
        command.push(' ');
        command.push_str(arg);
    }
    command
}

/// Ensure the per-user login item points at `exe` (with `args`). Reads first and
/// only writes when missing or stale, so calling it on every launch is cheap and
/// produces no duplicate entries.
#[cfg(windows)]
pub fn ensure_login_item(name: &str, exe: &Path, args: &[&str]) -> io::Result<EnsureOutcome> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let desired = run_command(exe, args);
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    // The `Run` key exists on every Windows install; `create_subkey` opens it
    // (and would create it on the pathological install that lacks it).
    let (run, _) = hkcu.create_subkey(RUN_SUBKEY)?;
    let current: Option<String> = run.get_value(name).ok();
    if current.as_deref() == Some(desired.as_str()) {
        return Ok(EnsureOutcome::AlreadyCurrent);
    }
    run.set_value(name, &desired)?;
    Ok(EnsureOutcome::Registered)
}

/// Read the current login-item command, or `None` if no entry exists.
#[cfg(windows)]
pub fn login_item_command(name: &str) -> io::Result<Option<String>> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run = match hkcu.open_subkey(RUN_SUBKEY) {
        Ok(run) => run,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    match run.get_value::<String, _>(name) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Remove the per-user login item. Idempotent: returns `Ok(false)` when there was
/// nothing to remove. Called from the Velopack uninstall hook so no stale `Run`
/// entry survives the app's removal.
#[cfg(windows)]
pub fn remove_login_item(name: &str) -> io::Result<bool> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run = match hkcu.open_subkey_with_flags(RUN_SUBKEY, KEY_SET_VALUE) {
        Ok(run) => run,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    match run.delete_value(name) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

// Off-Windows honest no-op stubs so the platform crate still compiles on the
// Linux dev host (it is excluded from the local test gate; the real registry
// behavior is exercised on the Windows build box).
#[cfg(not(windows))]
pub fn ensure_login_item(_name: &str, _exe: &Path, _args: &[&str]) -> io::Result<EnsureOutcome> {
    Ok(EnsureOutcome::AlreadyCurrent)
}

#[cfg(not(windows))]
pub fn login_item_command(_name: &str) -> io::Result<Option<String>> {
    Ok(None)
}

#[cfg(not(windows))]
pub fn remove_login_item(_name: &str) -> io::Result<bool> {
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn run_command_quotes_the_executable_path() {
        let command = run_command(
            &PathBuf::from(
                r"C:\Users\Jane Doe\AppData\Local\Solstone\current\solstone-windows-app.exe",
            ),
            &[],
        );
        assert_eq!(
            command,
            r#""C:\Users\Jane Doe\AppData\Local\Solstone\current\solstone-windows-app.exe""#
        );
    }

    #[test]
    fn run_command_appends_args_after_the_quoted_path() {
        let command = run_command(&PathBuf::from(r"C:\app.exe"), &["--from-autostart"]);
        assert_eq!(command, r#""C:\app.exe" --from-autostart"#);
    }

    // The HKCU round-trip runs only on Windows (the build box): write, read back,
    // confirm idempotency, then remove and confirm removal is idempotent. Uses a
    // throwaway value name so it never touches the real `Solstone` entry, and
    // cleans up after itself.
    #[cfg(windows)]
    #[test]
    fn ensure_read_remove_round_trip() {
        let name = format!("SolstoneAutostartTest-{}", std::process::id());
        let exe =
            PathBuf::from(r"C:\Users\test\AppData\Local\Solstone\current\solstone-windows-app.exe");

        // Clean slate.
        let _ = remove_login_item(&name);
        assert_eq!(login_item_command(&name).unwrap(), None);

        // First ensure writes the entry.
        assert_eq!(
            ensure_login_item(&name, &exe, &[]).unwrap(),
            EnsureOutcome::Registered
        );
        let expected = run_command(&exe, &[]);
        assert_eq!(
            login_item_command(&name).unwrap().as_deref(),
            Some(expected.as_str())
        );

        // Second ensure is a no-op (idempotent — no duplicate, no rewrite).
        assert_eq!(
            ensure_login_item(&name, &exe, &[]).unwrap(),
            EnsureOutcome::AlreadyCurrent
        );

        // A changed path re-registers in place.
        let exe2 = PathBuf::from(
            r"C:\Users\test\AppData\Local\Solstone\current\solstone-windows-app.exe ",
        );
        assert_eq!(
            ensure_login_item(&name, &exe2, &[]).unwrap(),
            EnsureOutcome::Registered
        );

        // Remove deletes it; a second remove is a clean no-op.
        assert_eq!(remove_login_item(&name).unwrap(), true);
        assert_eq!(login_item_command(&name).unwrap(), None);
        assert_eq!(remove_login_item(&name).unwrap(), false);
    }
}
