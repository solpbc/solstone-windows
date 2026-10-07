// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Pairing session coordination and confirmation gate actions.

use std::sync::{Arc, Mutex};

use observer_model::{SyncSnapshot, MARK_REJECTED_DETAIL, PAIRING_CANCELLED_DETAIL};

use crate::access::CredentialAccess;
use crate::answer::{answer_path, read_answer, write_answer};
use crate::credential::StorageError;
use crate::service::{publish_pairing, BoundKind, PairingWrite, SyncConfig};
use crate::slot::UploaderSlot;
use crate::unknown_journals::mark_spec_for_jid;
use crate::TransportError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingAction {
    Confirm,
    Reject,
    Cancel,
}

pub async fn answer(
    action: PairingAction,
    binding: &str,
    cfg: &SyncConfig,
    sync: &Arc<Mutex<SyncSnapshot>>,
    access_mutex: &tokio::sync::Mutex<Option<CredentialAccess>>,
    slot_mutex: &tokio::sync::Mutex<UploaderSlot>,
) -> Result<(), StorageError> {
    let mut slot = slot_mutex.lock().await;
    let mut access_guard = access_mutex.lock().await;

    // Stale: if binding is empty, doesn't match snapshot binding, or phase is not
    // AwaitingConfirmation, return before any file write, cache write, open_gate, kick,
    // retire, DELETE, or credential delete.
    if binding.is_empty() {
        return Ok(());
    }
    let (snapshot_binding, snapshot_phase) = {
        let snap = sync.lock().unwrap();
        (snap.pairing.binding.clone(), snap.pairing.phase)
    };
    if snapshot_binding != binding
        || snapshot_phase != observer_model::PairingPhase::AwaitingConfirmation
    {
        return Ok(());
    }

    let ans_path = answer_path(&cfg.state_path);
    let mut answer_state = crate::answer::read_answer_or_reset(&ans_path)?.unwrap_or_default();

    match action {
        PairingAction::Confirm => {
            answer_state.confirmed = binding.to_string();
            write_answer(&ans_path, &answer_state)?;
            if let Ok(mut lock) = cfg.confirmation.lock() {
                *lock = binding.to_string();
            }
            if let Some(access) = access_guard.as_ref() {
                let client = access.client_slot().load();
                if client.journal_identity().client_cert_sha256 == binding {
                    client.open_gate();
                }
                slot.kick();
                let label = client.home_label().to_string();
                let mark = mark_spec_for_jid(&client.credential().instance_id);
                publish_pairing(
                    sync,
                    &cfg.confirmation,
                    &cfg.tombstone,
                    PairingWrite::Bound {
                        binding: binding.to_string(),
                        label,
                        mark,
                        kind: BoundKind::Paired,
                    },
                );
            }
        }
        PairingAction::Reject | PairingAction::Cancel => {
            let detail = if action == PairingAction::Reject {
                MARK_REJECTED_DETAIL
            } else {
                PAIRING_CANCELLED_DETAIL
            };

            let outcome = crate::retirement::reject_pair_with(
                &cfg.state_path,
                binding,
                || async {
                    answer_state.rejected = binding.to_string();
                    let write_result =
                        write_answer(&ans_path, &answer_state).map_err(TransportError::from);
                    slot.stop().await;
                    if let Some(access) = access_guard.take() {
                        access.retire();
                    }
                    write_result
                },
                crate::retirement::retire_credential,
            )
            .await;

            match outcome {
                Ok(crate::retirement::ReconcileOutcome::Completed) => {
                    if let Ok(mut confirmation) = cfg.confirmation.lock() {
                        confirmation.clear();
                    }
                    publish_pairing(
                        sync,
                        &cfg.confirmation,
                        &cfg.tombstone,
                        PairingWrite::NotPaired {
                            detail: Some(detail.to_string()),
                        },
                    );
                }
                Ok(crate::retirement::ReconcileOutcome::Superseded) => {}
                Ok(crate::retirement::ReconcileOutcome::None) | Err(_) => {
                    publish_pairing(
                        sync,
                        &cfg.confirmation,
                        &cfg.tombstone,
                        PairingWrite::Failed {
                            detail: "client_retirement_pending".to_owned(),
                        },
                    );
                }
            }
        }
    }
    Ok(())
}

