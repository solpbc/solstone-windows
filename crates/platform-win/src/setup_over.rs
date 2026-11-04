// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Keep the owner's state when Setup runs over an existing install.
//!
//! The owner's state lives in `%LocalAppData%\Solstone`, which is also the folder
//! Velopack installs into. Setup, unlike the in-app updater, replaces that whole
//! folder: it renames it to `Solstone.<16 alphanumerics>`, installs fresh, runs the
//! new version's install hook, and only then deletes the renamed folder. The hook
//! can't cancel anything, but it runs inside that window.
//!
//! So the hook first renames that folder to `Solstone.kept.<same suffix>`, where
//! Setup's delete can't reach it, and only then carries the state back entry by
//! entry. A rename costs the same however large the segments are, which keeps it
//! inside the hook's 30-second limit. Anything it can't carry, or anything left when
//! the hook dies, stays in the kept folder and the next launch finishes the job.
//! State is lost only if that first rename keeps failing and an entry can't be moved
//! either, or if Setup itself fails after the hook and rolls back.
//!
//! This runs from the new version's hook, so it protects an upgrade from any older
//! version. A state-only folder (a portable install keeps its state here too) is
//! renamed the same way and has no Velopack files in it.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime};

/// Together these stay well inside the 30 seconds Setup allows the hook.
const KEEP_RETRY_FOR: Duration = Duration::from_secs(5);
const RETRY_FOR: Duration = Duration::from_secs(20);
const RETRY_EVERY: Duration = Duration::from_millis(250);
const KEPT: &str = "kept";

/// What one restore did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Restored {
    /// Entries now back in the data root.
    pub moved: usize,
    /// Entries that could not be moved, left in the kept folder for the next launch.
    pub failed: Vec<(String, String)>,
}

/// Carry the owner's state from Setup's rollback folder into `root`. With no
/// rollback folder this was a fresh install and there is nothing to do.
pub fn restore_state_from_rollback(root: &Path) -> Restored {
    restore_within(root, RETRY_FOR)
}

fn restore_within(root: &Path, retry_for: Duration) -> Restored {
    match newest_rollback_with_state(root) {
        Some(rollback) => {
            let from = keep_out_of_setups_reach(&rollback, retry_for.min(KEEP_RETRY_FOR));
            finish(&from, root, retry_for)
        }
        None => Restored::default(),
    }
}

/// Finish a restore that a hook couldn't: move what's still in a kept folder back
/// into `root`. Runs on every launch, before anything reads the state.
pub fn finish_interrupted_restore(root: &Path) -> Restored {
    let mut restored = Restored::default();
    for kept in kept_folders(root) {
        let this = finish(&kept, root, Duration::ZERO);
        restored.moved += this.moved;
        restored.failed.extend(this.failed);
    }
    restored
}

/// Uninstall removes the owner's state, and that includes a kept folder.
pub fn remove_kept_state(root: &Path) {
    for kept in kept_folders(root) {
        let _ = fs::remove_dir_all(kept);
    }
}

/// Rename the rollback folder away from the path Setup will delete, leaving an empty
/// folder there for Setup to remove. If the rename fails, the state is no worse off
/// than before: it is moved from where it is, and Setup deletes what remains.
fn keep_out_of_setups_reach(rollback: &Path, retry_for: Duration) -> PathBuf {
    let kept = rollback
        .file_name()
        .and_then(OsStr::to_str)
        .and_then(|name| name.rsplit_once('.'))
        .map(|(stem, suffix)| rollback.with_file_name(format!("{stem}.{KEPT}.{suffix}")));
    let Some(kept) = kept else {
        return rollback.to_path_buf();
    };
    // A scanner can briefly hold a file open in the folder Setup just renamed.
    let deadline = Instant::now() + retry_for;
    while fs::rename(rollback, &kept).is_err() {
        if Instant::now() >= deadline {
            return rollback.to_path_buf();
        }
        sleep(RETRY_EVERY);
    }
    let _ = fs::create_dir(rollback);
    kept
}

