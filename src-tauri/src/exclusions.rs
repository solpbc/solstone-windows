// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Capture-exclusion config controller (shell side).
//!
//! Owns the persisted `exclusions.json` and the shared
//! `Arc<RwLock<ExclusionRules>>` handed to the WGC screen source. The owner edits
//! rules over IPC; `set` writes the shared handle — so the next captured frame
//! sees the change live, no restart — and persists to disk so it survives a
//! restart. The policy + matching + redaction live in the pure
//! `observer-exclusion` crate; this is only I/O + sharing.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use observer_exclusion::ExclusionRules;
use serde::Serialize;

/// Agent-native diagnostic for `--dump-windows`: the windows the exclusion
/// enumerator admits on the primary monitor and the content-free per-frame
/// verdict. Excluded or uncertain windows and unreadable policies disclose no
/// observed window content. Lets an operator confirm
/// exclusion behavior headlessly (run it in the interactive session) without
/// inspecting a captured segment. Read-only: it does not touch `exclusions.json`.
pub fn dump_windows_json() -> String {
    let (rules, available) =
        match load_rules(&platform_win::local_data_root().join("exclusions.json")) {
            RulesLoadOutcome::Fresh(rules) | RulesLoadOutcome::Available(rules) => (rules, true),
            RulesLoadOutcome::Unavailable => (ExclusionRules::default(), false),
        };
    let windows = if available {
        capture_wgc::dump_primary_monitor_windows()
    } else {
        Vec::new()
    };
    let decision = observer_exclusion::evaluate(&rules, &windows);
    let windows: Vec<_> = windows
        .into_iter()
        .filter(|window| {
            !window.identity_uncertain
                && !observer_exclusion::foreground_excluded(&rules, &window.exe_name, &window.title)
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::json!({
        "rules_available": available,
        "windows": windows,
        "decision": decision,
    }))
    .unwrap_or_else(|e| format!("{{\"error\":\"failed to serialize dump: {e}\"}}"))
}

/// Cheaply-clonable handle shared by the IPC commands and the screen source.
#[derive(Clone)]
pub struct ExclusionController {
    rules: Arc<RwLock<ExclusionRules>>,
    path: Arc<PathBuf>,
    available: Arc<std::sync::atomic::AtomicBool>,
    writes: Arc<Mutex<()>>,
}

/// IPC outcome of a `set_exclusions`: whether the new rules were durably
/// written to disk. The rules take effect in memory regardless; `persisted`
/// is false only when the disk write (or serialize) failed.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct SetExclusionsOutcome {
    pub persisted: bool,
}

impl ExclusionController {
    /// Load rules from `path` (defaulting when absent, but flagging unavailable
    /// when corrupt or unreadable), normalize them, hold them behind a shared lock,
    /// and seed the file on disk only on a fresh install.
    pub fn new(path: PathBuf) -> Self {
        let (loaded, is_avail, should_seed) = match load_rules(&path) {
            RulesLoadOutcome::Fresh(defaults) => (defaults, true, true),
            RulesLoadOutcome::Available(rules) => (rules, true, false),
            RulesLoadOutcome::Unavailable => (ExclusionRules::default().normalized(), false, false),
        };

        let available = Arc::new(std::sync::atomic::AtomicBool::new(is_avail));
        let ctrl = Self {
            rules: Arc::new(RwLock::new(loaded)),
            path: Arc::new(path),
            available,
            writes: Arc::new(Mutex::new(())),
        };
        if should_seed {
            let _ = ctrl.persist(&ctrl.get());
        }
        ctrl
    }

    /// The shared handle the WGC source reads each frame.
    pub fn rules_handle(&self) -> Arc<RwLock<ExclusionRules>> {
        Arc::clone(&self.rules)
    }

    /// Availability flag: false when exclusions file exists but cannot be read or parsed.
    pub fn availability_handle(&self) -> Arc<std::sync::atomic::AtomicBool> {
        Arc::clone(&self.available)
    }

    #[cfg(test)]
    pub fn is_available(&self) -> bool {
        self.available.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The current rules (for the Settings initial render).
    pub fn get(&self) -> ExclusionRules {
        self.rules.read().map(|r| r.clone()).unwrap_or_default()
    }

    /// Apply owner rules immediately and report whether they were also saved.
    /// An unreadable saved policy stays unavailable until a valid repair is saved.
    pub fn set(&self, rules: ExclusionRules) -> SetExclusionsOutcome {
        let Ok(_write) = self.writes.lock() else {
            self.available
                .store(false, std::sync::atomic::Ordering::SeqCst);
            return SetExclusionsOutcome { persisted: false };
        };
        let normalized = rules.normalized();
        if let Ok(mut guard) = self.rules.write() {
            *guard = normalized.clone();
        } else {
            self.available
                .store(false, std::sync::atomic::Ordering::SeqCst);
            return SetExclusionsOutcome { persisted: false };
        }
        let persisted = self.persist(&normalized);
        if persisted {
            self.available
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        SetExclusionsOutcome { persisted }
    }

    fn persist(&self, rules: &ExclusionRules) -> bool {
        match persist_rules(self.path.as_path(), rules) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(
                    target: "config",
                    area = "exclusions",
                    path = %self.path.display(),
                    error = %e,
                    "persist failed"
                );
                false
            }
        }
    }
}

