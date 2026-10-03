// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Post-connect device metadata publication and relay-access acquisition controller.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use observer_model::about::{decode_journal_resource, normalize_version, JournalAboutFacts};
use observer_model::SyncSnapshot;
use spl_core::http::HttpResponse;

use crate::client::ClientSlot;
#[cfg(test)]
use crate::credential::FS_FAIL_POINT;
use crate::credential::{pairing_generation, CasKey, Credential, PairedState, StorageError};
use crate::device_metadata::{
    sanitize_facts, MetadataGetResponse, MetadataPutRequest, MetadataPutResponse, RawDeviceFacts,
    ReportedMetadata,
};
use crate::journal_version::{JournalVersionController, JournalVersionSessionToken};

/// Session identity for post-connect work. It is intentionally not
/// interchangeable with journal-version sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PostConnectSessionToken(pub(crate) u64);

#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
enum RelayAccessValidationError {
    #[error("envelope")]
    Envelope,
    #[error("protocol")]
    Protocol,
    #[error("identity")]
    Identity,
    #[error("origin")]
    Origin,
    #[error("claims")]
    Claims,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValidatedReadyAccess {
    pub(crate) relay_origin: String,
    pub(crate) instance_id: String,
    pub(crate) device_token: String,
    pub(crate) expires_at: i64,
}

#[derive(serde::Deserialize)]
#[serde(tag = "status", deny_unknown_fields)]
enum RelayAccessEnvelope {
    #[serde(rename = "ready")]
    Ready {
        protocol_version: u8,
        relay_origin: String,
        instance_id: String,
        device_token: String,
        expires_at: String,
    },
    #[serde(rename = "not_configured")]
    NotConfigured { protocol_version: u8 },
}

fn validate_relay_access_response(
    body: &[u8],
    paired_instance_id: &str,
    now_secs: i64,
) -> Result<Option<ValidatedReadyAccess>, RelayAccessValidationError> {
    let envelope: RelayAccessEnvelope =
        serde_json::from_slice(body).map_err(|_| RelayAccessValidationError::Envelope)?;
    match envelope {
        RelayAccessEnvelope::NotConfigured {
            protocol_version: 2,
        } => Ok(None),
        RelayAccessEnvelope::NotConfigured { .. } => Err(RelayAccessValidationError::Protocol),
        RelayAccessEnvelope::Ready {
            protocol_version,
            relay_origin,
            instance_id,
            device_token,
            expires_at,
        } => {
            if protocol_version != 2 {
                return Err(RelayAccessValidationError::Protocol);
            }
            if instance_id != paired_instance_id {
                return Err(RelayAccessValidationError::Identity);
            }
            if spl_core::relay::dial_url(&relay_origin, "relay-origin-check").is_err() {
                return Err(RelayAccessValidationError::Origin);
            }
            let access = spl_core::relay_access::RelayAccess {
                protocol_version,
                status: "ready".to_string(),
                relay_origin: relay_origin.clone(),
                instance_id,
                device_token: device_token.clone(),
                expires_at,
            };
            let claims = access
                .claims(paired_instance_id, now_secs)
                .ok_or(RelayAccessValidationError::Claims)?;
            Ok(Some(ValidatedReadyAccess {
                relay_origin,
                instance_id: paired_instance_id.to_string(),
                device_token,
                expires_at: claims.exp,
            }))
        }
    }
}

/// Default overall timeout for a post-connect job (requests + body reads).
pub const DEFAULT_POST_CONNECT_DEADLINE: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum BurstPhase {
    #[default]
    Idle,
    FirstPass,
    FollowUp,
    Quiesced,
}

#[derive(Debug, Clone)]
struct PassStart {
    token: (u64, u64, u64),
    paired_id: String,
    journal_version_token: Option<(u64, u64, u64)>,
}

#[derive(Debug, Default)]
struct AboutReconciliation {
    accepted_metadata_version: Option<String>,
    pending_about: Option<PendingAbout>,
}

#[derive(Debug)]
struct PendingAbout {
    facts: JournalAboutFacts,
    identity: (String, String),
}

#[derive(Debug, Default)]
struct PostConnectState {
    session_generation: u64,
    connection_epoch: u64,
    last_connected_epoch: Option<u64>,
    pairing_generation: u64,
    paired_instance_id: Option<String>,
    in_flight_metadata: Option<(u64, u64, u64)>,
    pending_metadata: Option<ReportedMetadata>,
    last_published_metadata: Option<ReportedMetadata>,
    in_flight_access: Option<(u64, u64, u64)>,
    pending_access_trigger: bool,
    burst_phase: BurstPhase,
    pending_durable_clear: Option<CasKey>,
}

/// Coordinates post-connect device metadata publication and relay access acquisition.
pub struct PostConnectController {
    client_slot: ClientSlot,
    state_path: Option<PathBuf>,
    journal_version: Option<Arc<JournalVersionController>>,
    journal_version_token: Mutex<Option<JournalVersionSessionToken>>,
    sync: Arc<Mutex<SyncSnapshot>>,
    facts_fn: Arc<dyn Fn() -> RawDeviceFacts + Send + Sync>,
    deadline: Duration,
    state: Mutex<PostConnectState>,
    pass_tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    #[cfg(any(test, feature = "transport-tests"))]
    last_attempt: tokio::sync::watch::Sender<Option<crate::test_completion::Attempt>>,
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
            journal_version_token: Mutex::new(None),
            sync,
            facts_fn,
            deadline: DEFAULT_POST_CONNECT_DEADLINE,
            state: Mutex::new(PostConnectState::default()),
            pass_tasks: Mutex::new(Vec::new()),
            #[cfg(any(test, feature = "transport-tests"))]
            last_attempt: tokio::sync::watch::channel(None).0,
        }
    }

    /// Quiesce active post-connect tasks and refuse follow-up passes without retiring credentials.
    pub async fn quiesce(&self) {
        {
            let mut state = self.state.lock().unwrap();
            state.burst_phase = BurstPhase::Quiesced;
            state.pending_metadata = None;
            state.pending_access_trigger = false;
        }
        let handles = {
            let mut tasks = self.pass_tasks.lock().unwrap();
            std::mem::take(&mut *tasks)
        };
        for handle in &handles {
            handle.abort();
        }
        for handle in handles {
            let _ = handle.await;
        }
    }

    /// The connection epoch of the last observed successful connection burst, if any.
    pub fn last_connected_epoch(&self) -> Option<u64> {
        self.state.lock().unwrap().last_connected_epoch
    }

    /// Set a custom execution deadline for post-connect jobs (useful in tests).
    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }

    pub(crate) fn disconnect_relay(&self) {
        self.client_slot.disable_relay();
    }

    pub(crate) fn set_journal_version_token(&self, token: JournalVersionSessionToken) {
        *self.journal_version_token.lock().unwrap() = Some(token);
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
    pub fn begin_session(&self, credential: &Credential) -> PostConnectSessionToken {
        let mut state = self.state.lock().unwrap();
        state.session_generation += 1;
        state.connection_epoch += 1;
        state.last_connected_epoch = None;
        state.pairing_generation = pairing_generation(&credential.client_cert_pem);
        state.paired_instance_id = Some(credential.instance_id.clone());
        state.in_flight_metadata = None;
        state.pending_metadata = None;
        state.last_published_metadata = None;
        state.in_flight_access = None;
        state.pending_access_trigger = false;
        state.burst_phase = BurstPhase::Idle;
        state.pending_durable_clear = None;
        PostConnectSessionToken(state.session_generation)
    }

    pub(crate) fn shutdown(&self, generation: PostConnectSessionToken) {
        if self.state.lock().unwrap().session_generation != generation.0 {
            return;
        }
        // A retired authority cannot be revived by an already-running disk worker.
        self.client_slot.retire();
        self.mark_session_disconnected(generation);
        if let (Some(jv), Some(token)) = (
            &self.journal_version,
            *self.journal_version_token.lock().unwrap(),
        ) {
            jv.mark_session_disconnected(token, &self.sync);
        }
        self.state.lock().unwrap().paired_instance_id = None;
    }

    /// Mark the connection disconnected for the given session generation, bumping epoch to fence in-flight jobs.
    pub fn mark_session_disconnected(&self, generation: PostConnectSessionToken) {
        let mut state = self.state.lock().unwrap();
        if state.session_generation == generation.0 {
            state.connection_epoch += 1;
            state.last_connected_epoch = None;
            if state
                .in_flight_metadata
                .is_some_and(|claim| claim.0 == generation.0)
            {
                state.in_flight_metadata = None;
            }
            if state
                .in_flight_access
                .is_some_and(|claim| claim.0 == generation.0)
            {
                state.in_flight_access = None;
            }
            state.burst_phase = BurstPhase::Quiesced;
        }
    }

    /// Arm one post-connect burst for the first successful dial in an epoch.
    pub fn note_connected(self: &Arc<Self>, generation: PostConnectSessionToken) {
        let should_trigger = {
            let mut state = self.state.lock().unwrap();
            if state.session_generation != generation.0
                || state.last_connected_epoch == Some(state.connection_epoch)
            {
                false
            } else {
                state.last_connected_epoch = Some(state.connection_epoch);
                true
            }
        };
        if should_trigger {
            self.trigger();
        }
    }

    /// Reset all state on unpair/re-pair.
    pub fn clear(&self) {
        let mut state = self.state.lock().unwrap();
        state.session_generation += 1;
        state.connection_epoch += 1;
        state.last_connected_epoch = None;
        state.pairing_generation = 0;
        state.paired_instance_id = None;
        state.in_flight_metadata = None;
        state.pending_metadata = None;
        state.last_published_metadata = None;
        state.in_flight_access = None;
        state.pending_access_trigger = false;
        state.burst_phase = BurstPhase::Idle;
        state.pending_durable_clear = None;
    }

    /// Trigger post-connect metadata publication and relay access acquisition.
    ///
    /// Resamples facts dynamically per trigger and starts no more than two shared passes.
    pub fn trigger(self: &Arc<Self>) {
        self.retry_pending_durable_clear_if_needed();

        let raw_facts = (self.facts_fn)();
        let sanitized = sanitize_facts(&raw_facts);

        let start = {
            let mut state = self.state.lock().unwrap();
            let Some(paired_id) = state.paired_instance_id.clone() else {
                return;
            };
            match state.burst_phase {
                BurstPhase::Idle | BurstPhase::Quiesced => {
                    state.connection_epoch = state.connection_epoch.wrapping_add(1);
                    state.last_connected_epoch = Some(state.connection_epoch);
                    let token = (
                        state.session_generation,
                        state.connection_epoch,
                        state.pairing_generation,
                    );
                    state.burst_phase = BurstPhase::FirstPass;
                    state.pending_metadata = None;
                    state.pending_access_trigger = false;
                    state.in_flight_metadata = Some(token);
                    state.in_flight_access = Some(token);
                    Some(PassStart {
                        token,
                        paired_id,
                        journal_version_token: self.journal_version_attempt_token(),
                    })
                }
                BurstPhase::FirstPass | BurstPhase::FollowUp => {
                    // Inputs arriving during an active burst are retained for one
                    // common follow-up pass. Inputs during that follow-up remain
                    // pending for the next external trigger.
                    state.pending_metadata = Some(sanitized);
                    state.pending_access_trigger = true;
                    None
                }
            }
        };

        if let Some(start) = start {
            self.start_pass(start);
        }
    }

    fn retry_pending_durable_clear_if_needed(self: &Arc<Self>) {
        if self.state.lock().unwrap().pending_durable_clear.is_none() {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move { this.retry_pending_durable_clear().await });
    }

    async fn retry_pending_durable_clear(self: &Arc<Self>) {
        let this = self.clone();
        #[cfg(test)]
        let failpoint = FS_FAIL_POINT.with(|f| f.get());
        let _ = tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            FS_FAIL_POINT.with(|f| f.set(failpoint));
            let owner = this.client_slot.publication_owner();
            let _guard = owner.lock().unwrap();
            this.retry_pending_durable_clear_owned();
            #[cfg(test)]
            FS_FAIL_POINT.with(|f| f.set(0));
        })
        .await;
    }

    fn retry_pending_durable_clear_owned(&self) {
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
                Ok(new_generation) => {
                    let mut state = self.state.lock().unwrap();
                    if state.pending_durable_clear == Some(cas_key) {
                        state.pending_durable_clear = None;
                        drop(state);
                        let mut credential = self.client_slot.load().credential().clone();
                        credential.relay_origin = None;
                        credential.device_token = None;
                        credential.device_token_expires_at = None;
                        let _ = self.client_slot.replace_from_incumbent(
                            credential,
                            CasKey {
                                pairing_generation: cas_key.pairing_generation,
                                access_mutation_generation: new_generation,
                            },
                        );
                    }
                }
                Err(StorageError::CasMismatch) => {
                    tracing::debug!(target: "sync", reason = "cas_mismatch", "durable clear retry deferred");
                }
                Err(StorageError::WriteFailed(_)) => {
                    tracing::warn!(target: "sync", reason = "write_failed", "durable clear retry failed");
                }
                Err(StorageError::DurabilityUncertain(_)) => {
                    tracing::warn!(target: "sync", reason = "durability_uncertain", "durable clear retry uncertain");
                    self.reconcile_from_disk();
                }
                Err(_) => {
                    tracing::warn!(target: "sync", reason = "storage", "durable clear retry failed");
                }
            }
        }
    }

    /// Retained processing receipt for the most recently started test pass.
    /// Capture after triggering; the ticket stays bound to that exact epoch.
    #[cfg(any(test, feature = "transport-tests"))]
    pub fn capture_attempt(&self) -> Option<crate::test_completion::Attempt> {
        self.last_attempt.borrow().clone()
    }

    /// Wait for the first test pass without relying on task scheduling or a request poll.
    #[cfg(any(test, feature = "transport-tests"))]
    pub async fn await_started_attempt(&self) -> crate::test_completion::Attempt {
        let mut receiver = self.last_attempt.subscribe();
        loop {
            if let Some(attempt) = receiver.borrow_and_update().clone() {
                return attempt;
            }
            receiver
                .changed()
                .await
                .expect("controller owns the attempt channel");
        }
    }

    fn start_pass(self: &Arc<Self>, start: PassStart) {
        #[cfg(any(test, feature = "transport-tests"))]
        let (metadata_completion, access_completion) = {
            let (attempt, metadata, access) = crate::test_completion::Attempt::new(start.token);
            self.last_attempt.send_replace(Some(attempt));
            (metadata, access)
        };
        let metadata = self.clone();
        let metadata_token = start.token;
        let metadata_journal_version_token = start.journal_version_token;
        let about_reconciliation = Arc::new(Mutex::new(AboutReconciliation::default()));
        let metadata_about_reconciliation = about_reconciliation.clone();
        let h1 = tokio::spawn(async move {
            let deadline = metadata.deadline;
            let result = tokio::time::timeout(
                deadline,
                metadata.execute_metadata_job(
                    metadata_token,
                    metadata_journal_version_token,
                    metadata_about_reconciliation,
                ),
            )
            .await;
            if result.is_err() {
                tracing::warn!(target: "sync", "post-connect metadata job timed out after {:?}", deadline);
            }
            #[cfg(any(test, feature = "transport-tests"))]
            let outcome = completion_outcome(&result, metadata.attempt_is_current(metadata_token));
            if let Some(next) = metadata.finish_pass_lane(metadata_token, true) {
                metadata.start_pass(next);
            }
            #[cfg(any(test, feature = "transport-tests"))]
            metadata_completion.finish(outcome);
        });

        let about = self.clone();
        let about_token = start.token;
        let about_journal_version_token = start.journal_version_token;
        let h3 = tokio::spawn(async move {
            let deadline = about.deadline;
            let result = tokio::time::timeout(
                deadline,
                about.execute_about_job(
                    about_token,
                    about_journal_version_token,
                    about_reconciliation,
                ),
            )
            .await;
            if result.is_err() {
                tracing::debug!(target: "sync", "post-connect about job timed out");
            }
        });

        let access = self.clone();
        let access_token = start.token;
        let h2 = tokio::spawn(async move {
            let deadline = access.deadline;
            let job = async {
                match access.fetch_access_outcome(&start.paired_id).await {
                    Ok(Some((validated, cas))) => {
                        access
                            .apply_relay_access_outcome(validated, &access_token, cas)
                            .await
                    }
                    Ok(None) => true,
                    Err(()) => false,
                }
            };
            let result = tokio::time::timeout(deadline, job).await;
            if result.is_err() {
                tracing::warn!(target: "sync", "post-connect relay access job timed out");
            }
            #[cfg(any(test, feature = "transport-tests"))]
            let outcome = completion_outcome(&result, access.attempt_is_current(access_token));
            if let Some(next) = access.finish_pass_lane(access_token, false) {
                access.start_pass(next);
            }
            #[cfg(any(test, feature = "transport-tests"))]
            access_completion.finish(outcome);
        });

        if let Ok(mut tasks) = self.pass_tasks.lock() {
            tasks.retain(|h| !h.is_finished());
            tasks.push(h1);
            tasks.push(h2);
            tasks.push(h3);
        }
    }

    /// Release a lane only when this completion owns its exact claim. The
    /// final first-pass completion alone may start one common follow-up.
    fn finish_pass_lane(&self, token: (u64, u64, u64), metadata: bool) -> Option<PassStart> {
        let mut state = self.state.lock().unwrap();
        let current_token = (
            state.session_generation,
            state.connection_epoch,
            state.pairing_generation,
        );
        if current_token != token {
            return None;
        }
        let claim = if metadata {
            &mut state.in_flight_metadata
        } else {
            &mut state.in_flight_access
        };
        if *claim != Some(token) {
            return None;
        }
        *claim = None;

        if state.in_flight_metadata.is_some() || state.in_flight_access.is_some() {
            return None;
        }

        match state.burst_phase {
            BurstPhase::FirstPass
                if state.pending_metadata.is_some() || state.pending_access_trigger =>
            {
                let paired_id = state.paired_instance_id.clone()?;
                state.connection_epoch = state.connection_epoch.wrapping_add(1);
                state.last_connected_epoch = Some(state.connection_epoch);
                let current_token = (
                    state.session_generation,
                    state.connection_epoch,
                    state.pairing_generation,
                );
                state.burst_phase = BurstPhase::FollowUp;
                state.pending_metadata = None;
                state.pending_access_trigger = false;
                state.in_flight_metadata = Some(current_token);
                state.in_flight_access = Some(current_token);
                Some(PassStart {
                    token: current_token,
                    paired_id,
                    journal_version_token: self.journal_version_attempt_token(),
                })
            }
            BurstPhase::FirstPass | BurstPhase::FollowUp => {
                // Do not let internal activity schedule a third pass. Inputs
                // recorded during the follow-up remain until an external event.
                state.burst_phase = BurstPhase::Quiesced;
                // Retire a timed-out worker that has not yet acquired publication.
                state.connection_epoch = state.connection_epoch.wrapping_add(1);
                state.last_connected_epoch = Some(state.connection_epoch);
                None
            }
            BurstPhase::Idle | BurstPhase::Quiesced => None,
        }
    }

    fn attempt_is_current(&self, token: (u64, u64, u64)) -> bool {
        if self.client_slot.is_retired() {
            return false;
        }
        let state = self.state.lock().unwrap();
        (
            state.session_generation,
            state.connection_epoch,
            state.pairing_generation,
        ) == token
    }

    fn journal_version_attempt_token(&self) -> Option<(u64, u64, u64)> {
        let (Some(journal_version), Some(session_token)) = (
            &self.journal_version,
            *self.journal_version_token.lock().unwrap(),
        ) else {
            return None;
        };
        journal_version.capture_metadata_attempt(session_token)
    }

    async fn publish_journal_metadata(
        &self,
        resource: &MetadataGetResponse,
        attempt: (u64, u64, u64),
        journal_version_token: Option<(u64, u64, u64)>,
    ) -> Option<String> {
        if !self.attempt_is_current(attempt) {
            return None;
        }
        let journal = &resource.journal;
        if let (Some(jv), Some(version_token)) = (&self.journal_version, journal_version_token) {
            let jv = jv.clone();
            let sync = self.sync.clone();
            let name = journal.name.clone();
            let version = journal.version.clone();
            // The worker retains publication ownership if the network job expires.
            let accepted = tokio::task::spawn_blocking(move || {
                jv.publish_info_for_attempt(Some(name.as_deref()), &version, version_token, &sync)
            })
            .await;
            if matches!(accepted, Ok(true)) {
                return Some(journal.version.clone());
            }
        }
        None
    }

    async fn fallback_journal_version(
        &self,
        client: &crate::ObserverClient,
        attempt: (u64, u64, u64),
        journal_version_token: Option<(u64, u64, u64)>,
    ) -> Option<String> {
        if !self.attempt_is_current(attempt) {
            return None;
        }
        let (Some(_jv), Some(version_token)) = (&self.journal_version, journal_version_token)
        else {
            return None;
        };
        if let Ok(version) = client.system_status().await {
            return self
                .publish_fallback_version(&version, attempt, Some(version_token))
                .await;
        }
        None
    }

    async fn publish_fallback_version(
        &self,
        version: &str,
        attempt: (u64, u64, u64),
        journal_version_token: Option<(u64, u64, u64)>,
    ) -> Option<String> {
        if !self.attempt_is_current(attempt) {
            return None;
        }
        let (Some(journal_version), Some(version_token)) =
            (&self.journal_version, journal_version_token)
        else {
            return None;
        };
        let journal_version = journal_version.clone();
        let sync = self.sync.clone();
        let version = version.to_owned();
        let accepted_version = version.clone();
        let accepted = tokio::task::spawn_blocking(move || {
            journal_version.publish_info_for_attempt(None, &version, version_token, &sync)
        })
        .await;
        matches!(accepted, Ok(true)).then_some(accepted_version)
    }

    async fn execute_metadata_job(
        &self,
        token: (u64, u64, u64),
        journal_version_token: Option<(u64, u64, u64)>,
        about_reconciliation: Arc<Mutex<AboutReconciliation>>,
    ) -> bool {
        let client = self.client_slot.load();
        let get_resp = match client.get_clients_self().await {
            Ok(resp) => resp,
            Err(_) => {
                tracing::debug!(target: "sync", reason = "transport", "metadata GET failed");
                return false;
            }
        };

        // Old home: 404 -> skip PUT
        if get_resp.status == 404 {
            if let Some(version) = self
                .fallback_journal_version(&client, token, journal_version_token)
                .await
            {
                self.apply_held_about_result(
                    &about_reconciliation,
                    &version,
                    token,
                    journal_version_token,
                )
                .await;
            }
            tracing::debug!(target: "sync", "metadata GET returned 404 (old home); skipping PUT");
            return true;
        }

        if get_resp.status != 200 {
            tracing::warn!(target: "sync", status = get_resp.status, "metadata GET non-success");
            return true;
        }

        let parsed: MetadataGetResponse = match serde_json::from_slice(&get_resp.body) {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!(target: "sync", reason = "invalid_response", "metadata GET rejected");
                return true;
            }
        };

        if parsed.protocol_version != 1 {
            tracing::warn!(target: "sync", ver = parsed.protocol_version, "unsupported metadata protocol version");
            return true;
        }

        if !self.attempt_is_current(token) {
            return true;
        }
        if let Some(version) = self
            .publish_journal_metadata(&parsed, token, journal_version_token)
            .await
        {
            self.apply_held_about_result(
                &about_reconciliation,
                &version,
                token,
                journal_version_token,
            )
            .await;
        }

        if !self.attempt_is_current(token) {
            return true;
        }

        let current = sanitize_facts(&(self.facts_fn)());

        // Loop prevention: check if unchanged vs server reported
        if parsed.reported.as_ref() == Some(&current) {
            if self.attempt_is_current(token) {
                self.state.lock().unwrap().last_published_metadata = Some(current.clone());
            }
            return true;
        }

        // Generation fence before PUT side-effect
        if !self.attempt_is_current(token) {
            tracing::debug!(target: "sync", "metadata PUT skipped due to stale session token");
            return true;
        }

        // Send PUT
        let put_body = match serde_json::to_vec(&MetadataPutRequest {
            protocol_version: 1,
            expected_revision: parsed.revision,
            reported: &current,
        }) {
            Ok(b) => b,
            Err(_) => return true,
        };

        let put_resp = match client.put_clients_self(&put_body).await {
            Ok(r) => r,
            Err(_) => {
                tracing::warn!(target: "sync", reason = "transport", "metadata PUT failed");
                return false;
            }
        };

        if put_resp.status == 200 {
            let parsed_put: MetadataPutResponse = match serde_json::from_slice(&put_resp.body) {
                Ok(response) => response,
                Err(_) => {
                    tracing::warn!(target: "sync", reason = "invalid_response", "metadata PUT rejected");
                    return true;
                }
            };
            if !self.attempt_is_current(token) {
                return true;
            }
            if let Some(version) = self
                .publish_journal_metadata(parsed_put.resource(), token, journal_version_token)
                .await
            {
                self.apply_held_about_result(
                    &about_reconciliation,
                    &version,
                    token,
                    journal_version_token,
                )
                .await;
            }
            let mut state = self.state.lock().unwrap();
            state.last_published_metadata = Some(current.clone());
            return true;
        }

        // HTTP 409 Conflict handling: re-read GET and retry at most once with newest pending snapshot
        if put_resp.status == 409 {
            tracing::debug!(target: "sync", "metadata PUT 409 conflict, retrying with newest snapshot");
            let retry_get = match client.get_clients_self().await {
                Ok(r) if r.status == 200 => r,
                Ok(_) => return true,
                Err(_) => return false,
            };
            let retry_parsed: MetadataGetResponse = match serde_json::from_slice(&retry_get.body) {
                Ok(p) => p,
                Err(_) => return true,
            };

            if retry_parsed.protocol_version != 1 {
                return true;
            }
            if !self.attempt_is_current(token) {
                return true;
            }
            if let Some(version) = self
                .publish_journal_metadata(&retry_parsed, token, journal_version_token)
                .await
            {
                self.apply_held_about_result(
                    &about_reconciliation,
                    &version,
                    token,
                    journal_version_token,
                )
                .await;
            }

            let newest_snapshot = {
                if !self.attempt_is_current(token) {
                    return true;
                }
                sanitize_facts(&(self.facts_fn)())
            };

            if retry_parsed.reported.as_ref() == Some(&newest_snapshot) {
                let mut state = self.state.lock().unwrap();
                state.last_published_metadata = Some(newest_snapshot);
                return true;
            }

            let retry_put_body = match serde_json::to_vec(&MetadataPutRequest {
                protocol_version: 1,
                expected_revision: retry_parsed.revision,
                reported: &newest_snapshot,
            }) {
                Ok(b) => b,
                Err(_) => return true,
            };

            let retry_put_resp = match client.put_clients_self(&retry_put_body).await {
                Ok(response) => response,
                Err(_) => return false,
            };
            {
                if retry_put_resp.status == 200 {
                    let parsed_put: MetadataPutResponse = match serde_json::from_slice(
                        &retry_put_resp.body,
                    ) {
                        Ok(response) => response,
                        Err(_) => {
                            tracing::warn!(target: "sync", reason = "invalid_response", "metadata retry PUT rejected");
                            return true;
                        }
                    };
                    if !self.attempt_is_current(token) {
                        return true;
                    }
                    if let Some(version) = self
                        .publish_journal_metadata(
                            parsed_put.resource(),
                            token,
                            journal_version_token,
                        )
                        .await
                    {
                        self.apply_held_about_result(
                            &about_reconciliation,
                            &version,
                            token,
                            journal_version_token,
                        )
                        .await;
                    }
                    let mut state = self.state.lock().unwrap();
                    state.last_published_metadata = Some(newest_snapshot);
                }
            }
        }
        true
    }

    async fn execute_about_job(
        &self,
        token: (u64, u64, u64),
        journal_version_token: Option<(u64, u64, u64)>,
        reconciliation: Arc<Mutex<AboutReconciliation>>,
    ) {
        let (Some(journal_version), Some(version_token)) =
            (&self.journal_version, journal_version_token)
        else {
            return;
        };
        let client = self.client_slot.load();
        let expected_identity = (
            client.credential().instance_id.clone(),
            crate::credential::hex_lower(&client.credential().ca_fp_prefix),
        );
        let response = client.system_about().await.ok();
        self.apply_about_response(
            response,
            expected_identity,
            token,
            version_token,
            reconciliation,
            journal_version.clone(),
        )
        .await;
    }

    async fn apply_about_response(
        &self,
        response: Option<HttpResponse>,
        expected_identity: (String, String),
        token: (u64, u64, u64),
        version_token: (u64, u64, u64),
        reconciliation: Arc<Mutex<AboutReconciliation>>,
        journal_version: Arc<JournalVersionController>,
    ) {
        let Some(response) = response else {
            return;
        };
        if response.status != 200 || !self.attempt_is_current(token) {
            return;
        }
        let Ok(facts) = decode_journal_resource(&response.body) else {
            return;
        };
        self.accept_about_result(
            &reconciliation,
            facts,
            expected_identity,
            token,
            version_token,
            journal_version,
        )
        .await;
    }

    async fn accept_about_result(
        &self,
        reconciliation: &Arc<Mutex<AboutReconciliation>>,
        facts: JournalAboutFacts,
        expected_identity: (String, String),
        attempt: (u64, u64, u64),
        journal_version_token: (u64, u64, u64),
        journal_version: Arc<JournalVersionController>,
    ) {
        let accepted_version = {
            let mut reconciliation = reconciliation.lock().unwrap();
            match reconciliation.accepted_metadata_version.clone() {
                Some(version) if normalize_version(&facts.version) == version => Some(version),
                Some(_) => {
                    reconciliation.pending_about = Some(PendingAbout {
                        facts,
                        identity: expected_identity,
                    });
                    return;
                }
                None => {
                    reconciliation.pending_about = Some(PendingAbout {
                        facts,
                        identity: expected_identity,
                    });
                    return;
                }
            }
        };
        let Some(accepted_version) = accepted_version else {
            return;
        };
        self.commit_about_facts(
            journal_version,
            facts,
            &accepted_version,
            expected_identity,
            attempt,
            journal_version_token,
        )
        .await;
    }

    /// Apply an About response that arrived before this attempt independently
    /// accepted its metadata version.
    async fn apply_held_about_result(
        &self,
        reconciliation: &Arc<Mutex<AboutReconciliation>>,
        version: &str,
        attempt: (u64, u64, u64),
        journal_version_token: Option<(u64, u64, u64)>,
    ) {
        let accepted_version = normalize_version(version).to_owned();
        let pending = {
            let mut reconciliation = reconciliation.lock().unwrap();
            reconciliation.accepted_metadata_version = Some(accepted_version.clone());
            if reconciliation
                .pending_about
                .as_ref()
                .is_some_and(|pending| {
                    normalize_version(&pending.facts.version) == accepted_version
                })
            {
                reconciliation.pending_about.take()
            } else {
                None
            }
        };
        let Some(pending) = pending else {
            return;
        };
        let (Some(journal_version), Some(version_token)) =
            (&self.journal_version, journal_version_token)
        else {
            return;
        };
        self.commit_about_facts(
            journal_version.clone(),
            pending.facts,
            &accepted_version,
            pending.identity,
            attempt,
            version_token,
        )
        .await;
    }

    async fn commit_about_facts(
        &self,
        journal_version: Arc<JournalVersionController>,
        facts: JournalAboutFacts,
        accepted_version: &str,
        expected_identity: (String, String),
        attempt: (u64, u64, u64),
        journal_version_token: (u64, u64, u64),
    ) {
        if !self.attempt_is_current(attempt) {
            return;
        }
        let credential = self.client_slot.load().credential().clone();
        if credential.instance_id != expected_identity.0
            || crate::credential::hex_lower(&credential.ca_fp_prefix) != expected_identity.1
        {
            return;
        }
        let sync = self.sync.clone();
        let accepted_version = accepted_version.to_owned();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        let _ = tokio::task::spawn_blocking(move || {
            journal_version.apply_about_for_attempt(
                facts,
                &accepted_version,
                journal_version_token,
                &expected_identity,
                &sync,
                now,
            )
        })
        .await;
    }

    async fn fetch_access_outcome(
        &self,
        paired_instance_id: &str,
    ) -> Result<Option<(Option<ValidatedReadyAccess>, CasKey)>, ()> {
        let client = self.client_slot.load();
        let captured_cas = client.current_cas_key().ok_or(())?;
        let resp = match client.get_relay_access().await {
            Ok(r) => r,
            Err(_) => {
                tracing::debug!(target: "sync", reason = "transport", "relay access GET failed");
                return Err(());
            }
        };

        // 404, 503, or other non-success -> preserve existing cache and LAN
        if resp.status != 200 {
            tracing::debug!(target: "sync", status = resp.status, "relay access GET returned non-success");
            return Ok(None);
        }

        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let validated =
            match validate_relay_access_response(&resp.body, paired_instance_id, now_secs) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(target: "sync", reason = ?e, "relay access validation failed");
                    return Ok(None);
                }
            };
        Ok(Some((validated, captured_cas)))
    }

    pub(crate) async fn apply_relay_access_outcome(
        self: &Arc<Self>,
        validated: Option<ValidatedReadyAccess>,
        token: &(u64, u64, u64),
        captured_cas: CasKey,
    ) -> bool {
        let this = self.clone();
        let token = *token;
        #[cfg(test)]
        let failpoint = FS_FAIL_POINT.with(|f| f.get());
        tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            FS_FAIL_POINT.with(|f| f.set(failpoint));
            let owner = this.client_slot.publication_owner();
            let _guard = owner.lock().unwrap();
            this.apply_relay_access_outcome_owned(validated, &token, captured_cas);
            #[cfg(test)]
            FS_FAIL_POINT.with(|f| f.set(0));
        })
        .await
        .is_ok()
    }

    fn apply_relay_access_outcome_owned(
        &self,
        validated: Option<ValidatedReadyAccess>,
        token: &(u64, u64, u64),
        captured_cas: CasKey,
    ) {
        // Generation fence before side-effects
        if self.client_slot.is_retired() {
            return;
        }
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
        if client.current_cas_key() != Some(captured_cas) {
            tracing::debug!(target: "sync", reason = "cas_changed", "relay access outcome skipped");
            return;
        }

        match validated {
            None => {
                // Fence first: a failed rebuild or disk write must never leave
                // a retired relay adapter usable.
                self.client_slot.disable_relay();
                let mut lan_cred = client.credential().clone();
                lan_cred.relay_origin = None;
                lan_cred.device_token = None;
                lan_cred.device_token_expires_at = None;
                let _ = self
                    .client_slot
                    .replace_from_incumbent(lan_cred, captured_cas);

                self.disconnect_relay();

                // Ordered durable clear
                if let Some(path) = &self.state_path {
                    let res = PairedState::mutate(path, captured_cas, |cred| {
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
                                pairing_generation: captured_cas.pairing_generation,
                                access_mutation_generation: new_gen,
                            };
                            let updated_cred = self.client_slot.load().credential().clone();
                            let _ = self
                                .client_slot
                                .replace_from_incumbent(updated_cred, new_cas);
                        }
                        Err(StorageError::CasMismatch) => {
                            tracing::debug!(target: "sync", reason = "cas_mismatch", "durable clear deferred");
                        }
                        Err(StorageError::WriteFailed(_)) => {
                            tracing::warn!(target: "sync", reason = "write_failed", "durable clear failed");
                            let mut state = self.state.lock().unwrap();
                            state.pending_durable_clear = Some(captured_cas);
                        }
                        Err(StorageError::DurabilityUncertain(_)) => {
                            tracing::warn!(target: "sync", reason = "durability_uncertain", "durable clear uncertain");
                            self.state.lock().unwrap().pending_durable_clear = Some(captured_cas);
                            self.reconcile_from_disk();
                        }
                        Err(_) => {
                            tracing::warn!(target: "sync", reason = "storage", "durable clear failed");
                        }
                    }
                }
            }
            Some(ready) => {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                if ready.expires_at <= now {
                    return;
                }
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

                let mutate_res = PairedState::mutate(path, captured_cas, |cred| {
                    if !self.attempt_is_current(*token)
                        || SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_secs() as i64 >= exp_to_save)
                            .unwrap_or(true)
                    {
                        return Err(StorageError::CasMismatch);
                    }
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

                        let new_cas = CasKey {
                            pairing_generation: captured_cas.pairing_generation,
                            access_mutation_generation: new_gen,
                        };
                        let _ = self.client_slot.replace_from_incumbent(new_cred, new_cas);
                        let mut state = self.state.lock().unwrap();
                        if state.pending_durable_clear == Some(captured_cas) {
                            state.pending_durable_clear = None;
                        }
                    }
                    Err(StorageError::DurabilityUncertain(_)) => {
                        tracing::warn!(target: "sync", reason = "durability_uncertain", "ready persist uncertain");
                        self.reconcile_from_disk();
                    }
                    Err(StorageError::WriteFailed(_)) => {
                        tracing::warn!(target: "sync", reason = "write_failed", "ready persist failed");
                    }
                    Err(StorageError::CasMismatch) => {
                        tracing::debug!(target: "sync", "ready persist skipped due to CAS mismatch");
                    }
                    Err(_) => {
                        tracing::warn!(target: "sync", reason = "storage", "ready persist failed");
                    }
                }
            }
        }
    }

    fn reconcile_from_disk(&self) {
        let Some(path) = &self.state_path else {
            return;
        };
        let Ok(state) = PairedState::load(path) else {
            tracing::warn!(target: "sync", reason = "reload", "pairing state reconciliation failed");
            return;
        };
        let Some(credential) = state.credential else {
            return;
        };
        if pairing_generation(&credential.client_cert_pem)
            != pairing_generation(&self.client_slot.load().credential().client_cert_pem)
        {
            return;
        }
        let cas = CasKey {
            pairing_generation: pairing_generation(&credential.client_cert_pem),
            access_mutation_generation: state.access_mutation_generation,
        };
        let is_clear = credential.relay_origin.is_none()
            && credential.device_token.is_none()
            && credential.device_token_expires_at.is_none();
        let _ = self.client_slot.replace_from_incumbent(credential, cas);
        let mut controller = self.state.lock().unwrap();
        if controller.pending_durable_clear.is_some_and(|pending| {
            pending.pairing_generation == cas.pairing_generation
                && pending.access_mutation_generation < cas.access_mutation_generation
        }) {
            // A visible clear still needs a confirmed durable retry. A newer
            // Ready supersedes the old clear instead of erasing that Ready.
            controller.pending_durable_clear = is_clear.then_some(cas);
        }
    }
}