/// Move the state back. The folder goes only once none of it is left.
fn finish(from: &Path, root: &Path, retry_for: Duration) -> Restored {
    let restored = move_state(from, root, retry_for);
    if restored.failed.is_empty() {
        let _ = fs::remove_dir_all(from);
    }
    restored
}

fn kept_folders(root: &Path) -> Vec<PathBuf> {
    let (Some(parent), Some(name)) = (root.parent(), root.file_name().and_then(OsStr::to_str))
    else {
        return Vec::new();
    };
    let prefix = format!("{name}.{KEPT}.").to_ascii_lowercase();
    let Ok(entries) = fs::read_dir(parent) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|n| n.to_ascii_lowercase().starts_with(&prefix))
        })
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect()
}

/// Whether `exe` is the installed app in `root`, so that Setup replaced the data
/// root. An install placed elsewhere (`--installto`) leaves the data root alone.
pub fn is_installed_in(exe: &Path, root: &Path) -> bool {
    let install_root = exe.parent().and_then(Path::parent);
    match (
        install_root.and_then(|path| path.canonicalize().ok()),
        root.canonicalize().ok(),
    ) {
        (Some(install_root), Some(root)) => install_root == root,
        _ => false,
    }
}

/// The folder Setup just renamed is the most recently changed one holding state.
/// An older one, left behind when Setup failed to delete it, is passed over.
fn newest_rollback_with_state(root: &Path) -> Option<PathBuf> {
    let name = root.file_name()?.to_str()?;
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(root.parent()?).ok()?.flatten() {
        let path = entry.path();
        if !path.is_dir() || !is_rollback_name(&entry.file_name(), name) {
            continue;
        }
        if !has_state(&path) {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        if newest.as_ref().is_none_or(|(when, _)| modified > *when) {
            newest = Some((modified, path));
        }
    }
    newest.map(|(_, path)| path)
}

fn is_rollback_name(candidate: &OsStr, root_name: &str) -> bool {
    let Some(candidate) = candidate.to_str() else {
        return false;
    };
    let Some((stem, suffix)) = candidate.rsplit_once('.') else {
        return false;
    };
    stem.eq_ignore_ascii_case(root_name)
        && suffix.len() == 16
        && suffix.chars().all(|c| c.is_ascii_alphanumeric())
}

fn has_state(rollback: &Path) -> bool {
    fs::read_dir(rollback).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|entry| !is_velopack_entry(&entry.file_name()))
    })
}

/// Setup writes its own copies of these into the new root. Everything else in the
/// folder is the owner's, including anything a future version starts keeping there.
fn is_velopack_entry(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    name.eq_ignore_ascii_case("current")
        || name.eq_ignore_ascii_case("packages")
        || Path::new(name.as_ref())
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
}

fn move_state(rollback: &Path, root: &Path, retry_for: Duration) -> Restored {
    let mut restored = Restored::default();
    let mut remaining: Vec<PathBuf> = match fs::read_dir(rollback) {
        Ok(entries) => entries
            .flatten()
            .filter(|entry| !is_velopack_entry(&entry.file_name()))
            .map(|entry| entry.path())
            .collect(),
        Err(error) => {
            restored
                .failed
                .push((rollback.display().to_string(), error.to_string()));
            return restored;
        }
    };
    let deadline = Instant::now() + retry_for;
    loop {
        let mut errors = Vec::new();
        remaining.retain(|from| match move_entry(from, root) {
            Ok(moved) => {
                restored.moved += usize::from(moved);
                false
            }
            Err(error) => {
                errors.push((from.display().to_string(), error.to_string()));
                true
            }
        });
        if remaining.is_empty() {
            return restored;
        }
        if Instant::now() >= deadline {
            restored.failed = errors;
            return restored;
        }
        sleep(RETRY_EVERY);
    }
}

