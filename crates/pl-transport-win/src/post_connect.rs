// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Post-connect device metadata publication and relay-access acquisition controller.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use observer_model::SyncSnapshot;

use crate::client::ClientSlot;
use crate::credential::{pairing_generation, CasKey, Credential, PairedState, StorageError};
use crate::device_metadata::{
    sanitize_facts, MetadataGetResponse, MetadataPutRequest, RawDeviceFacts, ReportedMetadata,
};
use crate::journal_version::JournalVersionController;
use crate::relay_access::{
    validate_relay_access_response, RelayAccessResponse, ValidatedReadyAccess,
};
use crate::ObserverClient;

/// Default overall timeout for a post-connect job (requests + body reads).
pub const DEFAULT_POST_CONNECT_DEADLINE: Duration = Duration::from_secs(15);

#[derive(Debug, Default)]
struct PostConnectState {
    session_generation: u64,
    connection_epoch: u64,
    pairing_generation: u64,
    paired_instance_id: Option<String>,
    in_flight_metadata: bool,
    pending_metadata: Option<ReportedMetadata>,
    last_published_metadata: Option<ReportedMetadata>,
    in_flight_access: bool,
    pending_access_trigger: bool,
    pending_durable_clear: Option<CasKey>,
}

/// Coordinates post-connect device metadata publication and relay access acquisition.
pub struct PostConnectController {
    client_slot: ClientSlot,
    state_path: Option<PathBuf>,
    journal_version: Option<Arc<JournalVersionController>>,
    sync: Arc<Mutex<SyncSnapshot>>,
    facts_fn: Arc<dyn Fn() -> RawDeviceFacts + Send + Sync>,
    deadline: Duration,
    relay_disconnect_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    state: Mutex<PostConnectState>,
}

impl PostConnectController {
    /// Create a new post-connect controller.
    pub fn new(
        client_slot: ClientSlot,
        state_path: Option<PathBuf>,
        journal_version: Option<Arc<JournalVersionController>>,
        sync: Arc<Mutex<SyncSnapshot>>,
        facts_fn: Arc<dyn Fn() -> RawDeviceFacts + Send + Sync>,
    ) -> Self {
        Self {
            client_slot,
            state_path,
            journal_version,
            sync,
            facts_fn,
            deadline: DEFAULT_POST_CONNECT_DEADLINE,
            relay_disconnect_hook: Mutex::new(None),
            state: Mutex::new(PostConnectState::default()),
        }
    }