pub async fn pair<FBridge, FWin, FutBridge, FutWin>(
    link: &str,
    cfg: &SyncConfig,
    sync: Arc<Mutex<SyncSnapshot>>,
    slot_mutex: &tokio::sync::Mutex<UploaderSlot>,
    access_mutex: &tokio::sync::Mutex<Option<CredentialAccess>>,
    shutdown_bridge: FBridge,
    close_journal_window: FWin,
) -> Result<crate::credential::PairedState, TransportError>
where
    FBridge: FnOnce() -> FutBridge,
    FutBridge: std::future::Future<Output = ()>,
    FWin: FnOnce() -> FutWin,
    FutWin: std::future::Future<Output = ()>,
{
    let mut slot = slot_mutex.lock().await;
    let mut access_guard = access_mutex.lock().await;

    let ans_path = answer_path(&cfg.state_path);
    match read_answer(&ans_path) {
        Ok(Some(ans)) => {
            if let Ok(mut lock) = cfg.confirmation.lock() {
                *lock = ans.confirmed;
            }
        }
        Ok(None) => {
            let _ = crate::answer::settle_grandfather(&cfg.state_path, &cfg.confirmation);
        }
        Err(_) => {
            // Unreadable answer file is not rewritten and is not a pair failure
        }
    }

    if let Ok(None) = read_answer(&ans_path) {
        write_answer(&ans_path, &crate::answer::AnswerState::default())?;
    }

    let paired = match crate::service::pair(link, cfg, sync.clone()).await {
        Ok(paired) => paired,
        Err(error) => {
            // A failed ceremony has not replaced the live authority. Project
            // that incumbent again rather than displaying the attempt as the
            // committed pairing. publish_pairing still derives the mark gate
            // from this credential's binding and the owner's saved answer.
            if let Some(access) = access_guard.as_ref() {
                let client = access.client_slot().load();
                publish_pairing(
                    &sync,
                    &cfg.confirmation,
                    &cfg.tombstone,
                    PairingWrite::Bound {
                        binding: client.journal_identity().client_cert_sha256.clone(),
                        label: client.home_label().to_string(),
                        mark: mark_spec_for_jid(&client.credential().instance_id),
                        kind: BoundKind::Paired,
                    },
                );
            }
            return Err(error);
        }
    };

    let access = CredentialAccess::bind(&paired, cfg, sync.clone(), None)?;
    if let Some(previous) = access_guard.take() {
        previous.retire();
    }
    *access_guard = Some(access.clone());

    shutdown_bridge().await;
    close_journal_window().await;

    let wake = slot.wake();
    let access_clone = access.clone();
    let cfg_clone = cfg.clone();
    let sync_clone = sync.clone();
    slot.replace(move |rx| async move {
        crate::run_uploader(access_clone, cfg_clone, sync_clone, rx, wake).await;
    })
    .await;

    Ok(paired)
}

// The rejection test needs a real (closed) TCP endpoint: transport lane only.
#[cfg(all(test, feature = "transport-tests"))]
mod tests {
    use super::*;
    use crate::credential::{Credential, PairedState};
    use crate::journal_version::JournalVersionController;
    use crate::RawDeviceFacts;
    use observer_model::{PairingPhase, SyncSnapshot};

    #[derive(Debug)]
    struct FixedOffset;

    impl observer_model::LocalOffset for FixedOffset {
        fn local_zone(
            &self,
            _epoch_secs: u64,
        ) -> Result<observer_model::LocalZone, observer_model::LocalOffsetError> {
            Ok(observer_model::LocalZone {
                tz: Some("UTC".to_owned()),
                utc_offset_seconds: 0,
            })
        }
    }

