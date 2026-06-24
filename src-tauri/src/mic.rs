// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Microphone-control config controller (shell side).
//!
//! Owns the persisted `mic.json` and the two shared handles the WASAPI mic source
//! uses: the owner's [`MicConfig`] (device priority + disable + gain, written here
//! on `set`) and the id of the device the loop has actually opened (written by the
//! source, read by Settings so "active" is earned, never guessed). The device
//! enumeration + selection + gain DSP live in the pure `observer-mic` crate and
//! the `capture-wasapi` platform tier; this is only I/O + sharing.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use observer_mic::{MicConfig, MicView};

/// Cheaply-clonable handle shared by the IPC commands and the mic source.
#[derive(Clone)]
pub struct MicController {
    config: Arc<RwLock<MicConfig>>,
    active: Arc<Mutex<Option<String>>>,
    path: Arc<PathBuf>,
}

impl MicController {
    /// Load the persisted config (default when absent/corrupt), normalize it,
    /// share it behind the config handle, and seed the file on disk.
    pub fn new(path: PathBuf) -> Self {
        let loaded = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<MicConfig>(&text).ok())
            .unwrap_or_default()
            .normalized();
        let ctrl = Self {
            config: Arc::new(RwLock::new(loaded.clone())),
            active: Arc::new(Mutex::new(None)),
            path: Arc::new(path),
        };
        ctrl.persist(&loaded);
        ctrl
    }

    /// The shared config handle the mic source reconciles selection + gain from.
    pub fn config_handle(&self) -> Arc<RwLock<MicConfig>> {
        Arc::clone(&self.config)
    }

    /// The shared handle the mic source publishes the actually-open device id into.
    pub fn active_handle(&self) -> Arc<Mutex<Option<String>>> {
        Arc::clone(&self.active)
    }

    /// The config + the actually-open device id, for the Settings render.
    pub fn view(&self) -> MicView {
        MicView {
            config: self.config.read().map(|c| c.clone()).unwrap_or_default(),
            active_id: self.active.lock().ok().and_then(|a| a.clone()),
        }
    }

    /// Replace the config: normalize, update the live shared handle (the mic loop
    /// reconciles on its next cadence), and persist to disk.
    pub fn set(&self, config: MicConfig) {
        let normalized = config.normalized();
        if let Ok(mut guard) = self.config.write() {
            *guard = normalized.clone();
        }
        self.persist(&normalized);
    }

    fn persist(&self, config: &MicConfig) {
        match serde_json::to_string_pretty(config) {
            Ok(text) => {
                if let Some(dir) = self.path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                if let Err(e) = std::fs::write(self.path.as_path(), text) {
                    tracing::warn!(
                        target: "config",
                        area = "mic",
                        path = %self.path.display(),
                        error = %e,
                        "persist failed"
                    );
                }
            }
            Err(e) => tracing::warn!(
                target: "config",
                area = "mic",
                error = %e,
                "serialize failed"
            ),
        }
    }
}