#[cfg(any(test, feature = "transport-tests"))]
fn completion_outcome(
    result: &Result<bool, tokio::time::error::Elapsed>,
    current: bool,
) -> crate::test_completion::Outcome {
    use crate::test_completion::Outcome;
    if !current {
        return Outcome::Cancelled;
    }
    match result {
        Ok(true) => Outcome::Processed,
        Ok(false) => Outcome::Failed,
        Err(_) => Outcome::TimedOut,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential::{pairing_generation, EndpointAddr, PairedState, FS_FAIL_POINT};
    use crate::device_metadata::RawDeviceFacts;
    use crate::{CasKey, Credential, ObserverClient};
    use observer_model::SyncSnapshot;
    use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

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
    ) -> (
        Arc<PostConnectController>,
        ClientSlot,
        PathBuf,
        Arc<AtomicBool>,
    ) {
        let sequence = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "solstone-pc-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            sequence
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
        let client = ObserverClient::new(cred, Arc::new(std::sync::atomic::AtomicBool::new(true)))
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

        (Arc::new(controller), slot, path, disconnect_called)
    }

    fn about_resource(version: &str, os: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "protocol_version": 1,
            "version": version,
            "os": os,
            "os_version": "24.04",
            "arch": "x86_64",
            "about": "server text is ignored",
            "hostname": "PRIVATE HOST"
        }))
        .unwrap()
    }

    fn strict_metadata_body(version: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "protocol_version": 1,
            "revision": 4,
            "reported": null,
            "owner_label": null,
            "display_label": "Device",
            "updated_at": null,
            "journal": { "name": "Home", "version": version }
        }))
        .unwrap()
    }

    fn write_cached_about(path: &Path, version: &str, seen_at: u64) {
        let text = serde_json::json!({
            "instance_id": "test",
            "ca_fp_prefix_hex": "00000000000000000000000000000000",
            "version": version,
            "journal_name": "Home",
            "updated_at_epoch_secs": seen_at,
            "facts": {
                "version": version,
                "build": null,
                "os": "old-os",
                "os_version": "old-version",
                "arch": "old-arch",
                "accepted_at_epoch_secs": seen_at
            },
            "future_top_level": true
        });
        std::fs::write(path, serde_json::to_vec(&text).unwrap()).unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn completion_reports_deadline_and_stale_epoch_without_a_socket() {
        use crate::test_completion::Outcome;
        let result =
            tokio::time::timeout(Duration::from_secs(15), std::future::pending::<bool>()).await;
        assert_eq!(completion_outcome(&result, true), Outcome::TimedOut);
        assert_eq!(completion_outcome(&Ok(true), false), Outcome::Cancelled);
        assert_eq!(completion_outcome(&Ok(false), true), Outcome::Failed);
        assert_eq!(completion_outcome(&Ok(true), true), Outcome::Processed);
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
        assert_eq!(session_gen.0, 1);

        let state = controller.state.lock().unwrap();
        assert_eq!(state.session_generation, 1);
        assert_eq!(state.connection_epoch, 1);
        assert_eq!(state.pairing_generation, p_gen);
        assert_eq!(state.paired_instance_id.as_deref(), Some("test"));
        drop(state);

        // Disconnect wrong generation ignored
        controller.mark_session_disconnected(PostConnectSessionToken(99));
        assert_eq!(controller.state.lock().unwrap().connection_epoch, 1);

        // Disconnect matching generation bumps epoch
        controller.mark_session_disconnected(session_gen);
        assert_eq!(controller.state.lock().unwrap().connection_epoch, 2);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn stale_metadata_get_does_not_publish_journal_cache() {
        let (template, slot, path, _) = test_setup(false);
        let sync = template.sync.clone();
        let facts = template.facts_fn.clone();
        let journal_path = path.parent().unwrap().join("journal-version.json");
        let journal_version = Arc::new(JournalVersionController::new(journal_path.clone()));
        let controller = PostConnectController::new(
            slot.clone(),
            Some(path.clone()),
            Some(journal_version.clone()),
            sync.clone(),
            facts,
        );
        let credential = slot.load().credential().clone();
        let journal_session = journal_version.begin_session(&credential, &sync);
        controller.set_journal_version_token(journal_session);
        let session = controller.begin_session(&credential);
        let attempt = (
            session.0,
            controller.state.lock().unwrap().connection_epoch,
            pairing_generation(&credential.client_cert_pem),
        );
        let journal_token = journal_version
            .capture_metadata_attempt(journal_session)
            .unwrap();
        let resource = MetadataGetResponse {
            protocol_version: 1,
            revision: 0,
            reported: None,
            owner_label: None,
            display_label: "Device".into(),
            updated_at: None,
            journal: crate::device_metadata::JournalInfo {
                name: Some("Old Journal".into()),
                version: "1.2.3".into(),
            },
        };

        controller.mark_session_disconnected(session);
        controller
            .publish_journal_metadata(&resource, attempt, Some(journal_token))
            .await;

        assert!(!journal_path.exists());
        assert!(sync.lock().unwrap().journal_version.is_none());
        drop(template);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn about_pending_body_is_not_applied_until_strict_metadata_completes() {
        let (template, slot, path, _) = test_setup(false);
        let sync = template.sync.clone();
        let facts_fn = template.facts_fn.clone();
        let credential = slot.load().credential().clone();
        let journal_path = path.parent().unwrap().join("journal-version.json");
        let old_seen_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 600;
        write_cached_about(&journal_path, "1.2.3", old_seen_at);
        let journal_version = Arc::new(JournalVersionController::new(journal_path));
        let journal_session = journal_version.begin_session(&credential, &sync);
        let controller = Arc::new(PostConnectController::new(
            slot,
            Some(path.clone()),
            Some(journal_version.clone()),
            sync.clone(),
            facts_fn,
        ));
        controller.set_journal_version_token(journal_session);
        let post_session = controller.begin_session(&credential);
        let attempt = (
            post_session.0,
            controller.state.lock().unwrap().connection_epoch,
            pairing_generation(&credential.client_cert_pem),
        );
        let journal_attempt = journal_version
            .capture_metadata_attempt(journal_session)
            .unwrap();
        let reconciliation = Arc::new(Mutex::new(AboutReconciliation::default()));
        let identity = (
            credential.instance_id.clone(),
            crate::credential::hex_lower(&credential.ca_fp_prefix),
        );
        let about_facts = decode_journal_resource(&about_resource("v1.2.3", "ubuntu")).unwrap();
        let (release_body, pending_body) = tokio::sync::oneshot::channel::<Vec<u8>>();
        let metadata_controller = controller.clone();
        let metadata_reconciliation = reconciliation.clone();
        let metadata_job = tokio::spawn(async move {
            let body = pending_body.await.unwrap();
            let parsed: MetadataGetResponse = serde_json::from_slice(&body).unwrap();
            let version = metadata_controller
                .publish_journal_metadata(&parsed, attempt, Some(journal_attempt))
                .await
                .unwrap();
            metadata_controller
                .apply_held_about_result(
                    &metadata_reconciliation,
                    &version,
                    attempt,
                    Some(journal_attempt),
                )
                .await;
        });

        controller
            .accept_about_result(
                &reconciliation,
                about_facts,
                identity,
                attempt,
                journal_attempt,
                journal_version.clone(),
            )
            .await;
        let before = sync.lock().unwrap().clone();
        assert_eq!(before.journal_seen_at_epoch_secs, Some(old_seen_at));
        assert!(before.journal_display_line.contains("old-os"));
        assert!(!before.journal_version_fresh);
        assert!(
            !metadata_job.is_finished(),
            "strict metadata body remains held"
        );

        release_body.send(strict_metadata_body("1.2.3")).unwrap();
        metadata_job.await.unwrap();

        let after = sync.lock().unwrap().clone();
        assert!(after.journal_version_fresh);
        assert!(after.journal_seen_at_epoch_secs.unwrap() >= old_seen_at);
        assert_eq!(
            after.journal_display_line,
            "journal 1.2.3 · ubuntu 24.04 · x86_64"
        );
        assert_eq!(
            after.about_block,
            format!("{}\n{}", after.about_app_line, after.journal_display_line)
        );
        drop(template);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn matching_about_after_status_fallback_keeps_fallback_version_current() {
        let (template, slot, path, _) = test_setup(false);
        let sync = template.sync.clone();
        let facts_fn = template.facts_fn.clone();
        let credential = slot.load().credential().clone();
        let journal_path = path.parent().unwrap().join("journal-version.json");
        let old_seen_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 600;
        write_cached_about(&journal_path, "1.2.3", old_seen_at);
        let journal_version = Arc::new(JournalVersionController::new(journal_path));
        let journal_session = journal_version.begin_session(&credential, &sync);
        let controller = Arc::new(PostConnectController::new(
            slot,
            Some(path.clone()),
            Some(journal_version.clone()),
            sync.clone(),
            facts_fn,
        ));
        controller.set_journal_version_token(journal_session);
        let post_session = controller.begin_session(&credential);
        let attempt = (
            post_session.0,
            controller.state.lock().unwrap().connection_epoch,
            pairing_generation(&credential.client_cert_pem),
        );
        let journal_attempt = journal_version
            .capture_metadata_attempt(journal_session)
            .unwrap();
        let reconciliation = Arc::new(Mutex::new(AboutReconciliation::default()));

        // The strict metadata GET's 404 path accepts system_status as its fallback.
        let version = controller
            .publish_fallback_version("1.2.3", attempt, Some(journal_attempt))
            .await
            .unwrap();
        controller
            .apply_held_about_result(&reconciliation, &version, attempt, Some(journal_attempt))
            .await;
        let version_seen_at = sync.lock().unwrap().journal_seen_at_epoch_secs;
        let identity = (
            credential.instance_id.clone(),
            crate::credential::hex_lower(&credential.ca_fp_prefix),
        );
        controller
            .accept_about_result(
                &reconciliation,
                decode_journal_resource(&about_resource("v1.2.3", "ubuntu")).unwrap(),
                identity,
                attempt,
                journal_attempt,
                journal_version.clone(),
            )
            .await;

        let snapshot = sync.lock().unwrap().clone();
        assert!(snapshot.journal_version_fresh);
        assert_eq!(snapshot.journal_version.as_deref(), Some("1.2.3"));
        assert_eq!(
            snapshot.journal_display_line,
            "journal 1.2.3 · ubuntu 24.04 · x86_64"
        );
        assert_eq!(snapshot.journal_seen_at_epoch_secs, version_seen_at);
        assert!(snapshot.journal_seen_at_epoch_secs.unwrap() >= old_seen_at);
        drop(template);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn about_transport_failure_does_not_fail_metadata_acceptance() {
        let (template, slot, path, _) = test_setup(false);
        let sync = template.sync.clone();
        let facts_fn = template.facts_fn.clone();
        let credential = slot.load().credential().clone();
        let journal_path = path.parent().unwrap().join("journal-version.json");
        let journal_version = Arc::new(JournalVersionController::new(journal_path));
        let journal_session = journal_version.begin_session(&credential, &sync);
        let controller = Arc::new(PostConnectController::new(
            slot,
            Some(path.clone()),
            Some(journal_version.clone()),
            sync.clone(),
            facts_fn,
        ));
        controller.set_journal_version_token(journal_session);
        let post_session = controller.begin_session(&credential);
        let attempt = (
            post_session.0,
            controller.state.lock().unwrap().connection_epoch,
            pairing_generation(&credential.client_cert_pem),
        );
        let journal_attempt = journal_version
            .capture_metadata_attempt(journal_session)
            .unwrap();
        let reconciliation = Arc::new(Mutex::new(AboutReconciliation::default()));
        let identity = (
            credential.instance_id.clone(),
            crate::credential::hex_lower(&credential.ca_fp_prefix),
        );

        for response in [
            None,
            Some(HttpResponse {
                status: 404,
                headers: Vec::new(),
                body: Vec::new(),
            }),
            Some(HttpResponse {
                status: 200,
                headers: Vec::new(),
                body: b"{".to_vec(),
            }),
        ] {
            controller
                .apply_about_response(
                    response,
                    identity.clone(),
                    attempt,
                    journal_attempt,
                    reconciliation.clone(),
                    journal_version.clone(),
                )
                .await;
        }

        let accepted_version = controller
            .publish_fallback_version("1.2.3", attempt, Some(journal_attempt))
            .await
            .expect("metadata fallback remains independently acceptable");
        controller
            .apply_held_about_result(
                &reconciliation,
                &accepted_version,
                attempt,
                Some(journal_attempt),
            )
            .await;

        let snapshot = sync.lock().unwrap().clone();
        assert_eq!(snapshot.journal_version.as_deref(), Some("1.2.3"));
        assert!(snapshot.journal_version_fresh);
        assert_eq!(snapshot.journal_display_line, "journal 1.2.3");
        drop(template);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn test_apply_relay_access_ready() {
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

        let cas = slot.load().current_cas_key().unwrap();
        controller
            .apply_relay_access_outcome(Some(ready), &token, cas)
            .await;

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

    #[tokio::test]
    async fn test_ready_write_failed_keeps_old_tuple_until_current_retry() {
        let (controller, slot, path, _) = test_setup(false);
        let cred = slot.load().credential().clone();
        let pairing = pairing_generation(&cred.client_cert_pem);
        controller.begin_session(&cred);
        let token = (1, 1, pairing);
        let cas = slot.load().current_cas_key().unwrap();
        let ready = ValidatedReadyAccess {
            relay_origin: "https://relay.new.org".into(),
            instance_id: "test".into(),
            device_token: "jwt-token-new".into(),
            expires_at: 1_800_000_000,
        };

        FS_FAIL_POINT.with(|f| f.set(1));
        controller
            .apply_relay_access_outcome(Some(ready.clone()), &token, cas)
            .await;
        FS_FAIL_POINT.with(|f| f.set(0));

        assert!(slot.load().credential().relay_origin.is_none());
        assert_eq!(slot.load().current_cas_key(), Some(cas));
        let disk = PairedState::load(&path).unwrap();
        assert_eq!(disk.access_mutation_generation, 0);
        assert!(disk.credential.unwrap().relay_origin.is_none());

        controller
            .apply_relay_access_outcome(Some(ready), &token, cas)
            .await;
        assert_eq!(
            slot.load().credential().relay_origin.as_deref(),
            Some("https://relay.new.org")
        );
        assert_eq!(
            slot.load()
                .current_cas_key()
                .unwrap()
                .access_mutation_generation,
            1
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn test_apply_relay_access_not_configured() {
        let (controller, slot, path, _disconnect_called) = test_setup(true);
        let cred = slot.load().credential().clone();
        let p_gen = pairing_generation(&cred.client_cert_pem);
        controller.begin_session(&cred);
        let token = (1, 1, p_gen);

        let cas = slot.load().current_cas_key().unwrap();
        controller
            .apply_relay_access_outcome(None, &token, cas)
            .await;

        // Client slot immediately swapped to LAN-only
        let active_client = slot.load();
        let active_cred = active_client.credential();
        assert_eq!(active_cred.relay_origin, None);
        assert_eq!(active_cred.device_token, None);
        assert_eq!(active_cred.device_token_expires_at, None);

        // The shared relay fence prevents future bridge relay opens.
        assert!(!slot.is_current_incarnation());

        // Disk state cleared
        let loaded = PairedState::load(&path).unwrap();
        assert_eq!(loaded.access_mutation_generation, 1);
        let disk_cred = loaded.credential.unwrap();
        assert_eq!(disk_cred.relay_origin, None);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn test_clear_write_failed_fence_retries_the_same_current_clear() {
        let (controller, slot, path, _) = test_setup(true);
        let controller = Arc::new(controller);
        let cred = slot.load().credential().clone();
        let pairing = pairing_generation(&cred.client_cert_pem);
        controller.begin_session(&cred);
        let token = (1, 1, pairing);
        let cas = slot.load().current_cas_key().unwrap();

        FS_FAIL_POINT.with(|f| f.set(1));
        controller
            .apply_relay_access_outcome(None, &token, cas)
            .await;
        FS_FAIL_POINT.with(|f| f.set(0));

        assert!(!slot.is_current_incarnation());
        assert_eq!(controller.pending_durable_clear(), Some(cas));
        assert_eq!(
            PairedState::load(&path).unwrap().access_mutation_generation,
            0
        );

        controller.retry_pending_durable_clear().await;

        assert_eq!(controller.pending_durable_clear(), None);
        assert_eq!(
            slot.load()
                .current_cas_key()
                .unwrap()
                .access_mutation_generation,
            1
        );
        let disk = PairedState::load(&path).unwrap();
        assert_eq!(disk.access_mutation_generation, 1);
        assert!(disk.credential.unwrap().relay_origin.is_none());

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn test_apply_relay_access_durability_uncertain_ready_reconciles_live_state() {
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

        let cas = slot.load().current_cas_key().unwrap();
        controller
            .apply_relay_access_outcome(Some(ready), &token, cas)
            .await;
        FS_FAIL_POINT.with(|f| f.set(0));

        // The published rename is reconciled from disk.
        let active_client = slot.load();
        assert_eq!(
            active_client.credential().relay_origin.as_deref(),
            Some("https://relay.new.org")
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn test_apply_relay_access_durability_uncertain_not_configured_reconciles_clear() {
        let (controller, slot, path, _disconnect_called) = test_setup(true);
        let cred = slot.load().credential().clone();
        let p_gen = pairing_generation(&cred.client_cert_pem);
        controller.begin_session(&cred);
        let token = (1, 1, p_gen);

        FS_FAIL_POINT.with(|f| f.set(2));

        let cas = slot.load().current_cas_key().unwrap();
        controller
            .apply_relay_access_outcome(None, &token, cas)
            .await;
        FS_FAIL_POINT.with(|f| f.set(0));

        // Live replace still occurred for LAN safety
        let active_client = slot.load();
        assert_eq!(active_client.credential().relay_origin, None);
        assert!(!slot.is_current_incarnation());

        assert_eq!(
            controller.pending_durable_clear(),
            active_client.current_cas_key()
        );
        controller.retry_pending_durable_clear().await;
        assert_eq!(controller.pending_durable_clear(), None);
        assert_eq!(
            slot.load()
                .current_cas_key()
                .unwrap()
                .access_mutation_generation,
            2
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn test_stale_token_fenced_from_applying_ready_or_not_configured() {
        let (controller, slot, path, disconnect_called) = test_setup(false);
        let cred = slot.load().credential().clone();
        let p_gen = pairing_generation(&cred.client_cert_pem);
        controller.begin_session(&cred);
        let stale_token = (1, 1, p_gen);

        // Disconnect bumps epoch to 2
        controller.mark_session_disconnected(PostConnectSessionToken(1));

        let ready = ValidatedReadyAccess {
            relay_origin: "https://relay.stale.org".into(),
            instance_id: "test".into(),
            device_token: "jwt-token-stale".into(),
            expires_at: 1800000000,
        };

        // Stale apply ready is a no-op
        let cas = slot.load().current_cas_key().unwrap();
        controller
            .apply_relay_access_outcome(Some(ready), &stale_token, cas)
            .await;
        assert_eq!(slot.load().credential().relay_origin, None);

        // Stale apply not_configured is a no-op
        controller
            .apply_relay_access_outcome(None, &stale_token, cas)
            .await;
        assert!(!disconnect_called.load(Ordering::SeqCst));

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn test_durable_clear_retry_cas_mismatch_does_not_clear_newer_ready() {
        let (controller, slot, path, _) = test_setup(true);
        let controller = Arc::new(controller);
        let cred = slot.load().credential().clone();
        let p_gen = pairing_generation(&cred.client_cert_pem);
        controller.begin_session(&cred);
        let token = (1, 1, p_gen);

        // 1. Simulate pre-rename write failure on not_configured at gen 0
        FS_FAIL_POINT.with(|f| f.set(1));
        let cas = slot.load().current_cas_key().unwrap();
        controller
            .apply_relay_access_outcome(None, &token, cas)
            .await;
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
        controller
            .apply_relay_access_outcome(Some(ready), &token, cas)
            .await;

        let loaded = PairedState::load(&path).unwrap();
        assert_eq!(loaded.access_mutation_generation, 1);
        assert_eq!(
            loaded.credential.as_ref().unwrap().relay_origin.as_deref(),
            Some("https://relay.new.org")
        );

        // 3. A newer Ready supersedes only the matching pending clear.
        controller.retry_pending_durable_clear_if_needed();
        tokio::task::yield_now().await;

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
            "owner_label": null,
            "display_label": "Device",
            "updated_at": null,
            "journal": {
                "name": "Home",
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
        assert_eq!(parsed.journal.version.as_str(), "1.2.3");
        assert_eq!(
            parsed.reported.as_ref().unwrap().name.as_deref(),
            Some("Host")
        );
    }

    fn relay_access_body(token: String, expires_at: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "status": "ready",
            "protocol_version": 2,
            "relay_origin": "https://relay.example.com",
            "instance_id": "test",
            "device_token": token,
            "expires_at": expires_at,
        }))
        .unwrap()
    }

    fn relay_access_token(iat: i64, exp: i64, jti: &str) -> String {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};

        let claims = serde_json::json!({
            "iss": "https://relay.example.com",
            "sub": "instance:test",
            "aud": "spl-relay",
            "scope": "session.dial",
            "ver": 2,
            "instance_id": "test",
            "iat": iat,
            "exp": exp,
            "jti": jti,
        });
        format!(
            "e30.{}.sig",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
        )
    }

    #[test]
    fn test_relay_access_validator_rejects_strict_invalid_envelopes_without_replacing_live_access()
    {
        let (controller, slot, path, _) = test_setup(true);
        let original = slot.load().credential().clone();
        let valid_token = relay_access_token(100, 200, "jti");

        let extra_not_configured =
            br#"{"status":"not_configured","protocol_version":2,"extra":true}"#;
        assert_eq!(
            validate_relay_access_response(extra_not_configured, "test", 150),
            Err(RelayAccessValidationError::Envelope)
        );
        assert_eq!(
            validate_relay_access_response(
                &relay_access_body(relay_access_token(100, 200, ""), "1970-01-01T00:03:20Z"),
                "test",
                150,
            ),
            Err(RelayAccessValidationError::Claims)
        );
        assert_eq!(
            validate_relay_access_response(
                &relay_access_body(valid_token.clone(), "1970-01-01T00:03:20.001Z"),
                "test",
                150,
            ),
            Err(RelayAccessValidationError::Claims)
        );
        assert_eq!(
            validate_relay_access_response(
                &relay_access_body(relay_access_token(211, 300, "jti"), "1970-01-01T00:05:00Z"),
                "test",
                150,
            ),
            Err(RelayAccessValidationError::Claims)
        );

        // Validation failures never reach apply, so the usable live access stays intact.
        assert_eq!(slot.load().credential(), &original);
        drop(controller);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn test_trigger_coalesces_when_in_flight() {
        let (controller, slot, path, _) = test_setup(false);
        let cred = slot.load().credential().clone();
        controller.begin_session(&cred);
        let controller = Arc::new(controller);

        {
            let mut state = controller.state.lock().unwrap();
            let claim = (1, 1, pairing_generation(&cred.client_cert_pem));
            state.in_flight_metadata = Some(claim);
            state.in_flight_access = Some(claim);
            state.burst_phase = BurstPhase::FirstPass;
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

    #[cfg(feature = "transport-tests")]
    #[tokio::test]
    async fn test_shared_burst_allows_one_follow_up_then_requires_external_trigger() {
        let (controller, slot, path, _) = test_setup(false);
        let cred = slot.load().credential().clone();
        let session = controller.begin_session(&cred);
        let controller = Arc::new(controller);
        let claim = (1, 1, pairing_generation(&cred.client_cert_pem));

        {
            let mut state = controller.state.lock().unwrap();
            state.burst_phase = BurstPhase::FirstPass;
            state.in_flight_metadata = Some(claim);
            state.in_flight_access = Some(claim);
            // Access work arrives while metadata is still in the first pass.
            state.pending_access_trigger = true;
        }

        assert!(controller.finish_pass_lane(claim, true).is_none());
        let follow_up = controller
            .finish_pass_lane(claim, false)
            .expect("first-pass input must receive one common follow-up");
        assert_ne!(follow_up.token, claim);
        // A stale first-pass completion cannot release the successor lane.
        assert!(controller.finish_pass_lane(claim, false).is_none());
        let claim = follow_up.token;
        {
            let state = controller.state.lock().unwrap();
            assert_eq!(state.burst_phase, BurstPhase::FollowUp);
            assert_eq!(state.in_flight_metadata, Some(claim));
            assert_eq!(state.in_flight_access, Some(claim));
            // A finished access lane cannot skip its still-running metadata peer.
        }

        {
            let mut state = controller.state.lock().unwrap();
            state.pending_metadata = Some(ReportedMetadata::default());
        }
        assert!(controller.finish_pass_lane(claim, false).is_none());
        assert_eq!(
            controller.state.lock().unwrap().in_flight_metadata,
            Some(claim)
        );
        assert!(controller.finish_pass_lane(claim, true).is_none());
        {
            let state = controller.state.lock().unwrap();
            assert_eq!(state.burst_phase, BurstPhase::Quiesced);
            assert!(state.pending_metadata.is_some());
            assert!(state.in_flight_metadata.is_none());
            assert!(state.in_flight_access.is_none());
        }

        // A duplicate successful dial in the already-noted epoch cannot start
        // another burst, while an explicit trigger after quiescence can.
        {
            let mut state = controller.state.lock().unwrap();
            state.last_connected_epoch = Some(state.connection_epoch);
        }
        controller.note_connected(session);
        assert_eq!(
            controller.state.lock().unwrap().burst_phase,
            BurstPhase::Quiesced
        );
        controller.trigger();
        assert_eq!(
            controller.state.lock().unwrap().burst_phase,
            BurstPhase::FirstPass
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn quiesce_aborts_active_pass_tasks_and_returns_promptly() {
        let (controller, _slot, path, _called) = test_setup(false);
        let forever_task = tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(1000)).await;
        });
        controller.pass_tasks.lock().unwrap().push(forever_task);

        let started = tokio::time::Instant::now();
        controller.quiesce().await;
        assert!(started.elapsed() < Duration::from_secs(2));

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