    fn test_credential(label: &str) -> Credential {
        let generated = rcgen::generate_simple_self_signed(vec![label.to_owned()]).unwrap();
        Credential {
            client_key_pem: generated.key_pair.serialize_pem(),
            client_cert_pem: generated.cert.pem(),
            ca_chain_pem: vec![generated.cert.pem()],
            ca_fp_prefix: vec![1, 2, 3, 4],
            instance_id: "fixture-journal".to_owned(),
            home_label: "Fixture Journal".to_owned(),
            endpoints: vec![crate::credential::EndpointAddr {
                host: "127.0.0.1".to_owned(),
                port: 7657,
            }],
            relay_origin: None,
            device_token: None,
            device_token_expires_at: None,
        }
    }

    #[tokio::test]
    async fn reject_with_unreachable_journal_still_allows_fresh_pairing() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "session-reject-unreachable-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state_path = dir.join("pairing.json");
        let closed_port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let mut rejected = test_credential("rejected-journal");
        rejected.endpoints[0].port = closed_port;
        let paired = PairedState {
            credential: Some(rejected.clone()),
            ..Default::default()
        };
        paired.save(&state_path).unwrap();

        let binding = crate::ack::JournalIdentity::from_credential(&rejected).client_cert_sha256;
        let mut snapshot = SyncSnapshot::default();
        snapshot.pairing.binding = binding.clone();
        snapshot.pairing.phase = PairingPhase::AwaitingConfirmation;
        let sync = Arc::new(Mutex::new(snapshot));
        let tombstone = Arc::new(Mutex::new(None));
        let cfg = SyncConfig {
            device_label: "device".to_owned(),
            period_secs: 300,
            state_path: state_path.clone(),
            segments_root: dir.join("segments"),
            local_offset: Arc::new(FixedOffset),
            journal_version: Arc::new(JournalVersionController::new(
                dir.join("journal-version.json"),
            )),
            facts_fn: Arc::new(RawDeviceFacts::default),
            confirmation: Arc::new(Mutex::new(String::new())),
            tombstone: tombstone.clone(),
            #[cfg(feature = "awaiting-hold")]
            awaiting_hold: None,
        };
        let access = CredentialAccess::bind(&paired, &cfg, sync.clone(), None).unwrap();
        let access_mutex = tokio::sync::Mutex::new(Some(access));
        let slot = tokio::sync::Mutex::new(UploaderSlot::new());

        answer(
            PairingAction::Reject,
            &binding,
            &cfg,
            &sync,
            &access_mutex,
            &slot,
        )
        .await
        .unwrap();

        {
            let snap = sync.lock().unwrap();
            assert_eq!(snap.pairing.phase, PairingPhase::NotPaired);
            assert_eq!(snap.pairing.detail.as_deref(), Some(MARK_REJECTED_DETAIL));
        }
        assert!(access_mutex.lock().await.is_none());
        assert_eq!(tombstone.lock().unwrap().as_deref(), Some(binding.as_str()));
        let after = PairedState::load(&state_path).unwrap();
        assert!(after.credential.is_none());
        assert!(after.retirement_intent.is_none());
        assert!(CredentialAccess::bind(&after, &cfg, sync.clone(), None).is_err());

        crate::service::ensure_pairable(&state_path).unwrap();
        let fresh = test_credential("fresh-journal");
        PairedState {
            credential: Some(fresh.clone()),
            ..Default::default()
        }
        .save(&state_path)
        .unwrap();
        let loaded = PairedState::load(&state_path).unwrap().credential.unwrap();
        assert_eq!(loaded.client_cert_pem, fresh.client_cert_pem);
        assert_ne!(loaded.client_cert_pem, rejected.client_cert_pem);

        let _ = std::fs::remove_dir_all(dir);
    }
}
