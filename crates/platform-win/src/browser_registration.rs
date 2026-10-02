// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Per-user native-messaging registration: manifest files under the app's
//! data directory, and an `HKEY_CURRENT_USER` key per browser whose default
//! value names the manifest, written in both registry views.
//!
//! Like the autostart login item, it is written on every launch of the copy
//! that owns it, only when missing or different, and removed by the uninstall
//! callback only where it still names this app's manifest.

use std::io;
use std::path::{Path, PathBuf};

/// One registration row to write or remove.
pub struct Row<'a> {
    pub registry_key: &'a str,
    pub manifest_file: &'a str,
    pub json: &'a str,
}

/// The directory holding the manifests.
pub fn manifests_dir() -> PathBuf {
    crate::local_data_root().join("browser")
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EnsureOutcome {
    pub written: usize,
    pub already_current: usize,
}

#[cfg(windows)]
mod imp {
    use super::*;
    use winreg::enums::{
        HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, KEY_WOW64_32KEY, KEY_WOW64_64KEY,
    };
    use winreg::RegKey;

    const VIEWS: [u32; 2] = [KEY_WOW64_64KEY, KEY_WOW64_32KEY];

    pub fn ensure(dir: &Path, rows: &[Row<'_>]) -> io::Result<EnsureOutcome> {
        std::fs::create_dir_all(dir)?;
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let mut outcome = EnsureOutcome::default();
        for row in rows {
            let manifest = dir.join(row.manifest_file);
            let mut changed = write_if_different(&manifest, row.json.as_bytes())?;
            let value = manifest.to_string_lossy().into_owned();
            for view in VIEWS {
                let (key, _) = hkcu.create_subkey_with_flags(
                    row.registry_key,
                    KEY_QUERY_VALUE | KEY_SET_VALUE | view,
                )?;
                if key.get_value::<String, _>("").ok().as_deref() != Some(value.as_str()) {
                    key.set_value("", &value)?;
                    changed = true;
                }
            }
            if changed {
                outcome.written += 1;
            } else {
                outcome.already_current += 1;
            }
        }
        Ok(outcome)
    }

    pub fn remove(dir: &Path, rows: &[Row<'_>]) -> io::Result<usize> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let mut removed = 0;
        for row in rows {
            let manifest = dir.join(row.manifest_file);
            let ours = manifest.to_string_lossy().into_owned();
            for view in VIEWS {
                let names_ours = hkcu
                    .open_subkey_with_flags(row.registry_key, KEY_QUERY_VALUE | view)
                    .and_then(|k| k.get_value::<String, _>(""))
                    .is_ok_and(|v| v == ours);
                if names_ours {
                    hkcu.delete_subkey_with_flags(row.registry_key, view)?;
                    removed += 1;
                }
            }
            match std::fs::remove_file(&manifest) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(removed)
    }
}

#[cfg(windows)]
pub use imp::{ensure, remove};

/// Off-Windows stub: nothing to register.
#[cfg(not(windows))]
pub fn ensure(_dir: &Path, _rows: &[Row<'_>]) -> io::Result<EnsureOutcome> {
    Ok(EnsureOutcome::default())
}

/// Off-Windows stub: nothing to remove.
#[cfg(not(windows))]
pub fn remove(_dir: &Path, _rows: &[Row<'_>]) -> io::Result<usize> {
    Ok(0)
}

/// Atomically replace `path` with `bytes` unless it already holds them.
#[cfg_attr(not(windows), allow(dead_code))]
fn write_if_different(path: &Path, bytes: &[u8]) -> io::Result<bool> {
    if std::fs::read(path).is_ok_and(|current| current == bytes) {
        return Ok(false);
    }
    let tmp = path.with_extension("json.partial");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(true)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_WOW64_32KEY, KEY_WOW64_64KEY};
    use winreg::RegKey;

    #[test]
    fn ensure_writes_both_views_once_and_remove_takes_only_ours() {
        let id = std::process::id();
        let key =
            format!(r"Software\SolstoneBrowserRegistrationTest{id}\NativeMessagingHosts\app.test");
        let dir = std::env::temp_dir().join(format!("solstone-reg-test-{id}"));
        let rows = [Row {
            registry_key: &key,
            manifest_file: "app.test.chrome.json",
            json: "{\"name\":\"app.test\"}\n",
        }];
        let first = ensure(&dir, &rows).unwrap();
        assert_eq!(first.written, 1);
        let again = ensure(&dir, &rows).unwrap();
        assert_eq!(again.already_current, 1);
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let manifest = dir
            .join("app.test.chrome.json")
            .to_string_lossy()
            .into_owned();
        for view in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
            let k = hkcu
                .open_subkey_with_flags(&key, KEY_QUERY_VALUE | view)
                .unwrap();
            assert_eq!(k.get_value::<String, _>("").unwrap(), manifest);
        }
        assert_eq!(std::fs::read_to_string(&manifest).unwrap(), rows[0].json);
        // HKCU\Software is one key in both views on current Windows, so one
        // delete may clear both.
        assert!(remove(&dir, &rows).unwrap() >= 1);
        assert!(hkcu.open_subkey(&key).is_err());
        assert!(!std::path::Path::new(&manifest).exists());
        let _ = hkcu.delete_subkey_all(format!(r"Software\SolstoneBrowserRegistrationTest{id}"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
