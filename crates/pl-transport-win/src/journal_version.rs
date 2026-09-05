// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! State owner for the read-only journal version fact.
//!
//! Owns the in-memory version string, freshness state, persistence to `journal-version.json`,
//! generation and epoch counters to discard obsolete fetch completions, and token-based concurrency gating.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use observer_model::SyncSnapshot;
use serde::{Deserialize, Serialize};

use crate::client::ObserverClient;
use crate::credential::{hex_lower, Credential};
use crate::TransportError;

fn is_sanitized_version(v: &str) -> bool {
    !v.is_empty() && v.len() <= 128 && !v.bytes().any(|b| b < 0x20 || b == 0x7f)
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedJournalVersion {
    instance_id: String,
    ca_fp_prefix_hex: String,
    version: String,
    updated_at_epoch_secs: u64,
}

fn save_persisted_atomic(path: &Path, record: &PersistedJournalVersion) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(record)?;
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[derive(Debug, Default)]
struct State {
    instance_id: Option<String>,
    ca_fp_prefix_hex: Option<String>,
    version: Option<String>,
    fresh: bool,
    session_generation: u64,
    connection_epoch: u64,
    in_flight_token: Option<(u64, u64)>,
}

/// Single process-wide controller for the journal version fact.
pub struct JournalVersionController {
    state_path: PathBuf,
    state: Mutex<State>,
}

impl JournalVersionController {
    /// Construct a new controller. Attempts to read the persisted state file if present.
    pub fn new(state_path: PathBuf) -> Self {
        let mut state = State::default();
        if let Ok(text) = std::fs::read_to_string(&state_path) {
            if let Ok(p) = serde_json::from_str::<PersistedJournalVersion>(&text) {
                if is_sanitized_version(&p.version) {
                    state.instance_id = Some(p.instance_id);
                    state.ca_fp_prefix_hex = Some(p.ca_fp_prefix_hex);
                    state.version = Some(p.version);
                    state.fresh = false;
                }
            }
        }
        Self {
            state_path,
            state: Mutex::new(state),
        }
    }

    /// Read the current (session_generation, connection_epoch) token.
    pub fn current_token(&self) -> (u64, u64) {
        let state = self.state.lock().expect("journal_version lock");
        (state.session_generation, state.connection_epoch)
    }

    /// Bind the identity for a new uploader session, advance the generation counter,
    /// and synchronize the snapshot.
    pub fn begin_session(&self, credential: &Credential, sync: &Arc<Mutex<SyncSnapshot>>) -> u64 {
        let target_instance_id = credential.instance_id.clone();
        let target_ca_fp_hex = hex_lower(&credential.ca_fp_prefix);

        let mut state = self.state.lock().expect("journal_version lock");
        let matches = match (&state.instance_id, &state.ca_fp_prefix_hex) {
            (Some(iid), Some(fp)) => iid == &target_instance_id && fp == &target_ca_fp_hex,
            _ => false,
        };

        if matches {
            // Identity unchanged: keep cached value, mark not fresh.
            state.fresh = false;
        } else {
            // Identity changed or first bind: check if disk file matches target identity.
            let disk_match = if let Ok(text) = std::fs::read_to_string(&self.state_path) {
                if let Ok(p) = serde_json::from_str::<PersistedJournalVersion>(&text) {
                    if p.instance_id == target_instance_id
                        && p.ca_fp_prefix_hex == target_ca_fp_hex
                        && is_sanitized_version(&p.version)
                    {
                        Some(p.version)
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };

            if let Some(cached_version) = disk_match {
                state.instance_id = Some(target_instance_id);
                state.ca_fp_prefix_hex = Some(target_ca_fp_hex);
                state.version = Some(cached_version);
                state.fresh = false;
            } else {
                // Wipe cache and delete persisted file to prevent resurrecting old identity's version.
                let _ = std::fs::remove_file(&self.state_path);
                state.instance_id = Some(target_instance_id);
                state.ca_fp_prefix_hex = Some(target_ca_fp_hex);
                state.version = None;
                state.fresh = false;
            }
        }

        state.session_generation += 1;
        let gen = state.session_generation;

        // Synchronize into SyncSnapshot.
        if let Ok(mut s) = sync.lock() {
            s.journal_version = state.version.clone();
            s.journal_version_fresh = state.fresh;
        }

        gen
    }

    /// Mark the connection disconnected: sets freshness to false in snapshot and in-memory state,
    /// and bumps connection epoch to fence in-flight fetches.
    pub fn mark_disconnected(&self, sync: &Arc<Mutex<SyncSnapshot>>) {
        if let Ok(mut state) = self.state.lock() {
            state.connection_epoch += 1;
            state.fresh = false;
        }
        if let Ok(mut s) = sync.lock() {
            s.journal_version_fresh = false;
        }
    }

    /// Trigger a background refresh of the journal version.
    /// Coalesces overlapping fetches within the exact current (session_generation, connection_epoch) token.
    pub fn trigger_refresh(
        self: &Arc<Self>,
        client: Arc<ObserverClient>,
        sync: Arc<Mutex<SyncSnapshot>>,
    ) {
        let token = {
            let mut state = self.state.lock().expect("journal_version lock");
            if state.session_generation == 0 {
                return;
            }
            let token = (state.session_generation, state.connection_epoch);
            if state.in_flight_token == Some(token) {
                return;
            }
            state.in_flight_token = Some(token);
            token
        };

        let ctrl = self.clone();
        tokio::spawn(async move {
            let result = client.system_status().await;
            ctrl.apply_result(token, result, &sync);
        });
    }

    /// Atomic completion path for a fetch result.
    pub fn apply_result(
        &self,
        token: (u64, u64),
        result: Result<String, TransportError>,
        sync: &Arc<Mutex<SyncSnapshot>>,
    ) {
        let (iid, fp, version_to_apply) = {
            let mut state = self.state.lock().expect("journal_version lock");
            if state.in_flight_token == Some(token) {
                state.in_flight_token = None;
            }
            if token != (state.session_generation, state.connection_epoch) {
                return;
            }
            match result {
                Ok(v) if is_sanitized_version(&v) => {
                    state.version = Some(v.clone());
                    state.fresh = true;
                    (
                        state.instance_id.clone(),
                        state.ca_fp_prefix_hex.clone(),
                        Some(v),
                    )
                }
                Ok(_) | Err(_) => (None, None, None),
            }
        };

        if let Some(version) = version_to_apply {
            if let Ok(mut s) = sync.lock() {
                s.journal_version = Some(version.clone());
                s.journal_version_fresh = true;
            }
            if let (Some(instance_id), Some(ca_fp_prefix_hex)) = (iid, fp) {
                let now_secs = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let persisted = PersistedJournalVersion {
                    instance_id,
                    ca_fp_prefix_hex,
                    version,
                    updated_at_epoch_secs: now_secs,
                };
                let _ = save_persisted_atomic(&self.state_path, &persisted);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn in_flight_token(&self) -> Option<(u64, u64)> {
        let state = self.state.lock().expect("journal_version lock");
        state.in_flight_token
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "solstone-jv-test-{}-{}-{}",
            std::process::id(),
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn make_credential(instance_id: &str, ca_fp_prefix: &[u8]) -> Credential {
        Credential {
            client_cert_pem: "cert".into(),
            client_key_pem: "key".into(),
            ca_chain_pem: vec!["ca".into()],
            ca_fp_prefix: ca_fp_prefix.to_vec(),
            instance_id: instance_id.to_string(),
            home_label: "Home".to_string(),
            endpoints: vec![crate::credential::EndpointAddr {
                host: "127.0.0.1".to_string(),
                port: 4443,
            }],
            relay_origin: None,
            device_token: None,
            device_token_expires_at: None,
        }
    }

    #[test]
    fn cold_start_restore_and_begin_session() {
        let dir = temp_test_dir("cold-start");
        let path = dir.join("journal-version.json");

        let initial_record = PersistedJournalVersion {
            instance_id: "inst-1".to_string(),
            ca_fp_prefix_hex: "0102".to_string(),
            version: "0.9.5".to_string(),
            updated_at_epoch_secs: 100,
        };
        save_persisted_atomic(&path, &initial_record).unwrap();

        let ctrl = JournalVersionController::new(path);
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let cred = make_credential("inst-1", &[0x01, 0x02]);

        let gen = ctrl.begin_session(&cred, &sync);
        assert_eq!(gen, 1);

        let snap = sync.lock().unwrap().clone();
        assert_eq!(snap.journal_version.as_deref(), Some("0.9.5"));
        assert!(!snap.journal_version_fresh);
    }

    #[test]
    fn different_identity_re_pair_clears_old_value_and_file() {
        let dir = temp_test_dir("different-identity");
        let path = dir.join("journal-version.json");

        let initial_record = PersistedJournalVersion {
            instance_id: "inst-1".to_string(),
            ca_fp_prefix_hex: "0102".to_string(),
            version: "0.9.5".to_string(),
            updated_at_epoch_secs: 100,
        };
        save_persisted_atomic(&path, &initial_record).unwrap();

        let ctrl = JournalVersionController::new(path.clone());
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));

        // Pair with DIFFERENT identity
        let cred2 = make_credential("inst-2", &[0xaa, 0xbb]);
        let gen = ctrl.begin_session(&cred2, &sync);
        assert_eq!(gen, 1);

        let snap = sync.lock().unwrap().clone();
        assert_eq!(snap.journal_version, None);
        assert!(!snap.journal_version_fresh);
        assert!(!path.exists());
    }

    #[test]
    fn apply_result_success_updates_state_sync_and_persists() {
        let dir = temp_test_dir("apply-success");
        let path = dir.join("journal-version.json");

        let ctrl = JournalVersionController::new(path.clone());
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let cred = make_credential("inst-1", &[0x01, 0x02]);

        ctrl.begin_session(&cred, &sync);
        let token = ctrl.current_token();
        ctrl.apply_result(token, Ok("1.2.3".into()), &sync);

        let snap = sync.lock().unwrap().clone();
        assert_eq!(snap.journal_version.as_deref(), Some("1.2.3"));
        assert!(snap.journal_version_fresh);

        // Verify persisted on disk
        let text = std::fs::read_to_string(&path).unwrap();
        let persisted: PersistedJournalVersion = serde_json::from_str(&text).unwrap();
        assert_eq!(persisted.version, "1.2.3");
        assert_eq!(persisted.instance_id, "inst-1");
        assert_eq!(persisted.ca_fp_prefix_hex, "0102");
    }

    #[test]
    fn apply_result_obsolete_generation_discarded() {
        let dir = temp_test_dir("obsolete-gen");
        let path = dir.join("journal-version.json");

        let ctrl = JournalVersionController::new(path.clone());
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let cred_a = make_credential("inst-a", &[0x01, 0x02]);

        ctrl.begin_session(&cred_a, &sync);
        let token_a = ctrl.current_token();

        // Re-pair with identity B (bumps session_generation)
        let cred_b = make_credential("inst-b", &[0xaa, 0xbb]);
        ctrl.begin_session(&cred_b, &sync);

        // Result from A's fetch completes now with stale token_a
        ctrl.apply_result(token_a, Ok("9.9.9".into()), &sync);

        let snap = sync.lock().unwrap().clone();
        // Stale result should NOT have been applied to identity B
        assert_eq!(snap.journal_version, None);
        assert!(!snap.journal_version_fresh);
        assert!(!path.exists());
    }

    #[test]
    fn disconnect_fences_inflight_completion() {
        let dir = temp_test_dir("disconnect-fence");
        let path = dir.join("journal-version.json");

        let ctrl = JournalVersionController::new(path);
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let cred = make_credential("inst-1", &[0x01, 0x02]);

        ctrl.begin_session(&cred, &sync);
        let token_before = ctrl.current_token();

        // Disconnect happens before fetch completes
        ctrl.mark_disconnected(&sync);

        // In-flight fetch finishes with token_before
        ctrl.apply_result(token_before, Ok("newer".into()), &sync);

        let snap = sync.lock().unwrap().clone();
        // Freshness should remain false, value should not be applied
        assert_eq!(snap.journal_version, None);
        assert!(!snap.journal_version_fresh);
    }

    #[test]
    fn malformed_or_failed_fetch_preserves_existing_cache() {
        let dir = temp_test_dir("malformed-failed-fetch");
        let path = dir.join("journal-version.json");

        let ctrl = JournalVersionController::new(path);
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let cred = make_credential("inst-1", &[0x01, 0x02]);

        ctrl.begin_session(&cred, &sync);
        let token1 = ctrl.current_token();
        ctrl.apply_result(token1, Ok("1.2.3".into()), &sync);

        ctrl.mark_disconnected(&sync);
        let token2 = ctrl.current_token();

        // Attempt to apply a transport error
        ctrl.apply_result(
            token2,
            Err(TransportError::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "reset",
            ))),
            &sync,
        );

        let snap = sync.lock().unwrap().clone();
        assert_eq!(snap.journal_version.as_deref(), Some("1.2.3"));
        assert!(!snap.journal_version_fresh);

        // Attempt to apply a malformed version (with newline)
        ctrl.apply_result(token2, Ok("1.2.4\nmalicious".into()), &sync);

        let snap = sync.lock().unwrap().clone();
        assert_eq!(snap.journal_version.as_deref(), Some("1.2.3"));
        assert!(!snap.journal_version_fresh);
    }

    #[test]
    fn newer_value_on_reconnect_replaces_old_value() {
        let dir = temp_test_dir("newer-value");
        let path = dir.join("journal-version.json");

        let ctrl = JournalVersionController::new(path.clone());
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let cred = make_credential("inst-1", &[0x01, 0x02]);

        ctrl.begin_session(&cred, &sync);
        let token1 = ctrl.current_token();
        ctrl.apply_result(token1, Ok("1.0.0".into()), &sync);

        // Disconnect
        ctrl.mark_disconnected(&sync);

        // Reconnect fetch with new token
        let token2 = ctrl.current_token();
        ctrl.apply_result(token2, Ok("2.0.0".into()), &sync);

        let snap = sync.lock().unwrap().clone();
        assert_eq!(snap.journal_version.as_deref(), Some("2.0.0"));
        assert!(snap.journal_version_fresh);

        let text = std::fs::read_to_string(&path).unwrap();
        let persisted: PersistedJournalVersion = serde_json::from_str(&text).unwrap();
        assert_eq!(persisted.version, "2.0.0");
    }
}
