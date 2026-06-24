// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Cache-retention config controller (shell side).
//!
//! Owns the persisted `retention.json` and the shared [`RetentionConfig`] handle
//! the upload coordinator reads (via `SyncConfig`) when an upload is confirmed:
//! delete the local copy now (don't-keep) or retain + prune past the window. The
//! prune decision lives in the pure `observer-retention` crate; this is only I/O +
//! sharing, the same shape as the other control controllers.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use observer_retention::RetentionConfig;

/// Cheaply-clonable handle shared by the IPC commands and the upload coordinator.
#[derive(Clone)]
pub struct RetentionController {
    config: Arc<RwLock<RetentionConfig>>,
    path: Arc<PathBuf>,
}

impl RetentionController {
    /// Load the persisted policy (default = don't-keep when absent/corrupt),
    /// normalize it, share it, and seed the file on disk.
    pub fn new(path: PathBuf) -> Self {
        let loaded = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<RetentionConfig>(&text).ok())
            .unwrap_or_default()
            .normalized();
        let ctrl = Self {
            config: Arc::new(RwLock::new(loaded)),
            path: Arc::new(path),
        };
        ctrl.persist(&loaded);
        ctrl
    }

    /// The shared handle the upload coordinator reads on each confirmation.
    pub fn config_handle(&self) -> Arc<RwLock<RetentionConfig>> {
        Arc::clone(&self.config)
    }

    /// The current policy, for the Settings render.
    pub fn get(&self) -> RetentionConfig {
        self.config.read().map(|c| *c).unwrap_or_default()
    }

    /// Replace the policy: normalize, update the live shared handle (the
    /// coordinator honors it on its next tick), and persist.
    pub fn set(&self, config: RetentionConfig) {
        let normalized = config.normalized();
        if let Ok(mut guard) = self.config.write() {
            *guard = normalized;
        }
        self.persist(&normalized);
    }

    fn persist(&self, config: &RetentionConfig) {
        match serde_json::to_string_pretty(config) {
            Ok(text) => {
                if let Some(dir) = self.path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                if let Err(e) = std::fs::write(self.path.as_path(), text) {
                    tracing::warn!(
                        target: "config",
                        area = "retention",
                        path = %self.path.display(),
                        error = %e,
                        "persist failed"
                    );
                }
            }
            Err(e) => tracing::warn!(
                target: "config",
                area = "retention",
                error = %e,
                "serialize failed"
            ),
        }
    }
}