/// Outcome of loading exclusion rules from disk.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RulesLoadOutcome {
    /// File was absent: fresh-install default rules, which may be seeded to disk.
    Fresh(ExclusionRules),
    /// File was present and successfully parsed.
    Available(ExclusionRules),
    /// File exists but cannot be read or parsed. Do not overwrite or treat as empty.
    Unavailable,
}

/// Load exclusion rules from `path`.
/// - Missing file: returns `Fresh` with default rules.
/// - File exists and is valid JSON matching `ExclusionRules`: returns `Available` with normalized rules.
/// - File exists but cannot be read or contains invalid JSON: returns `Unavailable`.
fn load_rules(path: &std::path::Path) -> RulesLoadOutcome {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<ExclusionRules>(&text) {
            Ok(rules) => RulesLoadOutcome::Available(rules.normalized()),
            Err(_) => RulesLoadOutcome::Unavailable,
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            RulesLoadOutcome::Fresh(ExclusionRules::default().normalized())
        }
        Err(_) => RulesLoadOutcome::Unavailable,
    }
}

/// Persist exclusion rules to `path` via a temporary sibling file and atomic rename.
/// If writing or renaming fails, the original file at `path` is preserved intact.
fn persist_rules(path: &std::path::Path, rules: &ExclusionRules) -> std::io::Result<()> {
    use std::io::Write;
    let normalized = rules.normalized();
    let text = serde_json::to_string_pretty(&normalized)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file_stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("exclusions");
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("json");
    let tmp_name = format!("{file_stem}.tmp.{}.{ext}", std::process::id());
    let tmp_path = path.with_file_name(tmp_name);
    let mut file = std::fs::File::create(&tmp_path)?;
    if let Err(error) = file
        .write_all(text.as_bytes())
        .and_then(|_| file.sync_all())
    {
        drop(file);
        let _ = std::fs::remove_file(&tmp_path);
        return Err(error);
    }
    drop(file);
    if let Err(err) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn unique_root(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        std::env::temp_dir().join(format!(
            "solstone-exclusions-{name}-{}-{stamp}",
            std::process::id()
        ))
    }

    #[test]
    fn persist_failure_reports_unpersisted() {
        let root = unique_root("persist-failure");
        std::fs::create_dir_all(&root).unwrap();
        let ctrl = ExclusionController::new(root.clone());

        let outcome = ctrl.set(ExclusionRules::default());

        assert!(!outcome.persisted);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn persist_success_reports_persisted() {
        let root = unique_root("persist-success");
        let path = root.join("exclusions.json");
        let ctrl = ExclusionController::new(path.clone());

        let outcome = ctrl.set(ExclusionRules::default());

        assert!(outcome.persisted);
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn corrupt_exclusions_starts_unavailable_and_repairs_on_set() {
        let root = unique_root("corrupt-repair");
        let path = root.join("exclusions.json");
        std::fs::create_dir_all(&root).unwrap();
        let corrupt = b"not json";
        std::fs::write(&path, corrupt).unwrap();

        let ctrl = ExclusionController::new(path.clone());
        assert!(!ctrl.is_available());
        // Corrupt file was not overwritten on new
        assert_eq!(std::fs::read(&path).unwrap(), corrupt);

        let outcome = ctrl.set(ExclusionRules::default());
        assert!(outcome.persisted);
        assert!(ctrl.is_available());
        assert_ne!(std::fs::read(&path).unwrap(), corrupt);

        let _ = std::fs::remove_dir_all(root);
    }
    #[test]
    fn unreadable_exclusions_do_not_become_allow_all_and_repair_restores_availability() {
        let dir = std::env::temp_dir().join(format!(
            "solstone-test-unreadable-exclusions-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("exclusions.json");

        // 1. Missing file -> Fresh default
        assert!(matches!(load_rules(&path), RulesLoadOutcome::Fresh(_)));

        // 2. Corrupt JSON -> Unavailable, bytes stay put
        let corrupt_bytes = b"not json at all { unclosed";
        std::fs::write(&path, corrupt_bytes).unwrap();
        assert_eq!(load_rules(&path), RulesLoadOutcome::Unavailable);
        assert_eq!(std::fs::read(&path).unwrap(), corrupt_bytes);

        // Failed repair to an invalid path leaves the original file untouched
        let not_a_dir = dir.join("not_a_dir");
        std::fs::write(&not_a_dir, b"file").unwrap();
        let bad_path = not_a_dir.join("exclusions.json");
        let repair_rules = ExclusionRules {
            excluded_exes: vec!["secret.exe".into()],
            title_patterns: vec!["confidential".into()],
            exclude_private_browsing: true,
        };
        assert!(persist_rules(&bad_path, &repair_rules).is_err());
        // Original corrupt bytes still stay put
        assert_eq!(std::fs::read(&path).unwrap(), corrupt_bytes);

        // 3. Unreadable input (directory at path on Unix)
        #[cfg(unix)]
        {
            let dir_as_file = dir.join("unreadable_dir.json");
            std::fs::create_dir_all(&dir_as_file).unwrap();
            assert_eq!(load_rules(&dir_as_file), RulesLoadOutcome::Unavailable);
            let _ = std::fs::remove_dir_all(&dir_as_file);
        }

        // 4. Successful repair replaces file via temp + rename and loads as Available
        persist_rules(&path, &repair_rules).expect("repair must succeed");
        match load_rules(&path) {
            RulesLoadOutcome::Available(loaded) => {
                assert_eq!(loaded, repair_rules.normalized());
            }
            other => panic!("expected Available after repair, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
