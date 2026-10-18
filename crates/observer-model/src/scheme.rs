// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! URL scheme registration and command quoting decisions.
//!
//! Owns the pure formatting of Windows shell open command strings and the
//! decision logic for registering or removing the URL scheme handler in HKCU.

use std::path::Path;

use crate::setup::CopyClass;

/// Decision on whether to ensure the scheme handler registration in the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemeLaunchDecision {
    Ensure,
    Leave,
}

/// Extract the first double-quoted token from a command string.
pub fn extract_first_quoted_token(s: &str) -> Option<&str> {
    let trimmed = s.trim_start();
    if !trimmed.starts_with('"') {
        return None;
    }
    let rest = &trimmed[1..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// Format the shell open command string for a URL scheme handler.
///
/// Returns `None` unless `arg == "%1"` and `exe` is a valid, non-empty UTF-8
/// path without double quotes or control characters (NUL, CR, LF).
pub fn scheme_open_command(exe: &Path, arg: &str) -> Option<String> {
    if arg != "%1" {
        return None;
    }
    let s = exe.to_str()?;
    if s.is_empty() || s.contains('"') || s.contains('\0') || s.contains('\r') || s.contains('\n') {
        return None;
    }
    Some(format!("\"{}\" \"%1\"", s))
}

/// Pure decision on whether the scheme handler should be registered.
pub fn scheme_launch_decision(
    copy: CopyClass,
    current: Option<&str>,
    exe: &Path,
) -> SchemeLaunchDecision {
    match copy {
        CopyClass::Dev | CopyClass::Portable => SchemeLaunchDecision::Leave,
        CopyClass::Installed => match current {
            None => SchemeLaunchDecision::Ensure,
            Some(curr) => {
                if let Some(exe_str) = exe.to_str() {
                    if let Some(first) = extract_first_quoted_token(curr) {
                        if first.eq_ignore_ascii_case(exe_str) {
                            return SchemeLaunchDecision::Ensure;
                        }
                    }
                }
                SchemeLaunchDecision::Leave
            }
        },
    }
}

/// Pure decision on whether uninstalling should remove the scheme handler.
///
/// Returns true only when the first quoted token in `current` matches `exe`.
pub fn scheme_uninstall_should_remove(current: Option<&str>, exe: &Path) -> bool {
    let Some(curr) = current else {
        return false;
    };
    let Some(exe_str) = exe.to_str() else {
        return false;
    };
    if let Some(first) = extract_first_quoted_token(curr) {
        first.eq_ignore_ascii_case(exe_str)
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn open_command_quoting() {
        let p1 = PathBuf::from(r"C:\Program Files\Solstone\solstone.exe");
        assert_eq!(
            scheme_open_command(&p1, "%1"),
            Some(r#""C:\Program Files\Solstone\solstone.exe" "%1""#.to_string())
        );

        let p2 = PathBuf::from(r"C:\Users\José\AppData\Local\Solstone\app.exe");
        assert_eq!(
            scheme_open_command(&p2, "%1"),
            Some(r#""C:\Users\José\AppData\Local\Solstone\app.exe" "%1""#.to_string())
        );

        assert_eq!(scheme_open_command(&p1, "%2"), None);
        assert_eq!(scheme_open_command(Path::new(""), "%1"), None);
        assert_eq!(
            scheme_open_command(Path::new(r#"C:\bad"path\app.exe"#), "%1"),
            None
        );
        assert_eq!(
            scheme_open_command(Path::new("C:\\bad\npath\\app.exe"), "%1"),
            None
        );
        assert_eq!(
            scheme_open_command(Path::new("C:\\bad\rpath\\app.exe"), "%1"),
            None
        );
    }

    #[test]
    fn scheme_launch_matrix() {
        let exe = PathBuf::from(r"C:\Users\alice\AppData\Local\Solstone\solstone.exe");
        let foreign = r#""C:\Other\app.exe" "%1""#;
        let self_cmd = r#""C:\Users\alice\AppData\Local\Solstone\solstone.exe" "%1""#;
        let self_cmd_diff_case = r#""C:\users\ALICE\appdata\local\solstone\SOLSTONE.EXE" "%1""#;

        // Installed absent -> Ensure
        assert_eq!(
            scheme_launch_decision(CopyClass::Installed, None, &exe),
            SchemeLaunchDecision::Ensure
        );
        // Installed self -> Ensure
        assert_eq!(
            scheme_launch_decision(CopyClass::Installed, Some(self_cmd), &exe),
            SchemeLaunchDecision::Ensure
        );
        assert_eq!(
            scheme_launch_decision(CopyClass::Installed, Some(self_cmd_diff_case), &exe),
            SchemeLaunchDecision::Ensure
        );
        // Installed foreign -> Leave
        assert_eq!(
            scheme_launch_decision(CopyClass::Installed, Some(foreign), &exe),
            SchemeLaunchDecision::Leave
        );

        // Portable -> Leave
        assert_eq!(
            scheme_launch_decision(CopyClass::Portable, None, &exe),
            SchemeLaunchDecision::Leave
        );
        assert_eq!(
            scheme_launch_decision(CopyClass::Portable, Some(self_cmd), &exe),
            SchemeLaunchDecision::Leave
        );

        // Dev -> Leave
        assert_eq!(
            scheme_launch_decision(CopyClass::Dev, None, &exe),
            SchemeLaunchDecision::Leave
        );
        assert_eq!(
            scheme_launch_decision(CopyClass::Dev, Some(self_cmd), &exe),
            SchemeLaunchDecision::Leave
        );
    }

    #[test]
    fn uninstall_owned_vs_foreign_vs_absent() {
        let exe = PathBuf::from(r"C:\Solstone\app.exe");
        let owned = r#""C:\Solstone\app.exe" "%1""#;
        let foreign = r#""C:\Other\app.exe" "%1""#;

        assert!(scheme_uninstall_should_remove(Some(owned), &exe));
        assert!(!scheme_uninstall_should_remove(Some(foreign), &exe));
        assert!(!scheme_uninstall_should_remove(None, &exe));
        assert!(!scheme_uninstall_should_remove(Some("malformed"), &exe));
    }
}