    /// Set a custom execution deadline for post-connect jobs (useful in tests).
    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }

    /// Attach a callback to be invoked when live relay is disabled on `not_configured`.
    pub fn with_relay_disconnect_hook(self, hook: Arc<dyn Fn() + Send + Sync>) -> Self {
        *self.relay_disconnect_hook.lock().unwrap() = Some(hook);
        self
    }

    /// Set a callback to be invoked when live relay is disabled on `not_configured`.
    pub fn set_relay_disconnect_hook(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *self.relay_disconnect_hook.lock().unwrap() = Some(hook);
    }

    /// Read the current pending durable clear CAS key (test inspection only).
    pub fn pending_durable_clear(&self) -> Option<CasKey> {
        self.state.lock().unwrap().pending_durable_clear
    }

    /// Read the last successfully published metadata (test inspection / telemetry).
    pub fn last_published_metadata(&self) -> Option<ReportedMetadata> {
        self.state.lock().unwrap().last_published_metadata.clone()
    }

    /// Begin a new session with the given paired credential.
    pub fn begin_session(&self, credential: &Credential) -> u64 {
        let mut state = self.state.lock().unwrap();
        state.session_generation += 1;
        state.connection_epoch += 1;
        state.pairing_generation = pairing_generation(&credential.client_cert_pem);
        state.paired_instance_id = Some(credential.instance_id.clone());
        state.in_flight_metadata = false;
        state.pending_metadata = None;
        state.last_published_metadata = None;
        state.in_flight_access = false;
        state.pending_access_trigger = false;
        state.pending_durable_clear = None;
        state.session_generation
    }

    /// Mark the connection disconnected for the given session generation, bumping epoch to fence in-flight jobs.
    pub fn mark_session_disconnected(&self, generation: u64) {
        let mut state = self.state.lock().unwrap();
        if state.session_generation == generation {
            state.connection_epoch += 1;
            state.in_flight_metadata = false;
            state.in_flight_access = false;
        }
    }

    /// Reset all state on unpair/re-pair.
    pub fn clear(&self) {
        let mut state = self.state.lock().unwrap();
        state.session_generation += 1;
        state.connection_epoch += 1;
        state.pairing_generation = 0;
        state.paired_instance_id = None;
        state.in_flight_metadata = false;
        state.pending_metadata = None;
        state.last_published_metadata = None;
        state.in_flight_access = false;
        state.pending_access_trigger = false;
        state.pending_durable_clear = None;
    }

    /// Trigger post-connect metadata publication and relay access acquisition.
    ///
    /// Resamples facts dynamically per trigger. Coalesces in-flight jobs using pending-latest-snapshot.
    pub fn trigger(self: &Arc<Self>) {
        self.retry_pending_durable_clear_if_needed();

        let raw_facts = (self.facts_fn)();
        let sanitized = sanitize_facts(&raw_facts);

        self.trigger_metadata(sanitized);
        self.trigger_access();
    }

    fn retry_pending_durable_clear_if_needed(&self) {
        let pending_cas = {
            let state = self.state.lock().unwrap();
            state.pending_durable_clear
        };

        if let (Some(cas_key), Some(path)) = (pending_cas, &self.state_path) {
            let res = PairedState::mutate(path, cas_key, |cred| {
                cred.relay_origin = None;
                cred.device_token = None;
                cred.device_token_expires_at = None;
                Ok(())
            });
            match res {
                Ok(_) | Err(StorageError::CasMismatch) => {
                    let mut state = self.state.lock().unwrap();
                    if state.pending_durable_clear == Some(cas_key) {
                        state.pending_durable_clear = None;
                    }
                }
                Err(StorageError::WriteFailed(e)) | Err(StorageError::DurabilityUncertain(e)) => {
                    tracing::warn!(target: "sync", error = %e, "retry of durable clear failed");
                }
                Err(e) => {
                    tracing::warn!(target: "sync", error = %e, "retry of durable clear error");
                }
            }
        }
    }

    fn trigger_metadata(self: &Arc<Self>, snapshot: ReportedMetadata) {
        let token = {
            let mut state = self.state.lock().unwrap();
            if state.paired_instance_id.is_none() {
                return;
            }
            let token = (
                state.session_generation,
                state.connection_epoch,
                state.pairing_generation,
            );
            if state.in_flight_metadata {
                state.pending_metadata = Some(snapshot);
                return;
            } else {
                state.in_flight_metadata = true;
                token
            }
        };

        let this = self.clone();
        tokio::spawn(async move {
            this.run_metadata_loop(token, snapshot).await;
        });
    }

    async fn run_metadata_loop(
        self: Arc<Self>,
        mut token: (u64, u64, u64),
        mut current: ReportedMetadata,
    ) {
        loop {
            let deadline = self.deadline;
            let result =
                tokio::time::timeout(deadline, self.execute_metadata_job(token, &current)).await;

            // Handle timeout / result
            if let Err(_timed_out) = result {
                tracing::warn!(target: "sync", "post-connect metadata job timed out after {:?}", deadline);
            }

            // In-flight release and pending check
            let next = {
                let mut state = self.state.lock().unwrap();
                state.in_flight_metadata = false;
                let current_token = (
                    state.session_generation,
                    state.connection_epoch,
                    state.pairing_generation,
                );
                if current_token == token {
                    if let Some(pending) = state.pending_metadata.take() {
                        state.in_flight_metadata = true;
                        Some((current_token, pending))
                    } else {
                        None
                    }
                } else {
                    None
                }
            };

            match next {
                Some((new_token, pending_snapshot)) => {
                    token = new_token;
                    current = pending_snapshot;
                }
                None => break,
            }
        }
    }

    async fn execute_metadata_job(&self, token: (u64, u64, u64), current: &ReportedMetadata) {
        let client = self.client_slot.load();
        let get_resp = match client.get_clients_self().await {
            Ok(resp) => resp,
            Err(e) => {
                tracing::debug!(target: "sync", error = %e, "metadata GET failed");
                return;
            }
        };

        // Old home: 404 -> skip PUT
        if get_resp.status == 404 {
            tracing::debug!(target: "sync", "metadata GET returned 404 (old home); skipping PUT");
            return;
        }

        if !get_resp.is_success() {
            tracing::warn!(target: "sync", status = get_resp.status, "metadata GET non-success");
            return;
        }

        let parsed: MetadataGetResponse = match serde_json::from_slice(&get_resp.body) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(target: "sync", error = %e, "failed to parse metadata GET response");
                return;
            }
        };

        if parsed.protocol_version != 1 {
            tracing::warn!(target: "sync", ver = parsed.protocol_version, "unsupported metadata protocol version");
            return;
        }

        // Publish journal version out-of-band if present
        if let Some(journal) = &parsed.journal {
            if let Some(v) = &journal.version {
                if let Some(jv) = &self.journal_version {
                    jv.publish_version(v, token.0, &self.sync);
                }
            }
        }

        // Loop prevention: check if unchanged vs server reported
        if parsed.reported.as_ref() == Some(current) {
            let mut state = self.state.lock().unwrap();
            state.last_published_metadata = Some(current.clone());
            return;
        }

        // Generation fence before PUT side-effect
        {
            let state = self.state.lock().unwrap();
            let current_token = (
                state.session_generation,
                state.connection_epoch,
                state.pairing_generation,
            );
            if current_token != token {
                tracing::debug!(target: "sync", "metadata PUT skipped due to stale session token");
                return;
            }
        }

        // Send PUT
        let put_body = match serde_json::to_vec(&MetadataPutRequest {
            protocol_version: 1,
            expected_revision: parsed.revision,
            reported: current,
        }) {
            Ok(b) => b,
            Err(_) => return,
        };

        let put_resp = match client.put_clients_self(&put_body).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(target: "sync", error = %e, "metadata PUT failed");
                return;
            }
        };

        if put_resp.is_success() {
            let mut state = self.state.lock().unwrap();
            state.last_published_metadata = Some(current.clone());
            return;
        }

        // HTTP 409 Conflict handling: re-read GET and retry at most once with newest pending snapshot
        if put_resp.status == 409 {
            tracing::debug!(target: "sync", "metadata PUT 409 conflict, retrying with newest snapshot");
            let retry_get = match client.get_clients_self().await {
                Ok(r) if r.is_success() => r,
                _ => return,
            };
            let retry_parsed: MetadataGetResponse = match serde_json::from_slice(&retry_get.body) {
                Ok(p) => p,
                Err(_) => return,
            };

            let newest_snapshot = {
                let mut state = self.state.lock().unwrap();
                let current_token = (
                    state.session_generation,
                    state.connection_epoch,
                    state.pairing_generation,
                );
                if current_token != token {
                    return;
                }
                state
                    .pending_metadata
                    .take()
                    .unwrap_or_else(|| current.clone())
            };

            if retry_parsed.reported.as_ref() == Some(&newest_snapshot) {
                let mut state = self.state.lock().unwrap();
                state.last_published_metadata = Some(newest_snapshot);
                return;
            }

            let retry_put_body = match serde_json::to_vec(&MetadataPutRequest {
                protocol_version: 1,
                expected_revision: retry_parsed.revision,
                reported: &newest_snapshot,
            }) {
                Ok(b) => b,
                Err(_) => return,
            };

            if let Ok(retry_put_resp) = client.put_clients_self(&retry_put_body).await {
                if retry_put_resp.is_success() {
                    let mut state = self.state.lock().unwrap();
                    state.last_published_metadata = Some(newest_snapshot);
                }
            }
        }
    }

    fn trigger_access(self: &Arc<Self>) {
        let (token, paired_id, should_spawn) = {
            let mut state = self.state.lock().unwrap();
            let paired_id = match &state.paired_instance_id {
                Some(id) => id.clone(),
                None => return,
            };
            let token = (
                state.session_generation,
                state.connection_epoch,
                state.pairing_generation,
            );
            if state.in_flight_access {
                state.pending_access_trigger = true;
                (token, paired_id, false)
            } else {
                state.in_flight_access = true;
                (token, paired_id, true)
            }
        };

        if should_spawn {
            let this = self.clone();
            tokio::spawn(async move {
                this.run_access_loop(token, paired_id).await;
            });
        }
    }

    async fn run_access_loop(self: Arc<Self>, mut token: (u64, u64, u64), paired_id: String) {
        loop {
            let deadline = self.deadline;
            let result =
                tokio::time::timeout(deadline, self.execute_access_job(token, &paired_id)).await;

            if let Err(_timed_out) = result {
                tracing::warn!(target: "sync", "post-connect relay access job timed out after {:?}", deadline);
            }

            // In-flight release and pending check
            let next = {
                let mut state = self.state.lock().unwrap();
                state.in_flight_access = false;
                let current_token = (
                    state.session_generation,
                    state.connection_epoch,
                    state.pairing_generation,
                );
                if current_token == token && state.pending_access_trigger {
                    state.pending_access_trigger = false;
                    state.in_flight_access = true;
                    Some(current_token)
                } else {
                    None
                }
            };

            match next {
                Some(new_token) => {
                    token = new_token;
                }
                None => break,
            }
        }
    }

    async fn execute_access_job(&self, token: (u64, u64, u64), paired_instance_id: &str) {
        let client = self.client_slot.load();
        let resp = match client.get_relay_access().await {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(target: "sync", error = %e, "relay access GET failed");
                return;
            }
        };

        // 404, 503, or other non-success -> preserve existing cache and LAN
        if !resp.is_success() {
            tracing::debug!(target: "sync", status = resp.status, "relay access GET returned non-success");
            return;
        }

        let parsed: RelayAccessResponse = match serde_json::from_slice(&resp.body) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(target: "sync", error = %e, "failed to parse relay access response");
                return;
            }
        };

        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let validated = match validate_relay_access_response(&parsed, paired_instance_id, now_secs)
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(target: "sync", error = %e, "relay access validation failed");
                return;
            }
        };

        self.apply_relay_access_outcome(validated, &token);
    }

    pub(crate) fn apply_relay_access_outcome(
        &self,
        validated: Option<ValidatedReadyAccess>,
        token: &(u64, u64, u64),
    ) {
        // Generation fence before side-effects
        {
            let state = self.state.lock().unwrap();
            let current_token = (
                state.session_generation,
                state.connection_epoch,
                state.pairing_generation,
            );
            if current_token != *token {
                tracing::debug!(target: "sync", "apply_relay_access_outcome skipped due to stale token");
                return;
            }
        }

        let client = self.client_slot.load();
        let current_cas = client.current_cas_key().unwrap_or(CasKey {
            pairing_generation: token.2,
            access_mutation_generation: 0,
        });

        match validated {
            None => {
                // not_configured: immediately replace with LAN-only client
                let mut lan_cred = client.credential().clone();
                lan_cred.relay_origin = None;
                lan_cred.device_token = None;
                lan_cred.device_token_expires_at = None;

                if let Ok(lan_client) = ObserverClient::new(lan_cred) {
                    let mut configured_client = lan_client.with_cas_key(current_cas);
                    if let Some(path) = &self.state_path {
                        configured_client = configured_client.with_state_path(path.clone());
                    }
                    self.client_slot.replace(Arc::new(configured_client));
                }

                let hook = self.relay_disconnect_hook.lock().unwrap().clone();
                if let Some(hook) = hook {
                    hook();
                }

                // Ordered durable clear
                if let Some(path) = &self.state_path {
                    let res = PairedState::mutate(path, current_cas, |cred| {
                        cred.relay_origin = None;
                        cred.device_token = None;
                        cred.device_token_expires_at = None;
                        Ok(())
                    });
                    match res {
                        Ok(new_gen) => {
                            let mut state = self.state.lock().unwrap();
                            state.pending_durable_clear = None;
                            let new_cas = CasKey {
                                pairing_generation: token.2,
                                access_mutation_generation: new_gen,
                            };
                            let client = self.client_slot.load();
                            let updated_cred = client.credential().clone();
                            if let Ok(c) = ObserverClient::new(updated_cred) {
                                let mut cc = c.with_cas_key(new_cas);
                                if let Some(path) = &self.state_path {
                                    cc = cc.with_state_path(path.clone());
                                }
                                self.client_slot.replace(Arc::new(cc));
                            }
                        }
                        Err(StorageError::CasMismatch) => {
                            tracing::debug!(target: "sync", "durable clear skipped due to CAS mismatch");
                            let mut state = self.state.lock().unwrap();
                            if state.pending_durable_clear == Some(current_cas) {
                                state.pending_durable_clear = None;
                            }
                        }
                        Err(StorageError::WriteFailed(e))
                        | Err(StorageError::DurabilityUncertain(e)) => {
                            tracing::warn!(target: "sync", error = %e, "durable clear write failed");
                            let mut state = self.state.lock().unwrap();
                            state.pending_durable_clear = Some(current_cas);
                        }
                        Err(e) => {
                            tracing::warn!(target: "sync", error = %e, "durable clear error");
                        }
                    }
                }
            }
            Some(ready) => {
                // Ready: check if unchanged vs current live credential
                let current_cred = client.credential();
                if current_cred.relay_origin.as_deref() == Some(&ready.relay_origin)
                    && current_cred.device_token.as_deref() == Some(&ready.device_token)
                    && current_cred.device_token_expires_at == Some(ready.expires_at)
                {
                    // Unchanged: skip persist and replace
                    return;
                }

                let Some(path) = &self.state_path else {
                    return;
                };

                let origin_to_save = ready.relay_origin.clone();
                let token_to_save = ready.device_token.clone();
                let exp_to_save = ready.expires_at;

                let mutate_res = PairedState::mutate(path, current_cas, |cred| {
                    cred.relay_origin = Some(origin_to_save);
                    cred.device_token = Some(token_to_save);
                    cred.device_token_expires_at = Some(exp_to_save);
                    Ok(())
                });

                match mutate_res {
                    Ok(new_gen) => {
                        let mut new_cred = client.credential().clone();
                        new_cred.relay_origin = Some(ready.relay_origin);
                        new_cred.device_token = Some(ready.device_token);
                        new_cred.device_token_expires_at = Some(ready.expires_at);

                        if let Ok(new_client) = ObserverClient::new(new_cred) {
                            let new_cas = CasKey {
                                pairing_generation: token.2,
                                access_mutation_generation: new_gen,
                            };
                            let configured_client = new_client
                                .with_state_path(path.clone())
                                .with_cas_key(new_cas);
                            self.client_slot.replace(Arc::new(configured_client));
                        }
                    }
                    Err(StorageError::DurabilityUncertain(e)) => {
                        tracing::warn!(target: "sync", error = %e, "durability uncertain on ready persist; no live replace");
                    }
                    Err(StorageError::WriteFailed(e)) => {
                        tracing::warn!(target: "sync", error = %e, "write failed on ready persist; no live replace");
                    }
                    Err(StorageError::CasMismatch) => {
                        tracing::debug!(target: "sync", "ready persist skipped due to CAS mismatch");
                    }
                    Err(e) => {
                        tracing::warn!(target: "sync", error = %e, "ready persist error");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential::{pairing_generation, EndpointAddr, PairedState, FS_FAIL_POINT};
    use crate::device_metadata::RawDeviceFacts;
    use crate::relay_access::ValidatedReadyAccess;
    use crate::{CasKey, Credential};
    use observer_model::SyncSnapshot;
    use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};
    use std::sync::atomic::{AtomicBool, Ordering};

    fn dummy_credential(with_relay: bool) -> Credential {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let params = CertificateParams::new(vec!["spl.local".to_string()]).unwrap();
        let cert = params.self_signed(&key).unwrap();
        Credential {
            client_key_pem: key.serialize_pem(),
            client_cert_pem: cert.pem(),
            ca_chain_pem: vec![cert.pem()],
            ca_fp_prefix: vec![0; 16],
            instance_id: "test".into(),
            home_label: "Home".into(),
            endpoints: vec![EndpointAddr {
                host: "127.0.0.1".into(),
                port: 9,
            }],
            relay_origin: if with_relay {
                Some("https://relay.example.com".into())
            } else {
                None
            },
            device_token: if with_relay {
                Some("jwt-token".into())
            } else {
                None
            },
            device_token_expires_at: if with_relay { Some(1700000000) } else { None },
        }
    }

    fn test_setup(
        with_relay: bool,
    ) -> (PostConnectController, ClientSlot, PathBuf, Arc<AtomicBool>) {
        let dir = std::env::temp_dir().join(format!(
            "solstone-pc-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("paired.json");

        let cred = dummy_credential(with_relay);
        let p_gen = pairing_generation(&cred.client_cert_pem);
        let paired = PairedState {
            credential: Some(cred.clone()),
            access_mutation_generation: 0,
        };
        paired.save(&path).unwrap();

        let cas = CasKey {
            pairing_generation: p_gen,
            access_mutation_generation: 0,
        };
        let client = ObserverClient::new(cred)
            .unwrap()
            .with_state_path(path.clone())
            .with_cas_key(cas);
        let slot = ClientSlot::new(Arc::new(client));
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let facts = Arc::new(|| RawDeviceFacts {
            name: Some("test-device".into()),
            platform: Some("windows".into()),
            device_type: None,
            app_id: Some("app.solstone.windows".into()),
            app_version: Some("2.0.0".into()),
        });

        let controller =
            PostConnectController::new(slot.clone(), Some(path.clone()), None, sync, facts);

        let disconnect_called = Arc::new(AtomicBool::new(false));
        let dc = disconnect_called.clone();
        controller.set_relay_disconnect_hook(Arc::new(move || {
            dc.store(true, Ordering::SeqCst);
        }));

        (controller, slot, path, disconnect_called)
    }

    #[test]
    fn test_session_lifecycle() {
        let (controller, slot, path, _) = test_setup(false);
        let cred = slot.load().credential().clone();
        let p_gen = pairing_generation(&cred.client_cert_pem);

        // Before begin_session
        let gen0 = controller.state.lock().unwrap().session_generation;
        assert_eq!(gen0, 0);

        // Begin session
        let session_gen = controller.begin_session(&cred);
        assert_eq!(session_gen, 1);

        let state = controller.state.lock().unwrap();
        assert_eq!(state.session_generation, 1);
        assert_eq!(state.connection_epoch, 1);
        assert_eq!(state.pairing_generation, p_gen);
        assert_eq!(state.paired_instance_id.as_deref(), Some("test"));
        drop(state);

        // Disconnect wrong generation ignored
        controller.mark_session_disconnected(99);
        assert_eq!(controller.state.lock().unwrap().connection_epoch, 1);

        // Disconnect matching generation bumps epoch
        controller.mark_session_disconnected(1);
        assert_eq!(controller.state.lock().unwrap().connection_epoch, 2);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn test_apply_relay_access_ready() {
        let (controller, slot, path, _) = test_setup(false);
        let cred = slot.load().credential().clone();
        let p_gen = pairing_generation(&cred.client_cert_pem);
        controller.begin_session(&cred);
        let token = (1, 1, p_gen);

        let ready = ValidatedReadyAccess {
            relay_origin: "https://relay.new.org".into(),
            instance_id: "test".into(),
            device_token: "jwt-token-new".into(),
            expires_at: 1800000000,
        };

        controller.apply_relay_access_outcome(Some(ready), &token);

        // Client slot replaced with updated cred
        let active_client = slot.load();
        let active_cred = active_client.credential();
        assert_eq!(
            active_cred.relay_origin.as_deref(),
            Some("https://relay.new.org")
        );
        assert_eq!(active_cred.device_token.as_deref(), Some("jwt-token-new"));
        assert_eq!(active_cred.device_token_expires_at, Some(1800000000));
        assert_eq!(
            active_client
                .current_cas_key()
                .unwrap()
                .access_mutation_generation,
            1
        );

        // Disk state updated
        let loaded = PairedState::load(&path).unwrap();
        assert_eq!(loaded.access_mutation_generation, 1);
        let disk_cred = loaded.credential.unwrap();
        assert_eq!(
            disk_cred.relay_origin.as_deref(),
            Some("https://relay.new.org")
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn test_apply_relay_access_not_configured() {
        let (controller, slot, path, disconnect_called) = test_setup(true);
        let cred = slot.load().credential().clone();
        let p_gen = pairing_generation(&cred.client_cert_pem);
        controller.begin_session(&cred);
        let token = (1, 1, p_gen);

        controller.apply_relay_access_outcome(None, &token);

        // Client slot immediately swapped to LAN-only
        let active_client = slot.load();
        let active_cred = active_client.credential();
        assert_eq!(active_cred.relay_origin, None);
        assert_eq!(active_cred.device_token, None);
        assert_eq!(active_cred.device_token_expires_at, None);

        // Relay disconnect hook invoked
        assert!(disconnect_called.load(Ordering::SeqCst));

        // Disk state cleared
        let loaded = PairedState::load(&path).unwrap();
        assert_eq!(loaded.access_mutation_generation, 1);
        let disk_cred = loaded.credential.unwrap();
        assert_eq!(disk_cred.relay_origin, None);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn test_apply_relay_access_durability_uncertain_ready_no_live_replace() {
        let (controller, slot, path, _) = test_setup(false);
        let cred = slot.load().credential().clone();
        let p_gen = pairing_generation(&cred.client_cert_pem);
        controller.begin_session(&cred);
        let token = (1, 1, p_gen);

        FS_FAIL_POINT.with(|f| f.set(2));

        let ready = ValidatedReadyAccess {
            relay_origin: "https://relay.new.org".into(),
            instance_id: "test".into(),
            device_token: "jwt-token-new".into(),
            expires_at: 1800000000,
        };

        controller.apply_relay_access_outcome(Some(ready), &token);
        FS_FAIL_POINT.with(|f| f.set(0));

        // No live replace
        let active_client = slot.load();
        assert_eq!(active_client.credential().relay_origin, None);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn test_apply_relay_access_durability_uncertain_not_configured_records_pending_clear() {
        let (controller, slot, path, disconnect_called) = test_setup(true);
        let cred = slot.load().credential().clone();
        let p_gen = pairing_generation(&cred.client_cert_pem);
        controller.begin_session(&cred);
        let token = (1, 1, p_gen);

        FS_FAIL_POINT.with(|f| f.set(2));

        controller.apply_relay_access_outcome(None, &token);
        FS_FAIL_POINT.with(|f| f.set(0));

        // Live replace still occurred for LAN safety
        let active_client = slot.load();
        assert_eq!(active_client.credential().relay_origin, None);
        assert!(disconnect_called.load(Ordering::SeqCst));

        // Pending durable clear was recorded
        let state = controller.state.lock().unwrap();
        assert!(state.pending_durable_clear.is_some());

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn test_stale_token_fenced_from_applying_ready_or_not_configured() {
        let (controller, slot, path, disconnect_called) = test_setup(false);
        let cred = slot.load().credential().clone();
        let p_gen = pairing_generation(&cred.client_cert_pem);
        controller.begin_session(&cred);
        let stale_token = (1, 1, p_gen);

        // Disconnect bumps epoch to 2
        controller.mark_session_disconnected(1);

        let ready = ValidatedReadyAccess {
            relay_origin: "https://relay.stale.org".into(),
            instance_id: "test".into(),
            device_token: "jwt-token-stale".into(),
            expires_at: 1800000000,
        };

        // Stale apply ready is a no-op
        controller.apply_relay_access_outcome(Some(ready), &stale_token);
        assert_eq!(slot.load().credential().relay_origin, None);

        // Stale apply not_configured is a no-op
        controller.apply_relay_access_outcome(None, &stale_token);
        assert!(!disconnect_called.load(Ordering::SeqCst));

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn test_durable_clear_retry_cas_mismatch_does_not_clear_newer_ready() {
        let (controller, slot, path, _) = test_setup(true);
        let cred = slot.load().credential().clone();
        let p_gen = pairing_generation(&cred.client_cert_pem);
        controller.begin_session(&cred);
        let token = (1, 1, p_gen);

        // 1. Simulate pre-rename write failure on not_configured at gen 0
        FS_FAIL_POINT.with(|f| f.set(1));
        controller.apply_relay_access_outcome(None, &token);
        FS_FAIL_POINT.with(|f| f.set(0));

        assert_eq!(
            controller.pending_durable_clear(),
            Some(CasKey {
                pairing_generation: p_gen,
                access_mutation_generation: 0,
            })
        );

        // 2. A newer ready is persisted to disk at gen 1
        let ready = ValidatedReadyAccess {
            relay_origin: "https://relay.new.org".into(),
            instance_id: "test".into(),
            device_token: "jwt-token-new".into(),
            expires_at: 1900000000,
        };
        controller.apply_relay_access_outcome(Some(ready), &token);

        let loaded = PairedState::load(&path).unwrap();
        assert_eq!(loaded.access_mutation_generation, 1);
        assert_eq!(
            loaded.credential.as_ref().unwrap().relay_origin.as_deref(),
            Some("https://relay.new.org")
        );

        // 3. Retry the pending durable clear (bound to gen 0) -> CasMismatch
        controller.retry_pending_durable_clear_if_needed();

        // Pending clear is dropped on CasMismatch
        assert_eq!(controller.pending_durable_clear(), None);

        // Disk state is still ready (not wiped)
        let loaded_after = PairedState::load(&path).unwrap();
        assert_eq!(loaded_after.access_mutation_generation, 1);
        assert_eq!(
            loaded_after
                .credential
                .as_ref()
                .unwrap()
                .relay_origin
                .as_deref(),
            Some("https://relay.new.org")
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn test_metadata_serialization_shape() {
        let req = MetadataPutRequest {
            protocol_version: 1,
            expected_revision: 42,
            reported: &ReportedMetadata {
                name: Some("My PC".into()),
                platform: Some("windows".into()),
                device_type: None,
                app_id: Some("app.solstone.windows".into()),
                app_version: Some("2.0.0".into()),
            },
        };
        let json_val = serde_json::to_value(&req).unwrap();
        assert_eq!(json_val["protocol_version"], 1);
        assert_eq!(json_val["expected_revision"], 42);
        assert_eq!(json_val["reported"]["name"], "My PC");
        assert_eq!(json_val["reported"]["platform"], "windows");
        assert!(json_val["reported"]["device_type"].is_null());
        assert_eq!(json_val["reported"]["app_id"], "app.solstone.windows");
        assert_eq!(json_val["reported"]["app_version"], "2.0.0");
    }

    #[test]
    fn test_metadata_get_response_deserialization() {
        let raw = r#"{
            "protocol_version": 1,
            "revision": 3,
            "journal": {
                "version": "1.2.3"
            },
            "reported": {
                "name": "Host",
                "platform": "windows",
                "device_type": null,
                "app_id": "app.solstone.windows",
                "app_version": "2.0.0"
            }
        }"#;
        let parsed: MetadataGetResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.protocol_version, 1);
        assert_eq!(parsed.revision, 3);
        assert_eq!(
            parsed.journal.as_ref().unwrap().version.as_deref(),
            Some("1.2.3")
        );
        assert_eq!(
            parsed.reported.as_ref().unwrap().name.as_deref(),
            Some("Host")
        );
    }

    #[test]
    fn test_trigger_coalesces_when_in_flight() {
        let (controller, slot, path, _) = test_setup(false);
        let cred = slot.load().credential().clone();
        controller.begin_session(&cred);
        let controller = Arc::new(controller);

        {
            let mut state = controller.state.lock().unwrap();
            state.in_flight_metadata = true;
            state.in_flight_access = true;
        }

        controller.trigger();

        let state = controller.state.lock().unwrap();
        assert!(state.pending_metadata.is_some());
        assert_eq!(
            state.pending_metadata.as_ref().unwrap().name.as_deref(),
            Some("test-device")
        );
        assert!(state.pending_access_trigger);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
