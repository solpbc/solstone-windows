// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Restartable retirement of a credential that must never become an active
//! pairing, such as an integration pairing rejected by its mark check.

use std::future::Future;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use crate::credential::{
    pairing_generation, Credential, PairedState, RetirementAnswerDisposition, RetirementIntent,
    RetirementOperation, RetirementPhase, StorageError,
};
use crate::device_marker::MarkerResult;
use crate::{answer, migration};
use crate::{ObserverClient, TransportError};

const RETIREMENT_SCHEMA: u32 = 1;
const RETIRE_CLIENT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileOutcome {
    None,
    Completed,
    Superseded,
}

/// Stage a wrong-mark candidate durably, then issue its idempotent retirement.
/// No transport callback is invoked unless the protected intent and its bytes
/// have been published and read back successfully.
pub async fn retire_wrong_mark_with<F, Fut>(
    state_path: &Path,
    candidate: Credential,
    retire: F,
) -> Result<(), TransportError>
where
    F: FnOnce(Credential, String) -> Fut,
    Fut: Future<Output = Result<(), TransportError>>,
{
    let intent = new_wrong_mark_intent(candidate)?;
    PairedState::install_retirement_intent(state_path, intent.clone())?;
    reconcile_intent_with(state_path, intent, retire).await?;
    Ok(())
}

/// Durably replace a current GUI pairing after the new pairing response has
/// been authenticated and validated. The callback that quiesces old sends runs
/// only after the protected candidate intent has passed durable readback.
pub async fn replace_pair_with<B, BFut, F, Fut>(
    state_path: &Path,
    candidate: Credential,
    marker_result: MarkerResult,
    before_retire: B,
    retire: F,
) -> Result<ReconcileOutcome, TransportError>
where
    B: FnOnce() -> BFut,
    BFut: Future<Output = ()>,
    F: FnOnce(Credential, String) -> Fut,
    Fut: Future<Output = Result<(), TransportError>>,
{
    let intent = new_pair_replacement_intent(state_path, candidate, marker_result)?;
    if let Err(error) = PairedState::install_pair_replacement_intent(
        state_path,
        intent.owner_generation,
        intent.access_mutation_generation,
        intent.clone(),
    ) {
        match PairedState::rollback_unretired_pair_replacement(
            state_path,
            &intent.operation_id,
            intent.owner_generation,
            intent.candidate_generation,
        ) {
            Ok(_) => return Err(error.into()),
            Err(_) => return Err(TransportError::CredentialRecoveryRequired),
        }
    }
    before_retire().await;
    reconcile_intent_with(state_path, intent, retire).await
}

/// Reconcile a saved intent before migration, pairing admission, or any send.
pub async fn reconcile_on_launch(state_path: &Path) -> Result<ReconcileOutcome, TransportError> {
    stage_legacy_rejection(state_path)?;
    reconcile_with(state_path, retire_credential).await
}

/// Convert a legacy rejected digest into the same typed transaction before
/// launch can bind access or admit migration/send work.
fn stage_legacy_rejection(state_path: &Path) -> Result<(), TransportError> {
    let answer_path = answer::answer_path(state_path);
    let Some(mut answer_state) = answer::read_answer(&answer_path)? else {
        return Ok(());
    };
    if answer_state.rejected.is_empty() {
        return Ok(());
    }
    let paired = PairedState::load(state_path)?;
    if paired.retirement_intent.is_some() {
        return Ok(());
    }
    let Some(credential) = paired.credential.clone() else {
        answer_state.rejected.clear();
        answer::write_answer(&answer_path, &answer_state)?;
        return Ok(());
    };
    let binding = crate::ack::JournalIdentity::from_credential(&credential).client_cert_sha256;
    if answer_state.rejected != binding {
        answer_state.rejected.clear();
        answer::write_answer(&answer_path, &answer_state)?;
        return Ok(());
    }
    let intent = new_pair_rejection_intent(&paired, credential)?;
    PairedState::install_pair_rejection_intent(
        state_path,
        intent.owner_generation,
        intent.access_mutation_generation,
        intent,
    )?;
    Ok(())
}

/// Persist and reconcile an explicit cancel/rejection. The callback that stops
/// the current uploader runs only after the exact pairing/access generation
/// intent has been durably read back.
pub async fn reject_pair_with<B, BFut, F, Fut>(
    state_path: &Path,
    binding: &str,
    before_retire: B,
    retire: F,
) -> Result<ReconcileOutcome, TransportError>
where
    B: FnOnce() -> BFut,
    BFut: Future<Output = Result<(), TransportError>>,
    F: FnOnce(Credential, String) -> Fut,
    Fut: Future<Output = Result<(), TransportError>>,
{
    let paired = PairedState::load(state_path)?;
    let credential = paired.credential.clone().ok_or(TransportError::NotPaired)?;
    if crate::ack::JournalIdentity::from_credential(&credential).client_cert_sha256 != binding {
        return Ok(ReconcileOutcome::Superseded);
    }
    let intent = new_pair_rejection_intent(&paired, credential)?;
    PairedState::install_pair_rejection_intent(
        state_path,
        intent.owner_generation,
        intent.access_mutation_generation,
        intent.clone(),
    )?;
    before_retire().await?;
    reconcile_intent_with(state_path, intent, retire).await
}

pub(crate) async fn retire_credential(
    credential: Credential,
    client_id: String,
) -> Result<(), TransportError> {
    let client = ObserverClient::new(credential, Arc::new(AtomicBool::new(false)))?;
    tokio::time::timeout(RETIRE_CLIENT_TIMEOUT, client.retire_client(&client_id))
        .await
        .map_err(|_| {
            TransportError::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "client retirement timed out",
            ))
        })?
}

/// Injected reconciliation seam used by the composed integration and restart
/// tests; the production path above supplies the real observer client.
pub async fn reconcile_with<F, Fut>(
    state_path: &Path,
    retire: F,
) -> Result<ReconcileOutcome, TransportError>
where
    F: FnOnce(Credential, String) -> Fut,
    Fut: Future<Output = Result<(), TransportError>>,
{
    PairedState::recover_staged_retirement_intent(state_path)?;
    let paired = PairedState::load(state_path)?;
    let Some(intent) = paired.retirement_intent else {
        return Ok(ReconcileOutcome::None);
    };
    reconcile_intent_with(state_path, intent, retire).await
}

async fn reconcile_intent_with<F, Fut>(
    state_path: &Path,
    mut intent: RetirementIntent,
    retire: F,
) -> Result<ReconcileOutcome, TransportError>
where
    F: FnOnce(Credential, String) -> Fut,
    Fut: Future<Output = Result<(), TransportError>>,
{
    validate_intent(&intent, state_path)?;
    if intent.phase != RetirementPhase::Succeeded {
        let paired = PairedState::load(state_path)?;
        let retiring_credential = match intent.operation {
            RetirementOperation::IntegrationWrongMark => intent.candidate.clone(),
            RetirementOperation::GuiPairReplacement => {
                let incumbent = paired
                    .credential
                    .ok_or(TransportError::CredentialMalformed)?;
                if pairing_generation(&incumbent.client_cert_pem) != intent.owner_generation {
                    return Ok(ReconcileOutcome::Superseded);
                }
                incumbent
            }
            RetirementOperation::GuiPairRejection => {
                let incumbent = paired
                    .credential
                    .ok_or(TransportError::CredentialMalformed)?;
                if pairing_generation(&incumbent.client_cert_pem) != intent.owner_generation
                    || paired.access_mutation_generation != intent.access_mutation_generation
                {
                    return Ok(ReconcileOutcome::Superseded);
                }
                incumbent
            }
        };
        match retire(retiring_credential, intent.client_id.clone()).await {
            Ok(()) => {
                if !set_phase_or_superseded(state_path, &intent, RetirementPhase::Succeeded)? {
                    return Ok(ReconcileOutcome::Superseded);
                }
                intent.phase = RetirementPhase::Succeeded;
            }
            Err(error) => {
                let _ = set_phase_or_superseded(state_path, &intent, RetirementPhase::Unknown)?;
                return Err(error);
            }
        }
    }

    match intent.operation {
        RetirementOperation::IntegrationWrongMark => {
            if !matches_owner_generation(state_path, &intent)? {
                return Ok(ReconcileOutcome::Superseded);
            }
            if PairedState::remove_retired_intent(
                state_path,
                &intent.operation_id,
                intent.owner_generation,
            )? {
                Ok(ReconcileOutcome::Completed)
            } else {
                Ok(ReconcileOutcome::Superseded)
            }
        }
        RetirementOperation::GuiPairReplacement => finish_pair_replacement(state_path, &intent),
        RetirementOperation::GuiPairRejection => finish_pair_rejection(state_path, &intent),
    }
}

