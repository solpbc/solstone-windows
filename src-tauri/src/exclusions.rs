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
use std::sync::{Arc, RwLock};

use observer_exclusion::ExclusionRules;
use serde::Serialize;

/// Agent-native diagnostic for `--dump-windows`: the windows the exclusion
/// enumerator currently sees on the primary monitor, the active rules, and the
/// per-frame verdict those rules produce — as JSON. Lets an operator confirm
/// exclusion behavior headlessly (run it in the interactive session) without
/// inspecting a captured segment. Read-only: it does not touch `exclusions.json`.
pub fn dump_windows_json() -> String {
    let rules = std::fs::read_to_string(platform_win::local_data_root().join("exclusions.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<ExclusionRules>(&text).ok())
        .unwrap_or_default()
        .normalized();
    let windows = capture_wgc::dump_primary_monitor_windows();
    let decision = observer_exclusion::evaluate(&rules, &windows);
    serde_json::to_string_pretty(&serde_json::json!({
        "rules": rules,
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
}

/// IPC outcome of a `set_exclusions`: whether the new rules were durably
/// written to disk. The rules take effect in memory regardless; `persisted`
/// is false only when the disk write (or serialize) failed.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct SetExclusionsOutcome {
    pub persisted: bool,
}

impl ExclusionController {
    /// Load rules from `path` (defaulting when absent or corrupt), normalize
    /// them, hold them behind a shared lock, and seed/normalize the file on disk.
    pub fn new(path: PathBuf) -> Self {
        let loaded = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<ExclusionRules>(&text).ok())
            .unwrap_or_default()
            .normalized();
        let ctrl = Self {
            rules: Arc::new(RwLock::new(loaded.clone())),
            path: Arc::new(path),
        };
        // A seed-write failure is non-fatal: rules are live in memory and the
        // failure was already logged.
        let _ = ctrl.persist(&loaded);
        ctrl
    }

    /// The shared handle the WGC source reads each frame.
    pub fn rules_handle(&self) -> Arc<RwLock<ExclusionRules>> {
        Arc::clone(&self.rules)
    }

    /// The current rules (for the Settings initial render).
    pub fn get(&self) -> ExclusionRules {
        self.rules.read().map(|r| r.clone()).unwrap_or_default()
    }

    /// Replace the rules: normalize, update the live shared handle (effective on
    /// the next frame), and persist to disk.
    pub fn set(&self, rules: ExclusionRules) -> SetExclusionsOutcome {
        let normalized = rules.normalized();
        if let Ok(mut guard) = self.rules.write() {
            *guard = normalized.clone();
        }
        SetExclusionsOutcome {
            persisted: self.persist(&normalized),
        }
    }

    fn persist(&self, rules: &ExclusionRules) -> bool {
        match serde_json::to_string_pretty(rules) {
            Ok(text) => {
                if let Some(dir) = self.path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                if let Err(e) = std::fs::write(self.path.as_path(), text) {
                    tracing::warn!(
                        target: "config",
                        area = "exclusions",
                        path = %self.path.display(),
                        error = %e,
                        "persist failed"
                    );
                    false
                } else {
                    true
                }
            }
            Err(e) => {
                tracing::warn!(
                    target: "config",
                    area = "exclusions",
                    error = %e,
                    "serialize failed"
                );
                false
            }
        }
    }
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
}
