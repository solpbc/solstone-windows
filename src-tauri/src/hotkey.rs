// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Global-hotkey config controller (shell side).
//!
//! Owns the persisted `hotkey.json` and the two shared handles the notification
//! pump uses: the owner's desired [`HotkeyConfig`] (written here on `set`) and the
//! honest [`HotkeyRegistration`] outcome the pump writes back. The Win32
//! registration itself lives in `platform-win` — it must run on the pump's own
//! thread, where `RegisterHotKey`/`WM_HOTKEY` live — so this module is only I/O +
//! sharing, the same shape as the capture-exclusions controller.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use observer_hotkey::{HotkeyConfig, HotkeyRegistration, HotkeyView};

/// Cheaply-clonable handle shared by the IPC commands and the notification pump.
#[derive(Clone)]
pub struct HotkeyController {
    desired: Arc<Mutex<HotkeyConfig>>,
    outcome: Arc<Mutex<HotkeyRegistration>>,
    path: Arc<PathBuf>,
}

impl HotkeyController {
    /// Load the persisted config (default when absent or corrupt), share it behind
    /// the desired handle, and seed the file on disk. The registration outcome
    /// starts `Inactive` and is updated by the pump once it reconciles.
    pub fn new(path: PathBuf) -> Self {
        let loaded = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<HotkeyConfig>(&text).ok())
            .unwrap_or_default();
        let ctrl = Self {
            desired: Arc::new(Mutex::new(loaded)),
            outcome: Arc::new(Mutex::new(HotkeyRegistration::Inactive)),
            path: Arc::new(path),
        };
        ctrl.persist(&loaded);
        ctrl
    }

    /// The desired-config handle the pump reads each poll.
    pub fn desired_handle(&self) -> Arc<Mutex<HotkeyConfig>> {
        Arc::clone(&self.desired)
    }

    /// The registration-outcome handle the pump writes the honest result into.
    pub fn outcome_handle(&self) -> Arc<Mutex<HotkeyRegistration>> {
        Arc::clone(&self.outcome)
    }

    /// The current config + live registration outcome, for the Settings render.
    pub fn view(&self) -> HotkeyView {
        HotkeyView {
            config: self.desired.lock().map(|c| *c).unwrap_or_default(),
            registration: self.outcome.lock().map(|o| *o).unwrap_or_default(),
        }
    }

    /// Replace the desired config: update the shared handle (the pump reconciles
    /// registration on its next poll, ~250 ms) and persist to disk.
    pub fn set(&self, config: HotkeyConfig) {
        if let Ok(mut guard) = self.desired.lock() {
            *guard = config;
        }
        self.persist(&config);
    }

    fn persist(&self, config: &HotkeyConfig) {
        match serde_json::to_string_pretty(config) {
            Ok(text) => {
                if let Some(dir) = self.path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                if let Err(e) = std::fs::write(self.path.as_path(), text) {
                    tracing::warn!(
                        target: "config",
                        area = "hotkey",
                        path = %self.path.display(),
                        error = %e,
                        "persist failed"
                    );
                }
            }
            Err(e) => tracing::warn!(
                target: "config",
                area = "hotkey",
                error = %e,
                "serialize failed"
            ),
        }
    }
}