fn finish_pair_rejection(
    state_path: &Path,
    intent: &RetirementIntent,
) -> Result<ReconcileOutcome, TransportError> {
    if intent.answer_disposition != Some(RetirementAnswerDisposition::ClearRejectedPairing) {
        return Err(TransportError::CredentialMalformed);
    }
    let binding =
        crate::ack::JournalIdentity::from_credential(&intent.candidate).client_cert_sha256;
    let completed = PairedState::finish_pair_rejection(
        state_path,
        &intent.operation_id,
        intent.owner_generation,
        intent.access_mutation_generation,
        || {
            let answer_path = answer::answer_path(state_path);
            let current = answer::read_answer(&answer_path)?.unwrap_or_default();
            if (!current.confirmed.is_empty() && current.confirmed != binding)
                || (!current.rejected.is_empty() && current.rejected != binding)
            {
                return Err(StorageError::CasMismatch);
            }
            answer::write_answer_with_owner_lock(&answer_path, &answer::AnswerState::default())
        },
    )?;
    Ok(if completed {
        ReconcileOutcome::Completed
    } else {
        ReconcileOutcome::Superseded
    })
}

fn finish_pair_replacement(
    state_path: &Path,
    intent: &RetirementIntent,
) -> Result<ReconcileOutcome, TransportError> {
    let candidate_generation = intent.candidate_generation;
    let paired = PairedState::commit_pair_replacement_candidate(
        state_path,
        &intent.operation_id,
        intent.owner_generation,
        candidate_generation,
    )?;
    let Some(candidate) = paired.credential.as_ref() else {
        return Ok(ReconcileOutcome::Superseded);
    };
    if pairing_generation(&candidate.client_cert_pem) != candidate_generation {
        return Ok(ReconcileOutcome::Superseded);
    }
    #[cfg(test)]
    if crate::credential::PAIR_REPLACEMENT_FAIL_POINT.with(|fail| fail.replace(false)) {
        return Err(TransportError::CredentialRecoveryRequired);
    }
    match intent.answer_disposition {
        Some(RetirementAnswerDisposition::ResetForCandidate) => {
            answer::write_answer(
                &answer::answer_path(state_path),
                &answer::AnswerState::default(),
            )
            .map_err(TransportError::from)?;
        }
        Some(RetirementAnswerDisposition::ClearRejectedPairing) => {
            return Err(TransportError::CredentialMalformed);
        }
        None => return Err(TransportError::CredentialMalformed),
    }
    let marker = intent
        .marker_result
        .as_ref()
        .ok_or(TransportError::CredentialMalformed)?;
    migration::record_pair_replacement_offer(state_path, candidate, marker)?;
    if PairedState::clear_pair_replacement_intent(
        state_path,
        &intent.operation_id,
        intent.owner_generation,
        candidate_generation,
    )? {
        Ok(ReconcileOutcome::Completed)
    } else {
        Ok(ReconcileOutcome::Superseded)
    }
}

fn set_phase_or_superseded(
    state_path: &Path,
    intent: &RetirementIntent,
    phase: RetirementPhase,
) -> Result<bool, TransportError> {
    match PairedState::update_retirement_phase(
        state_path,
        &intent.operation_id,
        intent.owner_generation,
        intent.phase,
        phase,
    ) {
        Ok(()) => Ok(true),
        Err(StorageError::CasMismatch) => {
            if matches_owner_generation(state_path, intent)? {
                Err(TransportError::ReplayUnsafe)
            } else {
                Ok(false)
            }
        }
        Err(error) => Err(error.into()),
    }
}

fn matches_owner_generation(
    state_path: &Path,
    intent: &RetirementIntent,
) -> Result<bool, TransportError> {
    let state = PairedState::load(state_path)?;
    Ok(state.retirement_intent.as_ref().is_some_and(|current| {
        current.operation_id == intent.operation_id
            && current.owner_generation == intent.owner_generation
            && current.schema == RETIREMENT_SCHEMA
            && current.operation == intent.operation
            && current.candidate_generation == intent.candidate_generation
            && current.access_mutation_generation == intent.access_mutation_generation
    }) && match intent.operation {
        RetirementOperation::IntegrationWrongMark => {
            state.credential.is_none() && intent.access_mutation_generation == 0
        }
        RetirementOperation::GuiPairReplacement => {
            state.credential.as_ref().is_some_and(|credential| {
                let generation = pairing_generation(&credential.client_cert_pem);
                generation == intent.owner_generation
                    || (generation == intent.candidate_generation
                        && intent.phase == RetirementPhase::Succeeded)
            })
        }
        RetirementOperation::GuiPairRejection => {
            state.credential.as_ref().is_some_and(|credential| {
                pairing_generation(&credential.client_cert_pem) == intent.owner_generation
                    && state.access_mutation_generation == intent.access_mutation_generation
            })
        }
    })
}

fn validate_intent(intent: &RetirementIntent, state_path: &Path) -> Result<(), TransportError> {
    let paired = PairedState::load(state_path)?;
    let candidate_generation = if intent.candidate_generation == [0; 32]
        && intent.operation == RetirementOperation::IntegrationWrongMark
    {
        intent.owner_generation
    } else {
        intent.candidate_generation
    };
    if intent.schema != RETIREMENT_SCHEMA
        || intent.operation_id.is_empty()
        || intent.client_id.is_empty()
        || pairing_generation(&intent.candidate.client_cert_pem) != candidate_generation
    {
        return Err(TransportError::CredentialMalformed);
    }
    match intent.operation {
        RetirementOperation::IntegrationWrongMark => {
            let expected_client_id = client_id_for_credential(&intent.candidate)?;
            if intent.owner_generation != candidate_generation
                || intent.access_mutation_generation != 0
                || (paired.credential.is_some() && intent.phase != RetirementPhase::Succeeded)
                || expected_client_id != intent.client_id
            {
                return Err(TransportError::CredentialMalformed);
            }
        }
        RetirementOperation::GuiPairReplacement => {
            let incumbent = paired
                .credential
                .as_ref()
                .ok_or(TransportError::CredentialMalformed)?;
            let incumbent_generation = pairing_generation(&incumbent.client_cert_pem);
            let candidate_is_current = incumbent_generation == candidate_generation;
            if intent.answer_disposition != Some(RetirementAnswerDisposition::ResetForCandidate)
                || intent.marker_result.is_none()
                || (candidate_is_current && intent.phase != RetirementPhase::Succeeded)
                || (!candidate_is_current && incumbent_generation != intent.owner_generation)
                || (!candidate_is_current
                    && paired.access_mutation_generation != intent.access_mutation_generation)
                || (!candidate_is_current
                    && client_id_for_credential(incumbent)? != intent.client_id)
            {
                return Err(TransportError::CredentialMalformed);
            }
        }
        RetirementOperation::GuiPairRejection => {
            let incumbent = paired
                .credential
                .as_ref()
                .ok_or(TransportError::CredentialMalformed)?;
            if pairing_generation(&incumbent.client_cert_pem) != intent.owner_generation
                || intent.candidate_generation != intent.owner_generation
                || pairing_generation(&intent.candidate.client_cert_pem) != intent.owner_generation
                || intent.access_mutation_generation != paired.access_mutation_generation
                || client_id_for_credential(incumbent)? != intent.client_id
                || intent.answer_disposition
                    != Some(RetirementAnswerDisposition::ClearRejectedPairing)
                || intent.marker_result.is_some()
            {
                return Err(TransportError::CredentialMalformed);
            }
        }
    }
    Ok(())
}