/// Ok(false) when the root already has this entry: what's there stays.
fn move_entry(from: &Path, root: &Path) -> io::Result<bool> {
    let Some(name) = from.file_name() else {
        return Ok(false);
    };
    let to = root.join(name);
    if to.exists() {
        return Ok(false);
    }
    fs::rename(from, to)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    const ROLLBACK: &str = "Solstone.aB3dE6gH9jK2mN5p";
    const KEPT_FOLDER: &str = "Solstone.kept.aB3dE6gH9jK2mN5p";
    const KEPT_FOLDER_SETUP: &str = "Solstone.kept.aB3dE6gH9jK2mN5p/setup.json";
    static NEXT: AtomicU32 = AtomicU32::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "setup-over-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(dir.join("Solstone")).unwrap();
            Self(dir)
        }

        fn root(&self) -> PathBuf {
            self.0.join("Solstone")
        }

        fn write(&self, relative: &str, text: &str) {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }

        fn read(&self, relative: &str) -> Option<String> {
            fs::read_to_string(self.0.join(relative)).ok()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn installed_prior(scratch: &Scratch, rollback: &str) {
        scratch.write(&format!("{rollback}/current/app.exe"), "old app");
        scratch.write(&format!("{rollback}/packages/old.nupkg"), "old package");
        scratch.write(&format!("{rollback}/Update.exe"), "old updater");
        scratch.write(&format!("{rollback}/sol.exe"), "stub from an older name");
    }

    fn fresh_install(scratch: &Scratch) {
        scratch.write("Solstone/current/app.exe", "new app");
        scratch.write("Solstone/packages/new.nupkg", "new package");
        scratch.write("Solstone/Update.exe", "new updater");
    }

    #[test]
    fn setup_over_an_install_keeps_the_owners_state_and_the_new_app() {
        let scratch = Scratch::new();
        installed_prior(&scratch, ROLLBACK);
        fresh_install(&scratch);
        scratch.write(&format!("{ROLLBACK}/pairing.json"), "pairing");
        scratch.write(&format!("{ROLLBACK}/setup.json"), "setup");
        scratch.write(
            &format!("{ROLLBACK}/segments/015000_300/screen.mp4"),
            "held",
        );
        scratch.write(&format!("{ROLLBACK}/logs/solstone.log"), "log");

        let restored = restore_state_from_rollback(&scratch.root());

        assert_eq!(restored.moved, 4);
        assert!(restored.failed.is_empty());
        assert_eq!(scratch.read("Solstone/pairing.json").unwrap(), "pairing");
        assert_eq!(scratch.read("Solstone/setup.json").unwrap(), "setup");
        assert_eq!(
            scratch
                .read("Solstone/segments/015000_300/screen.mp4")
                .unwrap(),
            "held"
        );
        assert_eq!(scratch.read("Solstone/current/app.exe").unwrap(), "new app");
        assert_eq!(scratch.read("Solstone/Update.exe").unwrap(), "new updater");
        assert!(scratch.read("Solstone/packages/old.nupkg").is_none());
        assert!(scratch.read("Solstone/sol.exe").is_none());
        assert!(!scratch.0.join(KEPT_FOLDER).exists());
    }

    #[test]
    fn a_state_only_folder_from_a_portable_install_comes_back_too() {
        let scratch = Scratch::new();
        fresh_install(&scratch);
        scratch.write(&format!("{ROLLBACK}/pairing.json"), "pairing");

        assert_eq!(restore_state_from_rollback(&scratch.root()).moved, 1);
        assert_eq!(scratch.read("Solstone/pairing.json").unwrap(), "pairing");
    }

    #[test]
    fn a_fresh_install_has_nothing_to_restore() {
        let scratch = Scratch::new();
        fresh_install(&scratch);
        scratch.write("Solstone.old/pairing.json", "not a rollback folder");
        scratch.write("Solstone.aB3dE6gH9jK2mN5pX/pairing.json", "wrong length");
        scratch.write("Other.aB3dE6gH9jK2mN5p/pairing.json", "another app");

        assert_eq!(
            restore_state_from_rollback(&scratch.root()),
            Restored::default()
        );
        assert!(scratch.read("Solstone/pairing.json").is_none());
    }

    #[test]
    fn what_the_new_install_already_has_is_not_replaced() {
        let scratch = Scratch::new();
        fresh_install(&scratch);
        scratch.write("Solstone/setup.json", "already here");
        scratch.write(&format!("{ROLLBACK}/setup.json"), "from the rollback");

        let restored = restore_state_from_rollback(&scratch.root());

        assert_eq!(restored.moved, 0);
        assert!(restored.failed.is_empty());
        assert_eq!(scratch.read("Solstone/setup.json").unwrap(), "already here");
    }

    #[test]
    fn a_leftover_rollback_loses_to_the_newer_one_and_one_with_no_state_is_skipped() {
        let scratch = Scratch::new();
        fresh_install(&scratch);
        scratch.write("Solstone.olderOlderOlder1/pairing.json", "old pairing");
        scratch.write("Solstone.olderOlderOlder1/stale.json", "old state");
        sleep(Duration::from_millis(50));
        scratch.write(&format!("{ROLLBACK}/pairing.json"), "current pairing");
        installed_prior(&scratch, "Solstone.noStateNoStateNo1");

        let restored = restore_state_from_rollback(&scratch.root());

        assert_eq!(restored.moved, 1);
        assert_eq!(
            scratch.read("Solstone/pairing.json").unwrap(),
            "current pairing"
        );
        assert!(scratch.read("Solstone/stale.json").is_none());
    }

    #[test]
    fn a_failed_restore_keeps_the_state_out_of_setups_reach_and_the_next_launch_finishes_it() {
        let scratch = Scratch::new();
        scratch.write(&format!("{ROLLBACK}/setup.json"), "setup");
        scratch.write(
            &format!("{ROLLBACK}/segments/015000_300/screen.mp4"),
            "held",
        );
        // With the root missing, every move fails.
        fs::remove_dir_all(scratch.root()).unwrap();

        let restored = restore_within(&scratch.root(), Duration::ZERO);

        assert_eq!(restored.moved, 0);
        assert_eq!(restored.failed.len(), 2);
        // What Setup does next: delete the folder it renamed.
        fs::remove_dir_all(scratch.0.join(ROLLBACK)).unwrap();
        assert_eq!(scratch.read(KEPT_FOLDER_SETUP).unwrap(), "setup");

        fresh_install(&scratch);
        let finished = finish_interrupted_restore(&scratch.root());

        assert_eq!(finished.moved, 2);
        assert!(finished.failed.is_empty());
        assert_eq!(scratch.read("Solstone/setup.json").unwrap(), "setup");
        assert_eq!(
            scratch
                .read("Solstone/segments/015000_300/screen.mp4")
                .unwrap(),
            "held"
        );
        assert!(!scratch.0.join(KEPT_FOLDER).exists());
    }

    #[test]
    fn a_launch_with_nothing_interrupted_does_nothing() {
        let scratch = Scratch::new();
        fresh_install(&scratch);
        scratch.write("Solstone/setup.json", "setup");

        assert_eq!(
            finish_interrupted_restore(&scratch.root()),
            Restored::default()
        );
        assert_eq!(scratch.read("Solstone/setup.json").unwrap(), "setup");
    }

    #[test]
    fn uninstall_removes_a_kept_folder_and_nothing_else() {
        let scratch = Scratch::new();
        scratch.write(KEPT_FOLDER_SETUP, "setup");
        scratch.write("SolstoneJournal/config.toml", "another app");

        remove_kept_state(&scratch.root());

        assert!(!scratch.0.join(KEPT_FOLDER).exists());
        assert_eq!(
            scratch.read("SolstoneJournal/config.toml").unwrap(),
            "another app"
        );
    }

    #[test]
    fn only_the_app_installed_in_the_data_root_touches_it() {
        let scratch = Scratch::new();
        fresh_install(&scratch);
        scratch.write("Elsewhere/current/app.exe", "installed with --installto");

        assert!(is_installed_in(
            &scratch.root().join("current/app.exe"),
            &scratch.root()
        ));
        assert!(!is_installed_in(
            &scratch.0.join("Elsewhere/current/app.exe"),
            &scratch.root()
        ));
    }
}
