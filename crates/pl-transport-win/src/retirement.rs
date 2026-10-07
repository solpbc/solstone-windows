// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Restartable retirement of a pairing the owner rejected or cancelled at its
//! mark check. The intent is durable before the uploader stops; the rejected
//! journal's DELETE is best effort, and the local retirement always completes
//! so the owner can pair again.

use std::future::Future;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use crate::answer;
use crate::credential::{
    pairing_generation, Credential, PairedState, RetirementIntent, RetirementPhase, StorageError,
};
use crate::{ObserverClient, TransportError};

const RETIREMENT_SCHEMA: u32 = 1;
const RETIRE_CLIENT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileOutcome {
    None,
    Completed,
    Superseded,
}

/// Reconcile a saved rejection before pairing admission or any send.
pub async fn reconcile_on_launch(state_path: &Path) -> Result<ReconcileOutcome, TransportError> {
    reconcile_on_launch_with(state_path, retire_credential).await
}

/// The launch reconciliation with an injected retirement callback.
pub async fn reconcile_on_launch_with<F, Fut>(
    state_path: &Path,
    retire: F,
) -> Result<ReconcileOutcome, TransportError>
where
    F: FnOnce(Credential, String) -> Fut,
    Fut: Future<Output = Result<(), TransportError>>,
{
    stage_legacy_rejection(state_path)?;
    reconcile_with(state_path, retire).await
}

/// Convert a legacy rejected digest into the same typed transaction before
/// launch can bind access or admit send work.
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

/// Injected reconciliation seam used by the restart tests; the production
/// path above supplies the real observer client.
pub async fn reconcile_with<F, Fut>(
    state_path: &Path,
    retire: F,
) -> Result<ReconcileOutcome, TransportError>
where
    F: FnOnce(Credential, String) -> Fut,
    Fut: Future<Output = Result<(), TransportError>>,
{
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
        let incumbent = paired
            .credential
            .ok_or(TransportError::CredentialMalformed)?;
        if pairing_generation(&incumbent.client_cert_pem) != intent.owner_generation
            || paired.access_mutation_generation != intent.access_mutation_generation
        {
            return Ok(ReconcileOutcome::Superseded);
        }
        match retire(incumbent, intent.client_id.clone()).await {
            Ok(()) => {
                if !set_phase_or_superseded(state_path, &intent, RetirementPhase::Succeeded)? {
                    return Ok(ReconcileOutcome::Superseded);
                }
                intent.phase = RetirementPhase::Succeeded;
            }
            Err(error) => {
                if !set_phase_or_superseded(state_path, &intent, RetirementPhase::Unknown)? {
                    return Ok(ReconcileOutcome::Superseded);
                }
                // The owner rejected this journal; it may never answer. Its
                // DELETE is best effort: finish the local retirement so the
                // owner can pair again, and leave the dead row to the journal.
                tracing::warn!(target: "sync", error = %error, "rejected journal did not confirm client retirement; retiring locally");
                intent.phase = RetirementPhase::Unknown;
            }
        }
    }
    finish_pair_rejection(state_path, &intent)
}

fn finish_pair_rejection(
    state_path: &Path,
    intent: &RetirementIntent,
) -> Result<ReconcileOutcome, TransportError> {
    let binding =
        crate::ack::JournalIdentity::from_credential(&intent.credential).client_cert_sha256;
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
            && current.access_mutation_generation == intent.access_mutation_generation
    }) && state.credential.as_ref().is_some_and(|credential| {
        pairing_generation(&credential.client_cert_pem) == intent.owner_generation
            && state.access_mutation_generation == intent.access_mutation_generation
    }))
}

fn validate_intent(intent: &RetirementIntent, state_path: &Path) -> Result<(), TransportError> {
    let paired = PairedState::load(state_path)?;
    let incumbent = paired
        .credential
        .as_ref()
        .ok_or(TransportError::CredentialMalformed)?;
    if intent.schema != RETIREMENT_SCHEMA
        || intent.operation_id.is_empty()
        || intent.client_id.is_empty()
        || pairing_generation(&intent.credential.client_cert_pem) != intent.owner_generation
        || pairing_generation(&incumbent.client_cert_pem) != intent.owner_generation
        || intent.access_mutation_generation != paired.access_mutation_generation
        || crate::migration::credential_cid(incumbent)? != intent.client_id
    {
        return Err(TransportError::CredentialMalformed);
    }
    Ok(())
}

fn new_pair_rejection_intent(
    paired: &PairedState,
    credential: Credential,
) -> Result<RetirementIntent, TransportError> {
    let owner_generation = pairing_generation(&credential.client_cert_pem);
    let client_id = crate::migration::credential_cid(&credential)?;
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
        phase: RetirementPhase::Prepared,
        owner_generation,
        access_mutation_generation: paired.access_mutation_generation,
        client_id,
        credential,
    })
}

fn hex(value: &[u8]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential::{FS_FAIL_POINT, PAIR_REJECTION_CLEANUP_FAIL_POINT};
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
    async fn rejection_unknown_result_retires_locally_and_restart_retries_once() {
        let dir = TestDir::new("reject-unknown-local");
        let old = credential("old-rejected");
        let binding = crate::ack::JournalIdentity::from_credential(&old).client_cert_sha256;
        save_incumbent(&dir.state_path(), old.clone(), 13);
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
        .await
        .unwrap();
        assert_eq!(first, ReconcileOutcome::Completed);
        assert_eq!(first_call.load(Ordering::SeqCst), 1);
        let settled = PairedState::load(&dir.state_path()).unwrap();
        assert!(settled.credential.is_none());
        assert!(settled.retirement_intent.is_none());

        // An intent left behind by an interrupted rejection is retried once at
        // restart while its credential is still on disk, then dropped even
        // when the rejected journal still does not answer.
        save_incumbent(&dir.state_path(), old.clone(), 13);
        let paired = PairedState::load(&dir.state_path()).unwrap();
        let intent = new_pair_rejection_intent(&paired, old).unwrap();
        PairedState::install_pair_rejection_intent(
            &dir.state_path(),
            intent.owner_generation,
            intent.access_mutation_generation,
            intent.clone(),
        )
        .unwrap();
        let replayed = AtomicUsize::new(0);
        let outcome = reconcile_with(&dir.state_path(), |credential, client_id| {
            replayed.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                pairing_generation(&credential.client_cert_pem),
                intent.owner_generation
            );
            assert_eq!(client_id, intent.client_id);
            async { Err(TransportError::Io(std::io::Error::other("unreachable"))) }
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