fn new_wrong_mark_intent(candidate: Credential) -> Result<RetirementIntent, TransportError> {
    let owner_generation = pairing_generation(&candidate.client_cert_pem);
    let client_id = client_id_for_credential(&candidate)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| TransportError::CredentialMalformed)?
        .as_nanos();
    let operation_id = spl_core::ca::sha256_hex(
        format!("{}:{}:{nonce}", std::process::id(), hex(&owner_generation)).as_bytes(),
    );
    Ok(RetirementIntent {
        schema: RETIREMENT_SCHEMA,
        operation_id,
        operation: RetirementOperation::IntegrationWrongMark,
        phase: RetirementPhase::Prepared,
        owner_generation,
        access_mutation_generation: 0,
        client_id,
        candidate,
        candidate_generation: owner_generation,
        answer_disposition: None,
        marker_result: None,
    })
}

fn new_pair_replacement_intent(
    state_path: &Path,
    candidate: Credential,
    marker_result: MarkerResult,
) -> Result<RetirementIntent, TransportError> {
    let paired = PairedState::load(state_path)?;
    if paired.retirement_intent.is_some() {
        return Err(TransportError::ReplayUnsafe);
    }
    let incumbent = paired.credential.ok_or(TransportError::NotPaired)?;
    let owner_generation = pairing_generation(&incumbent.client_cert_pem);
    let candidate_generation = pairing_generation(&candidate.client_cert_pem);
    if owner_generation == candidate_generation {
        return Err(TransportError::CredentialMalformed);
    }
    let client_id = client_id_for_credential(&incumbent)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| TransportError::CredentialMalformed)?
        .as_nanos();
    let operation_id = spl_core::ca::sha256_hex(
        format!("{}:{}:{nonce}", std::process::id(), hex(&owner_generation)).as_bytes(),
    );
    Ok(RetirementIntent {
        schema: RETIREMENT_SCHEMA,
        operation_id,
        operation: RetirementOperation::GuiPairReplacement,
        phase: RetirementPhase::Prepared,
        owner_generation,
        candidate_generation,
        access_mutation_generation: paired.access_mutation_generation,
        client_id,
        candidate,
        answer_disposition: Some(RetirementAnswerDisposition::ResetForCandidate),
        marker_result: Some(marker_result),
    })
}

fn new_pair_rejection_intent(
    paired: &PairedState,
    credential: Credential,
) -> Result<RetirementIntent, TransportError> {
    let owner_generation = pairing_generation(&credential.client_cert_pem);
    let client_id = client_id_for_credential(&credential)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| TransportError::CredentialMalformed)?
        .as_nanos();
    let operation_id = spl_core::ca::sha256_hex(
        format!("{}:{}:{nonce}", std::process::id(), hex(&owner_generation)).as_bytes(),
    );
    Ok(RetirementIntent {
        schema: RETIREMENT_SCHEMA,
        operation_id,
        operation: RetirementOperation::GuiPairRejection,
        phase: RetirementPhase::Prepared,
        owner_generation,
        candidate_generation: owner_generation,
        access_mutation_generation: paired.access_mutation_generation,
        client_id,
        candidate: credential,
        answer_disposition: Some(RetirementAnswerDisposition::ClearRejectedPairing),
        marker_result: None,
    })
}

fn client_id_for_credential(credential: &Credential) -> Result<String, TransportError> {
    let certs = spl_transport::tls::parse_certs(&credential.client_cert_pem)
        .map_err(|_| TransportError::CredentialMalformed)?;
    let cert = certs.first().ok_or(TransportError::CredentialMalformed)?;
    Ok(format!(
        "sha256:{}",
        spl_core::ca::sha256_hex(cert.as_ref())
    ))
}

