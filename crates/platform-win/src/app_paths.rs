//! Where Windows says an installed program lives: its `App Paths` registration.
//!
//! Launching a program by bare name (`chrome.exe`) through the shell shows
//! Windows' own "cannot find" dialog when it isn't installed. Resolving the
//! registration first lets a caller skip a program that isn't there.

use std::path::PathBuf;

/// The registry path, under a root key, that holds every `App Paths` entry.
const APP_PATHS: &str = r"Software\Microsoft\Windows\CurrentVersion\App Paths";

/// The full path Windows registers for `exe` (for example `chrome.exe`):
/// per-user first, then per-machine (native, then 32-bit view). `None` when no
/// registration names a file that exists.
pub fn registered_executable(exe: &str) -> Option<PathBuf> {
    registered_under(APP_PATHS, exe)
}

#[cfg(windows)]
fn registered_under(base: &str, exe: &str) -> Option<PathBuf> {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY};
    use winreg::RegKey;

    let sub = format!(r"{base}\{exe}");
    let candidates = [
        (HKEY_CURRENT_USER, KEY_READ),
        (HKEY_LOCAL_MACHINE, KEY_READ),
        (HKEY_LOCAL_MACHINE, KEY_READ | KEY_WOW64_32KEY),
    ];
    for (root, flags) in candidates {
        let Ok(key) = RegKey::predef(root).open_subkey_with_flags(&sub, flags) else {
            continue;
        };
        let Ok(value) = key.get_value::<String, _>("") else {
            continue;
        };
        let path = PathBuf::from(value.trim().trim_matches('"'));
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

#[cfg(not(windows))]
fn registered_under(_base: &str, _exe: &str) -> Option<PathBuf> {
    None
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    #[test]
    fn a_registration_naming_an_existing_file_resolves_and_others_do_not() {
        let base = format!(r"Software\solstone-test-app-paths-{}", std::process::id());
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let present = std::env::current_exe().unwrap();
        let (key, _) = hkcu.create_subkey(format!(r"{base}\present.exe")).unwrap();
        key.set_value("", &format!("\"{}\"", present.display()))
            .unwrap();
        let (key, _) = hkcu.create_subkey(format!(r"{base}\gone.exe")).unwrap();
        key.set_value("", &r"C:\nowhere\gone.exe").unwrap();

        let found = registered_under(&base, "present.exe");
        let gone = registered_under(&base, "gone.exe");
        let unregistered = registered_under(&base, "never.exe");
        hkcu.delete_subkey_all(&base).unwrap();

        assert_eq!(found, Some(present));
        assert_eq!(gone, None);
        assert_eq!(unregistered, None);
    }
}