fn hex(value: &[u8]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential::{
        FS_FAIL_POINT, PAIR_REJECTION_CLEANUP_FAIL_POINT, PAIR_REPLACEMENT_FAIL_POINT,
        RETIREMENT_CLEANUP_FAIL_POINT,
    };
    use crate::device_marker::{DeviceMarker, MarkerConfidence, MarkerSource};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "pl-retirement-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }

        fn state_path(&self) -> PathBuf {
            self.0.join("pairing.json")
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn credential(label: &str) -> Credential {
        let generated = rcgen::generate_simple_self_signed(vec![label.to_owned()]).unwrap();
        Credential {
            client_key_pem: generated.key_pair.serialize_pem(),
            client_cert_pem: generated.cert.pem(),
            ca_chain_pem: vec![generated.cert.pem()],
            ca_fp_prefix: vec![1, 2, 3],
            instance_id: "fixture-journal".into(),
            home_label: "Fixture Journal".into(),
            endpoints: vec![crate::credential::EndpointAddr {
                host: "127.0.0.1".into(),
                port: 7657,
            }],
            relay_origin: None,
            device_token: None,
            device_token_expires_at: None,
        }
    }

    fn marker() -> MarkerResult {
        MarkerResult::Available(DeviceMarker {
            digest: "c".repeat(64),
            source: MarkerSource::PublisherSystemId,
            confidence: MarkerConfidence::Primary,
        })
    }

    fn save_incumbent(path: &Path, incumbent: Credential, access_generation: u64) {
        PairedState {
            credential: Some(incumbent),
            access_mutation_generation: access_generation,
            ..Default::default()
        }
        .save(path)
        .unwrap();
    }

    async fn success(_: Credential, _: String) -> Result<(), TransportError> {
        Ok(())
    }

    #[tokio::test]
    async fn intent_write_and_readback_failures_never_call_remote_retirement() {
        for (point, expected_file, expected_tmp) in
            [(1, false, false), (3, true, false), (4, false, true)]
        {
            let dir = TestDir::new(&format!("persist-{point}"));
            let calls = AtomicUsize::new(0);
            FS_FAIL_POINT.with(|fail| fail.set(point));
            let result =
                retire_wrong_mark_with(&dir.state_path(), credential("candidate"), |_, _| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    success(credential("unused"), String::new())
                })
                .await;
            FS_FAIL_POINT.with(|fail| fail.set(0));
            assert!(result.is_err());
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_eq!(dir.state_path().exists(), expected_file);
            assert_eq!(
                dir.state_path().with_extension("json.tmp").exists(),
                expected_tmp
            );
            if expected_file {
                assert_eq!(
                    PairedState::load(&dir.state_path())
                        .unwrap()
                        .retirement_intent
                        .unwrap()
                        .phase,
                    RetirementPhase::Prepared
                );
            }
        }
    }

    #[tokio::test]
    async fn restart_reconciles_a_durable_intent_before_clearing_it() {
        let dir = TestDir::new("restart");
        let candidate = credential("candidate");
        PairedState::install_retirement_intent(
            &dir.state_path(),
            new_wrong_mark_intent(candidate).unwrap(),
        )
        .unwrap();
        let stored = std::fs::read_to_string(dir.state_path()).unwrap();
        assert!(stored.contains("dpapi:v1:"));
        assert!(!stored.contains("PRIVATE KEY"));
        let calls = AtomicUsize::new(0);
        let outcome = reconcile_with(&dir.state_path(), |_, _| {
            calls.fetch_add(1, Ordering::SeqCst);
            success(credential("unused"), String::new())
        })
        .await
        .unwrap();
        assert_eq!(outcome, ReconcileOutcome::Completed);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!dir.state_path().exists());
    }

    #[tokio::test]
    async fn restart_promotes_only_a_fully_synced_staged_retirement_intent() {
        let dir = TestDir::new("staged-restart");
        let calls = AtomicUsize::new(0);
        FS_FAIL_POINT.with(|fail| fail.set(4));
        let first = retire_wrong_mark_with(&dir.state_path(), credential("candidate"), |_, _| {
            calls.fetch_add(1, Ordering::SeqCst);
            success(credential("unused"), String::new())
        })
        .await;
        FS_FAIL_POINT.with(|fail| fail.set(0));
        assert!(first.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(!dir.state_path().exists());
        assert!(dir.state_path().with_extension("json.tmp").exists());

        let result = reconcile_with(&dir.state_path(), |_, _| {
            calls.fetch_add(1, Ordering::SeqCst);
            success(credential("unused"), String::new())
        })
        .await
        .unwrap();
        assert_eq!(result, ReconcileOutcome::Completed);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!dir.state_path().exists());
        assert!(!dir.state_path().with_extension("json.tmp").exists());
    }

    #[tokio::test]
    async fn wrong_mark_retirement_refuses_to_claim_a_live_pairing() {
        let dir = TestDir::new("live-owner");
        let incumbent = credential("incumbent");
        let expected_cert = incumbent.client_cert_pem.clone();
        PairedState {
            credential: Some(incumbent),
            ..Default::default()
        }
        .save(&dir.state_path())
        .unwrap();
        let calls = AtomicUsize::new(0);
        let result = retire_wrong_mark_with(
            &dir.state_path(),
            credential("wrong-mark-candidate"),
            |_, _| {
                calls.fetch_add(1, Ordering::SeqCst);
                success(credential("unused"), String::new())
            },
        )
        .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let state = PairedState::load(&dir.state_path()).unwrap();
        assert_eq!(state.credential.unwrap().client_cert_pem, expected_cert);
        assert!(state.retirement_intent.is_none());
    }

    #[tokio::test]
    async fn malformed_owner_generation_is_rejected_before_remote_retirement() {
        let dir = TestDir::new("malformed-generation");
        let mut intent = new_wrong_mark_intent(credential("candidate")).unwrap();
        intent.owner_generation = [0; 32];
        PairedState {
            retirement_intent: Some(intent),
            ..Default::default()
        }
        .save(&dir.state_path())
        .unwrap();
        let calls = AtomicUsize::new(0);
        let result = reconcile_with(&dir.state_path(), |_, _| {
            calls.fetch_add(1, Ordering::SeqCst);
            success(credential("unused"), String::new())
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(dir.state_path().exists());

        let wrong_cid = TestDir::new("malformed-client-id");
        let mut intent = new_wrong_mark_intent(credential("candidate")).unwrap();
        intent.client_id = "sha256:wrong-device".into();
        PairedState {
            retirement_intent: Some(intent),
            ..Default::default()
        }
        .save(&wrong_cid.state_path())
        .unwrap();
        let cid_calls = AtomicUsize::new(0);
        assert!(reconcile_with(&wrong_cid.state_path(), |_, _| {
            cid_calls.fetch_add(1, Ordering::SeqCst);
            success(credential("unused"), String::new())
        })
        .await
        .is_err());
        assert_eq!(cid_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn retirement_cleanup_requires_the_exact_succeeded_intent() {
        let dir = TestDir::new("cleanup-fence");
        let intent = new_wrong_mark_intent(credential("candidate")).unwrap();
        let operation_id = intent.operation_id.clone();
        let generation = intent.owner_generation;
        PairedState::install_retirement_intent(&dir.state_path(), intent).unwrap();

        assert!(
            !PairedState::remove_retired_intent(&dir.state_path(), &operation_id, generation)
                .unwrap()
        );
        assert!(matches!(
            PairedState::update_retirement_phase(
                &dir.state_path(),
                &operation_id,
                generation,
                RetirementPhase::Unknown,
                RetirementPhase::Succeeded,
            ),
            Err(StorageError::CasMismatch)
        ));
        PairedState::update_retirement_phase(
            &dir.state_path(),
            &operation_id,
            generation,
            RetirementPhase::Prepared,
            RetirementPhase::Succeeded,
        )
        .unwrap();
        assert!(!PairedState::remove_retired_intent(
            &dir.state_path(),
            "stale-operation",
            generation
        )
        .unwrap());
        let mut wrong_generation = generation;
        wrong_generation[0] ^= 0xff;
        assert!(!PairedState::remove_retired_intent(
            &dir.state_path(),
            &operation_id,
            wrong_generation
        )
        .unwrap());
        assert!(dir.state_path().exists());
        assert!(
            PairedState::remove_retired_intent(&dir.state_path(), &operation_id, generation)
                .unwrap()
        );
        assert!(!dir.state_path().exists());
    }

    #[test]
    fn cleanup_never_removes_a_newer_paired_credential_even_with_a_stale_intent() {
        let dir = TestDir::new("cleanup-newer-pair");
        let mut intent = new_wrong_mark_intent(credential("old-device")).unwrap();
        intent.phase = RetirementPhase::Succeeded;
        let operation_id = intent.operation_id.clone();
        let generation = intent.owner_generation;
        let newer = credential("new-device");
        let expected_cert = newer.client_cert_pem.clone();
        PairedState {
            credential: Some(newer),
            retirement_intent: Some(intent),
            ..Default::default()
        }
        .save(&dir.state_path())
        .unwrap();

        assert!(
            !PairedState::remove_retired_intent(&dir.state_path(), &operation_id, generation)
                .unwrap()
        );
        let current = PairedState::load(&dir.state_path()).unwrap();
        assert_eq!(current.credential.unwrap().client_cert_pem, expected_cert);
        assert!(current.retirement_intent.is_some());
    }

    #[test]
    fn staged_recovery_ignores_unrelated_state_and_refuses_unprotected_candidates() {
        let dir = TestDir::new("staged-validation");
        let tmp = dir.state_path().with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(&PairedState::default()).unwrap()).unwrap();
        assert!(!PairedState::recover_staged_retirement_intent(&dir.state_path()).unwrap());
        assert!(!dir.state_path().exists());
        assert!(tmp.exists());
        std::fs::remove_file(&tmp).unwrap();

        let intent = new_wrong_mark_intent(credential("candidate")).unwrap();
        let staged = PairedState {
            retirement_intent: Some(intent),
            ..Default::default()
        };
        std::fs::write(&tmp, serde_json::to_vec(&staged).unwrap()).unwrap();
        assert!(matches!(
            PairedState::recover_staged_retirement_intent(&dir.state_path()),
            Err(StorageError::Transport(TransportError::CredentialMalformed))
        ));
        assert!(!dir.state_path().exists());
    }

    #[tokio::test]
    async fn unknown_retirement_keeps_the_same_intent_for_restart_retry() {
        let dir = TestDir::new("unknown");
        let candidate = credential("candidate");
        let original = new_wrong_mark_intent(candidate.clone()).unwrap();
        PairedState::install_retirement_intent(&dir.state_path(), original.clone()).unwrap();
        let result = reconcile_with(&dir.state_path(), move |_, _| {
            let error = TransportError::Io(std::io::Error::other("unknown remote result"));
            async move { Err(error) }
        })
        .await;
        assert!(result.is_err());
        let unknown = PairedState::load(&dir.state_path())
            .unwrap()
            .retirement_intent
            .unwrap();
        assert_eq!(unknown.phase, RetirementPhase::Unknown);
        assert_eq!(unknown.operation_id, original.operation_id);
        reconcile_with(&dir.state_path(), |_, _| {
            success(credential("unused"), String::new())
        })
        .await
        .unwrap();
        assert!(!dir.state_path().exists());
    }

    #[tokio::test]
    async fn cleanup_failure_keeps_succeeded_intent_and_restart_skips_delete() {
        let dir = TestDir::new("cleanup");
        PairedState::install_retirement_intent(
            &dir.state_path(),
            new_wrong_mark_intent(credential("candidate")).unwrap(),
        )
        .unwrap();
        let calls = AtomicUsize::new(0);
        RETIREMENT_CLEANUP_FAIL_POINT.with(|fail| fail.set(true));
        let first = reconcile_with(&dir.state_path(), |_, _| {
            calls.fetch_add(1, Ordering::SeqCst);
            success(credential("unused"), String::new())
        })
        .await;
        RETIREMENT_CLEANUP_FAIL_POINT.with(|fail| fail.set(false));
        assert!(first.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            PairedState::load(&dir.state_path())
                .unwrap()
                .retirement_intent
                .unwrap()
                .phase,
            RetirementPhase::Succeeded
        );

        reconcile_with(&dir.state_path(), |_, _| async {
            panic!("succeeded retirement must not be sent again")
        })
        .await
        .unwrap();
        assert!(!dir.state_path().exists());
    }

    #[tokio::test]
    async fn delayed_old_retirement_cannot_clear_a_newer_same_journal_credential() {
        let dir = TestDir::new("stale-callback");
        PairedState::install_retirement_intent(
            &dir.state_path(),
            new_wrong_mark_intent(credential("old-device")).unwrap(),
        )
        .unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let path = dir.state_path();
        let old_retirement = tokio::spawn(async move {
            reconcile_with(&path, move |_, _| async move {
                let _ = started_tx.send(());
                let _ = finish_rx.await;
                Ok(())
            })
            .await
        });
        started_rx.await.unwrap();

        let newer = credential("new-device");
        let expected_cert = newer.client_cert_pem.clone();
        PairedState {
            credential: Some(newer),
            ..Default::default()
        }
        .save(&dir.state_path())
        .unwrap();
        finish_tx.send(()).unwrap();
        assert_eq!(
            old_retirement.await.unwrap().unwrap(),
            ReconcileOutcome::Superseded
        );

        let current = PairedState::load(&dir.state_path()).unwrap();
        assert_eq!(current.credential.unwrap().client_cert_pem, expected_cert);
        assert!(current.retirement_intent.is_none());
    }

    #[tokio::test]
    async fn delayed_old_retirement_cannot_advance_a_newer_same_journal_intent() {
        let dir = TestDir::new("stale-new-intent");
        PairedState::install_retirement_intent(
            &dir.state_path(),
            new_wrong_mark_intent(credential("old-device")).unwrap(),
        )
        .unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let path = dir.state_path();
        let old_retirement = tokio::spawn(async move {
            reconcile_with(&path, move |_, _| async move {
                let _ = started_tx.send(());
                let _ = finish_rx.await;
                Ok(())
            })
            .await
        });
        started_rx.await.unwrap();

        let newer = new_wrong_mark_intent(credential("new-device")).unwrap();
        let newer_id = newer.operation_id.clone();
        PairedState {
            retirement_intent: Some(newer),
            ..Default::default()
        }
        .save(&dir.state_path())
        .unwrap();
        finish_tx.send(()).unwrap();
        assert_eq!(
            old_retirement.await.unwrap().unwrap(),
            ReconcileOutcome::Superseded
        );

        let current = PairedState::load(&dir.state_path()).unwrap();
        let current_intent = current.retirement_intent.unwrap();
        assert_eq!(current_intent.operation_id, newer_id);
        assert_eq!(current_intent.phase, RetirementPhase::Prepared);
    }

    #[tokio::test]
    async fn replacement_intent_write_or_readback_failure_never_deletes_and_keeps_incumbent() {
        for point in [1, 3] {
            let dir = TestDir::new(&format!("replacement-persist-{point}"));
            let incumbent = credential("replacement-old");
            let old_cert = incumbent.client_cert_pem.clone();
            let old_binding =
                crate::ack::JournalIdentity::from_credential(&incumbent).client_cert_sha256;
            save_incumbent(&dir.state_path(), incumbent, 7);
            answer::write_answer(
                &answer::answer_path(&dir.state_path()),
                &answer::AnswerState {
                    confirmed: old_binding.clone(),
                    rejected: String::new(),
                },
            )
            .unwrap();
            let before_calls = AtomicUsize::new(0);
            let delete_calls = AtomicUsize::new(0);
            FS_FAIL_POINT.with(|fail| fail.set(point));
            let result = replace_pair_with(
                &dir.state_path(),
                credential("replacement-new"),
                marker(),
                || async {
                    before_calls.fetch_add(1, Ordering::SeqCst);
                },
                |_, _| {
                    delete_calls.fetch_add(1, Ordering::SeqCst);
                    success(credential("unused"), String::new())
                },
            )
            .await;
            FS_FAIL_POINT.with(|fail| fail.set(0));

            assert!(result.is_err());
            assert_eq!(before_calls.load(Ordering::SeqCst), 0);
            assert_eq!(delete_calls.load(Ordering::SeqCst), 0);
            let current = PairedState::load(&dir.state_path()).unwrap();
            assert_eq!(current.credential.unwrap().client_cert_pem, old_cert);
            assert!(current.retirement_intent.is_none());
            assert_eq!(
                answer::read_answer(&answer::answer_path(&dir.state_path()))
                    .unwrap()
                    .unwrap()
                    .confirmed,
                old_binding
            );
        }
    }

    #[tokio::test]
    async fn replacement_unknown_response_replays_same_retirement_and_resumes_after_restart() {
        let dir = TestDir::new("replacement-restart");
        let incumbent = credential("replacement-old");
        let incumbent_cert = incumbent.client_cert_pem.clone();
        let old_cid = client_id_for_credential(&incumbent).unwrap();
        let candidate = credential("replacement-new");
        let candidate_cert = candidate.client_cert_pem.clone();
        let candidate_generation = pairing_generation(&candidate_cert);
        save_incumbent(&dir.state_path(), incumbent, 9);
        let calls = AtomicUsize::new(0);

        let first = replace_pair_with(
            &dir.state_path(),
            candidate,
            marker(),
            || async {},
            |retiring, cid| {
                assert_eq!(retiring.client_cert_pem, incumbent_cert);
                assert_eq!(cid, old_cid);
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err(TransportError::Io(std::io::Error::other("lost response"))) }
            },
        )
        .await;
        assert!(first.is_err());
        let saved = PairedState::load(&dir.state_path())
            .unwrap()
            .retirement_intent
            .unwrap();
        assert_eq!(saved.phase, RetirementPhase::Unknown);
        let operation_id = saved.operation_id.clone();
        assert_eq!(saved.client_id, old_cid);
        assert_eq!(saved.candidate_generation, candidate_generation);
        assert_eq!(saved.candidate.client_cert_pem, candidate_cert);

        let retry_path = dir.state_path();
        let retry_callback_path = retry_path.clone();
        let completed = reconcile_with(&dir.state_path(), |retiring, cid| {
            assert_eq!(retiring.client_cert_pem, incumbent_cert);
            assert_eq!(cid, old_cid);
            calls.fetch_add(1, Ordering::SeqCst);
            async move {
                let pending = PairedState::load(&retry_callback_path).unwrap();
                assert_eq!(
                    pending.retirement_intent.unwrap().operation_id,
                    operation_id
                );
                Ok(())
            }
        })
        .await
        .unwrap();
        assert_eq!(completed, ReconcileOutcome::Completed);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let current = PairedState::load(&dir.state_path()).unwrap();
        assert_eq!(current.credential.unwrap().client_cert_pem, candidate_cert);
        assert!(current.retirement_intent.is_none());
        assert_eq!(
            answer::read_answer(&answer::answer_path(&dir.state_path()))
                .unwrap()
                .unwrap(),
            answer::AnswerState::default()
        );
        let record = migration::load(&dir.state_path()).unwrap().unwrap();
        assert_eq!(record.pairing_generation, candidate_generation);
    }

    #[tokio::test]
    async fn replacement_restart_finishes_partial_candidate_publication_without_second_delete() {
        let dir = TestDir::new("replacement-partial-publication");
        let incumbent = credential("replacement-old");
        let old_binding =
            crate::ack::JournalIdentity::from_credential(&incumbent).client_cert_sha256;
        let candidate = credential("replacement-new");
        let candidate_cert = candidate.client_cert_pem.clone();
        save_incumbent(&dir.state_path(), incumbent, 2);
        answer::write_answer(
            &answer::answer_path(&dir.state_path()),
            &answer::AnswerState {
                confirmed: old_binding.clone(),
                rejected: String::new(),
            },
        )
        .unwrap();
        let deletes = AtomicUsize::new(0);
        PAIR_REPLACEMENT_FAIL_POINT.with(|fail| fail.set(true));
        let first = replace_pair_with(
            &dir.state_path(),
            candidate,
            marker(),
            || async {},
            |_, _| {
                deletes.fetch_add(1, Ordering::SeqCst);
                success(credential("unused"), String::new())
            },
        )
        .await;
        assert!(first.is_err());
        let partial = PairedState::load(&dir.state_path()).unwrap();
        assert_eq!(partial.credential.unwrap().client_cert_pem, candidate_cert);
        assert_eq!(
            partial.retirement_intent.unwrap().phase,
            RetirementPhase::Succeeded
        );
        assert_eq!(deletes.load(Ordering::SeqCst), 1);
        assert_eq!(
            answer::read_answer(&answer::answer_path(&dir.state_path()))
                .unwrap()
                .unwrap()
                .confirmed,
            old_binding
        );

        let restarted = reconcile_with(&dir.state_path(), |_, _| async {
            panic!("succeeded retirement must not be repeated")
        })
        .await
        .unwrap();
        assert_eq!(restarted, ReconcileOutcome::Completed);
        assert_eq!(deletes.load(Ordering::SeqCst), 1);
        assert!(PairedState::load(&dir.state_path())
            .unwrap()
            .retirement_intent
            .is_none());
        assert_eq!(
            answer::read_answer(&answer::answer_path(&dir.state_path()))
                .unwrap()
                .unwrap(),
            answer::AnswerState::default()
        );
        assert!(
            migration::load(&dir.state_path())
                .unwrap()
                .unwrap()
                .fresh_pair_offer_pending
        );
    }

    #[tokio::test]
    async fn delayed_replacement_retirement_cannot_mutate_a_newer_same_journal_pair() {
        let dir = TestDir::new("replacement-stale-callback");
        let incumbent = credential("replacement-old");
        // Install the incumbent before producing the intent that binds its CAS key.
        save_incumbent(&dir.state_path(), incumbent, 3);
        let intent = new_pair_replacement_intent(
            &dir.state_path(),
            credential("replacement-staged"),
            marker(),
        )
        .unwrap();
        let owner_generation = intent.owner_generation;
        let candidate_generation = intent.candidate_generation;
        PairedState::install_pair_replacement_intent(
            &dir.state_path(),
            owner_generation,
            intent.access_mutation_generation,
            intent,
        )
        .unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let path = dir.state_path();
        let old_delete = tokio::spawn(async move {
            reconcile_with(&path, move |_, _| async move {
                let _ = started_tx.send(());
                let _ = finish_rx.await;
                Ok(())
            })
            .await
        });
        started_rx.await.unwrap();

        let newer = credential("replacement-newer");
        let newer_cert = newer.client_cert_pem.clone();
        let newer_binding = crate::ack::JournalIdentity::from_credential(&newer).client_cert_sha256;
        PairedState {
            credential: Some(newer),
            access_mutation_generation: 0,
            ..Default::default()
        }
        .save(&dir.state_path())
        .unwrap();
        answer::write_answer(
            &answer::answer_path(&dir.state_path()),
            &answer::AnswerState {
                confirmed: newer_binding.clone(),
                rejected: String::new(),
            },
        )
        .unwrap();
        let newer_marker = MarkerResult::Available(DeviceMarker {
            digest: "d".repeat(64),
            source: MarkerSource::PublisherSystemId,
            confidence: MarkerConfidence::Primary,
        });
        migration::record_pair_replacement_offer(
            &dir.state_path(),
            PairedState::load(&dir.state_path())
                .unwrap()
                .credential
                .as_ref()
                .unwrap(),
            &newer_marker,
        )
        .unwrap();
        let newer_migration = migration::load(&dir.state_path()).unwrap().unwrap();
        finish_tx.send(()).unwrap();

        assert_eq!(
            old_delete.await.unwrap().unwrap(),
            ReconcileOutcome::Superseded
        );
        let current = PairedState::load(&dir.state_path()).unwrap();
        assert_eq!(current.credential.unwrap().client_cert_pem, newer_cert);
        assert!(current.retirement_intent.is_none());
        assert_eq!(
            answer::read_answer(&answer::answer_path(&dir.state_path()))
                .unwrap()
                .unwrap()
                .confirmed,
            newer_binding
        );
        let current_migration = migration::load(&dir.state_path()).unwrap().unwrap();
        assert_eq!(current_migration.revision, newer_migration.revision);
        assert_eq!(
            current_migration.pairing_generation,
            pairing_generation(&newer_cert)
        );
        assert_ne!(current_migration.pairing_generation, candidate_generation);
        assert_ne!(current_migration.pairing_generation, owner_generation);
    }

    #[test]
    fn replacement_intent_install_refuses_a_stale_incumbent_generation() {
        let dir = TestDir::new("replacement-install-cas");
        let expected_incumbent = credential("expected-old");
        let stale_incumbent = credential("actual-old");
        let expected_generation = pairing_generation(&expected_incumbent.client_cert_pem);
        save_incumbent(&dir.state_path(), stale_incumbent.clone(), 5);
        let mut intent = new_pair_replacement_intent(
            &dir.state_path(),
            credential("replacement-candidate"),
            marker(),
        )
        .unwrap();
        intent.owner_generation = expected_generation;
        intent.access_mutation_generation = 5;

        assert!(matches!(
            PairedState::install_pair_replacement_intent(
                &dir.state_path(),
                expected_generation,
                5,
                intent,
            ),
            Err(StorageError::CasMismatch)
        ));
        let state = PairedState::load(&dir.state_path()).unwrap();
        assert_eq!(
            state.credential.unwrap().client_cert_pem,
            stale_incumbent.client_cert_pem
        );
        assert!(state.retirement_intent.is_none());
    }

    #[test]
    fn replacement_intent_install_refuses_a_stale_access_generation() {
        let dir = TestDir::new("replacement-install-access-cas");
        let incumbent = credential("replacement-old");
        let incumbent_cert = incumbent.client_cert_pem.clone();
        save_incumbent(&dir.state_path(), incumbent.clone(), 5);
        let intent = new_pair_replacement_intent(
            &dir.state_path(),
            credential("replacement-candidate"),
            marker(),
        )
        .unwrap();
        let owner_generation = intent.owner_generation;
        let mut newer_access = PairedState::load(&dir.state_path()).unwrap();
        newer_access.access_mutation_generation = 6;
        newer_access.save(&dir.state_path()).unwrap();

        assert!(matches!(
            PairedState::install_pair_replacement_intent(
                &dir.state_path(),
                owner_generation,
                5,
                intent,
            ),
            Err(StorageError::CasMismatch)
        ));
        let state = PairedState::load(&dir.state_path()).unwrap();
        assert_eq!(state.access_mutation_generation, 6);
        assert_eq!(state.credential.unwrap().client_cert_pem, incumbent_cert);
        assert!(state.retirement_intent.is_none());
    }

    #[test]
    fn replacement_rollback_refuses_a_newer_operation() {
        let dir = TestDir::new("replacement-rollback-cas");
        let incumbent = credential("replacement-old");
        save_incumbent(&dir.state_path(), incumbent, 2);
        let intent = new_pair_replacement_intent(
            &dir.state_path(),
            credential("replacement-candidate"),
            marker(),
        )
        .unwrap();
        let owner_generation = intent.owner_generation;
        let candidate_generation = intent.candidate_generation;
        let stale_operation = intent.operation_id.clone();
        PairedState::install_pair_replacement_intent(
            &dir.state_path(),
            owner_generation,
            intent.access_mutation_generation,
            intent.clone(),
        )
        .unwrap();
        let mut newer_state = PairedState::load(&dir.state_path()).unwrap();
        newer_state.retirement_intent.as_mut().unwrap().operation_id = "newer-operation".into();
        newer_state.save(&dir.state_path()).unwrap();

        assert!(!PairedState::rollback_unretired_pair_replacement(
            &dir.state_path(),
            &stale_operation,
            owner_generation,
            candidate_generation,
        )
        .unwrap());
        assert_eq!(
            PairedState::load(&dir.state_path())
                .unwrap()
                .retirement_intent
                .unwrap()
                .operation_id,
            "newer-operation"
        );
    }

    #[test]
    fn replacement_candidate_commit_requires_exact_succeeded_operation_and_owner() {
        let dir = TestDir::new("replacement-commit-cas");
        let incumbent = credential("replacement-old");
        let old_cert = incumbent.client_cert_pem.clone();
        save_incumbent(&dir.state_path(), incumbent, 8);
        let intent = new_pair_replacement_intent(
            &dir.state_path(),
            credential("replacement-candidate"),
            marker(),
        )
        .unwrap();
        let owner_generation = intent.owner_generation;
        let candidate_generation = intent.candidate_generation;
        let operation_id = intent.operation_id.clone();
        PairedState::install_pair_replacement_intent(
            &dir.state_path(),
            owner_generation,
            intent.access_mutation_generation,
            intent,
        )
        .unwrap();
        PairedState::update_retirement_phase(
            &dir.state_path(),
            &operation_id,
            owner_generation,
            RetirementPhase::Prepared,
            RetirementPhase::Succeeded,
        )
        .unwrap();

        assert!(matches!(
            PairedState::commit_pair_replacement_candidate(
                &dir.state_path(),
                "stale-operation",
                owner_generation,
                candidate_generation,
            ),
            Err(StorageError::CasMismatch)
        ));
        assert_eq!(
            PairedState::load(&dir.state_path())
                .unwrap()
                .credential
                .unwrap()
                .client_cert_pem,
            old_cert
        );
    }

    #[test]
    fn replacement_candidate_commit_refuses_unretired_intent() {
        let dir = TestDir::new("replacement-commit-pending-cas");
        let incumbent = credential("replacement-old");
        let old_cert = incumbent.client_cert_pem.clone();
        save_incumbent(&dir.state_path(), incumbent, 8);
        let intent = new_pair_replacement_intent(
            &dir.state_path(),
            credential("replacement-candidate"),
            marker(),
        )
        .unwrap();
        let owner_generation = intent.owner_generation;
        let candidate_generation = intent.candidate_generation;
        let operation_id = intent.operation_id.clone();
        PairedState::install_pair_replacement_intent(
            &dir.state_path(),
            owner_generation,
            intent.access_mutation_generation,
            intent,
        )
        .unwrap();

        assert!(matches!(
            PairedState::commit_pair_replacement_candidate(
                &dir.state_path(),
                &operation_id,
                owner_generation,
                candidate_generation,
            ),
            Err(StorageError::CasMismatch)
        ));
        assert_eq!(
            PairedState::load(&dir.state_path())
                .unwrap()
                .credential
                .unwrap()
                .client_cert_pem,
            old_cert
        );
    }

    #[test]
    fn replacement_intent_clear_requires_exact_committed_candidate_generation() {
        let dir = TestDir::new("replacement-clear-cas");
        save_incumbent(&dir.state_path(), credential("replacement-old"), 1);
        let intent = new_pair_replacement_intent(
            &dir.state_path(),
            credential("replacement-candidate"),
            marker(),
        )
        .unwrap();
        let owner_generation = intent.owner_generation;
        let candidate_generation = intent.candidate_generation;
        let operation_id = intent.operation_id.clone();
        PairedState::install_pair_replacement_intent(
            &dir.state_path(),
            owner_generation,
            intent.access_mutation_generation,
            intent,
        )
        .unwrap();
        PairedState::update_retirement_phase(
            &dir.state_path(),
            &operation_id,
            owner_generation,
            RetirementPhase::Prepared,
            RetirementPhase::Succeeded,
        )
        .unwrap();
        PairedState::commit_pair_replacement_candidate(
            &dir.state_path(),
            &operation_id,
            owner_generation,
            candidate_generation,
        )
        .unwrap();

        assert!(!PairedState::clear_pair_replacement_intent(
            &dir.state_path(),
            "stale-operation",
            owner_generation,
            candidate_generation,
        )
        .unwrap());
        let mut wrong_generation = candidate_generation;
        wrong_generation[0] ^= 0xff;
        assert!(!PairedState::clear_pair_replacement_intent(
            &dir.state_path(),
            &operation_id,
            owner_generation,
            wrong_generation,
        )
        .unwrap());
        let state = PairedState::load(&dir.state_path()).unwrap();
        assert!(state.credential.is_some());
        assert!(state.retirement_intent.is_some());
        assert!(PairedState::clear_pair_replacement_intent(
            &dir.state_path(),
            &operation_id,
            owner_generation,
            candidate_generation,
        )
        .unwrap());
        assert!(PairedState::load(&dir.state_path())
            .unwrap()
            .retirement_intent
            .is_none());
    }

    #[tokio::test]
    async fn rejection_intent_write_and_readback_failures_send_no_remote_delete() {
        for point in [1, 2, 3, 4] {
            let dir = TestDir::new(&format!("reject-persist-{point}"));
            let old = credential("old-rejected");
            let binding = crate::ack::JournalIdentity::from_credential(&old).client_cert_sha256;
            save_incumbent(&dir.state_path(), old.clone(), 7);
            let remote_calls = AtomicUsize::new(0);
            let before_calls = AtomicUsize::new(0);
            FS_FAIL_POINT.with(|fail| fail.set(point));
            let result = reject_pair_with(
                &dir.state_path(),
                &binding,
                || async {
                    before_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
                |_, _| {
                    remote_calls.fetch_add(1, Ordering::SeqCst);
                    success(credential("unused"), String::new())
                },
            )
            .await;
            FS_FAIL_POINT.with(|fail| fail.set(0));

            assert!(result.is_err());
            assert_eq!(remote_calls.load(Ordering::SeqCst), 0);
            assert_eq!(before_calls.load(Ordering::SeqCst), 0);
            assert_eq!(
                PairedState::load(&dir.state_path())
                    .unwrap()
                    .credential
                    .unwrap()
                    .client_cert_pem,
                old.client_cert_pem
            );
        }
    }

    #[tokio::test]
    async fn rejection_unknown_result_retries_same_intent_on_restart() {
        let dir = TestDir::new("reject-unknown-restart");
        let old = credential("old-rejected");
        let binding = crate::ack::JournalIdentity::from_credential(&old).client_cert_sha256;
        save_incumbent(&dir.state_path(), old, 13);
        let first_call = AtomicUsize::new(0);
        let first = reject_pair_with(
            &dir.state_path(),
            &binding,
            || async { Ok(()) },
            |_, _| {
                first_call.fetch_add(1, Ordering::SeqCst);
                async { Err(TransportError::Io(std::io::Error::other("response lost"))) }
            },
        )
        .await;
        assert!(first.is_err());
        assert_eq!(first_call.load(Ordering::SeqCst), 1);
        let saved = PairedState::load(&dir.state_path())
            .unwrap()
            .retirement_intent
            .unwrap();
        assert_eq!(saved.operation, RetirementOperation::GuiPairRejection);
        assert_eq!(saved.phase, RetirementPhase::Unknown);

        let replayed = AtomicUsize::new(0);
        let outcome = reconcile_with(&dir.state_path(), |credential, client_id| {
            replayed.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                pairing_generation(&credential.client_cert_pem),
                saved.owner_generation
            );
            assert_eq!(client_id, saved.client_id);
            success(credential, client_id)
        })
        .await
        .unwrap();

        assert_eq!(outcome, ReconcileOutcome::Completed);
        assert_eq!(replayed.load(Ordering::SeqCst), 1);
        let settled = PairedState::load(&dir.state_path()).unwrap();
        assert!(settled.credential.is_none());
        assert!(settled.retirement_intent.is_none());
    }

    #[tokio::test]
    async fn rejection_cleanup_failure_restarts_without_repeating_succeeded_delete() {
        let dir = TestDir::new("reject-cleanup-failure");
        let old = credential("old-rejected");
        let binding = crate::ack::JournalIdentity::from_credential(&old).client_cert_sha256;
        save_incumbent(&dir.state_path(), old, 17);
        let remote_calls = AtomicUsize::new(0);
        PAIR_REJECTION_CLEANUP_FAIL_POINT.with(|fail| fail.set(true));
        let first = reject_pair_with(
            &dir.state_path(),
            &binding,
            || async { Ok(()) },
            |credential, client_id| {
                remote_calls.fetch_add(1, Ordering::SeqCst);
                success(credential, client_id)
            },
        )
        .await;
        assert!(first.is_err());
        assert_eq!(remote_calls.load(Ordering::SeqCst), 1);
        let saved = PairedState::load(&dir.state_path()).unwrap();
        assert_eq!(
            saved.retirement_intent.as_ref().unwrap().phase,
            RetirementPhase::Succeeded
        );
        assert!(saved.credential.is_some());

        assert_eq!(
            reconcile_with(&dir.state_path(), |_, _| async {
                panic!("a succeeded rejection must not send DELETE again")
            })
            .await
            .unwrap(),
            ReconcileOutcome::Completed
        );
        assert_eq!(remote_calls.load(Ordering::SeqCst), 1);
        assert!(PairedState::load(&dir.state_path())
            .unwrap()
            .credential
            .is_none());
    }

    #[tokio::test]
    async fn delayed_rejection_cannot_clear_a_newer_same_journal_pair_or_answer() {
        let dir = TestDir::new("reject-stale-callback");
        let old = credential("old-rejected");
        let old_binding = crate::ack::JournalIdentity::from_credential(&old).client_cert_sha256;
        save_incumbent(&dir.state_path(), old, 19);
        answer::write_answer(
            &answer::answer_path(&dir.state_path()),
            &answer::AnswerState {
                confirmed: String::new(),
                rejected: old_binding.clone(),
            },
        )
        .unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let path = dir.state_path();
        let binding = old_binding;
        let old_rejection = tokio::spawn(async move {
            reject_pair_with(
                &path,
                &binding,
                || async { Ok(()) },
                move |_, _| async move {
                    let _ = started_tx.send(());
                    let _ = finish_rx.await;
                    Ok(())
                },
            )
            .await
        });
        started_rx.await.unwrap();

        let mut newer = credential("newer-same-journal");
        newer.instance_id = "fixture-journal".to_owned();
        let new_binding = crate::ack::JournalIdentity::from_credential(&newer).client_cert_sha256;
        PairedState {
            credential: Some(newer.clone()),
            access_mutation_generation: 23,
            ..Default::default()
        }
        .save(&dir.state_path())
        .unwrap();
        let new_answer = answer::AnswerState {
            confirmed: new_binding,
            rejected: String::new(),
        };
        answer::write_answer(&answer::answer_path(&dir.state_path()), &new_answer).unwrap();
        finish_tx.send(()).unwrap();

        assert_eq!(
            old_rejection.await.unwrap().unwrap(),
            ReconcileOutcome::Superseded
        );
        let current = PairedState::load(&dir.state_path()).unwrap();
        assert_eq!(
            current.credential.unwrap().client_cert_pem,
            newer.client_cert_pem
        );
        assert!(current.retirement_intent.is_none());
        assert_eq!(
            answer::read_answer(&answer::answer_path(&dir.state_path())).unwrap(),
            Some(new_answer)
        );
    }

    #[test]
    fn persisted_rejection_intent_cannot_finish_against_new_pair_or_access_generation() {
        let old = credential("old-rejected");
        let old_state = PairedState {
            credential: Some(old.clone()),
            access_mutation_generation: 29,
            ..Default::default()
        };
        let mut intent = new_pair_rejection_intent(&old_state, old.clone()).unwrap();
        intent.phase = RetirementPhase::Succeeded;

        let mut newer = credential("newer-same-journal");
        newer.instance_id = "fixture-journal".to_owned();
        let stale_states = [
            (
                "reject-persisted-stale-pair-generation",
                newer.clone(),
                intent.access_mutation_generation,
            ),
            (
                "reject-persisted-stale-access-generation",
                old.clone(),
                intent.access_mutation_generation.wrapping_add(1),
            ),
        ];

        for (label, current_credential, access_generation) in stale_states {
            let dir = TestDir::new(label);
            PairedState {
                credential: Some(current_credential.clone()),
                retirement_intent: Some(intent.clone()),
                access_mutation_generation: access_generation,
            }
            .save(&dir.state_path())
            .unwrap();

            let cleanup_called = AtomicUsize::new(0);
            assert!(!PairedState::finish_pair_rejection(
                &dir.state_path(),
                &intent.operation_id,
                intent.owner_generation,
                intent.access_mutation_generation,
                || {
                    cleanup_called.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            )
            .unwrap());

            assert_eq!(cleanup_called.load(Ordering::SeqCst), 0);
            let current = PairedState::load(&dir.state_path()).unwrap();
            assert_eq!(
                current.credential.unwrap().client_cert_pem,
                current_credential.client_cert_pem
            );
            assert_eq!(
                current.retirement_intent.unwrap().operation_id,
                intent.operation_id
            );
        }
    }
}
