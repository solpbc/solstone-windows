// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Pairing session coordination and confirmation gate actions.

use std::sync::{Arc, Mutex};

use observer_model::{SyncSnapshot, MARK_REJECTED_DETAIL, PAIRING_CANCELLED_DETAIL};

use crate::access::CredentialAccess;
use crate::answer::{answer_path, read_answer, write_answer};
use crate::credential::{Credential, PairedState, StorageError};
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

const RETIRE_CLIENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

pub async fn answer(
    action: PairingAction,
    binding: &str,
    cfg: &SyncConfig,
    sync: &Arc<Mutex<SyncSnapshot>>,
    access_mutex: &tokio::sync::Mutex<Option<CredentialAccess>>,
    slot_mutex: &tokio::sync::Mutex<UploaderSlot>,
    baseline_marker: Option<crate::device_marker::MarkerResult>,
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
    let mut answer_state = read_answer(&ans_path)?.unwrap_or_default();

    match action {
        PairingAction::Confirm => {
            answer_state.confirmed = binding.to_string();
            write_answer(&ans_path, &answer_state)?;
            if let Some(marker) = baseline_marker.as_ref() {
                let paired = crate::credential::PairedState::load(&cfg.state_path)?;
                crate::migration::commit_baseline_after_answer(&cfg.state_path, marker, &paired)?;
            }
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

            // The rejected digest is the durable, generation-bound local
            // invalidation intent. No remote retirement or local completion is
            // allowed until it is verified on disk.
            answer_state.rejected = binding.to_string();
            write_answer(&ans_path, &answer_state)?;

            // 2. slot.stop() (Quiesce) so current tick finishes and client stays live
            slot.stop().await;

            // 3. Retire this exact credential. A failed request leaves the
            // durable rejected digest in place for launch-time reconciliation.
            let mut retired = false;
            if let Some(access) = access_guard.as_ref() {
                let client = access.client_slot().load();
                if client.journal_identity().client_cert_sha256 == binding {
                    if let Ok(certs) =
                        spl_transport::tls::parse_certs(&client.credential().client_cert_pem)
                    {
                        if let Some(cert) = certs.first() {
                            let der_hex = spl_core::ca::sha256_hex(cert.as_ref());
                            let client_id = format!("sha256:{der_hex}");
                            retired = tokio::time::timeout(
                                RETIRE_CLIENT_TIMEOUT,
                                client.retire_client(&client_id),
                            )
                            .await
                            .is_ok_and(|result| result.is_ok());
                        }
                    }
                }
            }

            // 4. CredentialAccess::retire
            if let Some(access) = access_guard.take() {
                access.retire();
            }

            // 5. Delete the exact old owner only after remote retirement earned
            // success. Otherwise its protected credential and intent survive
            // for launch-time retry.
            let tmp = cfg.state_path.with_extension("json.tmp");
            let mut primary_deleted = false;
            if retired {
                let _guard = crate::credential::owner_state_write_guard();
                let current = crate::credential::PairedState::load(&cfg.state_path);
                let still_rejected =
                    current
                        .ok()
                        .and_then(|state| state.credential)
                        .is_some_and(|current| {
                            crate::ack::JournalIdentity::from_credential(&current)
                                .client_cert_sha256
                                == binding
                        });
                if still_rejected {
                    primary_deleted =
                        !cfg.state_path.exists() || std::fs::remove_file(&cfg.state_path).is_ok();
                    if primary_deleted && tmp.exists() {
                        primary_deleted = std::fs::remove_file(&tmp).is_ok();
                    }
                }
            }

            // Clear rejected only when primary path is gone
            if primary_deleted {
                answer_state.rejected.clear();
                let _ = write_answer(&ans_path, &answer_state);
            }

            if primary_deleted {
                publish_pairing(
                    sync,
                    &cfg.confirmation,
                    &cfg.tombstone,
                    PairingWrite::NotPaired {
                        detail: Some(detail.to_string()),
                    },
                );
            } else {
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

    let current = PairedState::load(&cfg.state_path)?;
    if current.retirement_intent.is_some() {
        return Err(TransportError::Pairing(
            "a client retirement is still pending; restart to resume it".to_owned(),
        ));
    }
    let replacing = current.credential.is_some();
    let paired = if replacing {
        publish_pairing(
            &sync,
            &cfg.confirmation,
            &cfg.tombstone,
            PairingWrite::BeginCeremony,
        );
        let result = replace_existing_pair_from_link(
            &cfg.state_path,
            link,
            &cfg.device_label,
            |link, label| async move { crate::pairing::pair_from_link(&link, &label).await },
            crate::device_marker::probe_platform,
            || async {
                slot.stop().await;
                if let Some(access) = access_guard.take() {
                    access.retire();
                }
            },
            crate::retirement::retire_credential,
        )
        .await;
        match result {
            Ok(paired) => {
                if let Ok(mut confirmation) = cfg.confirmation.lock() {
                    confirmation.clear();
                }
                cfg.journal_version.clear(&sync);
                let credential = paired
                    .credential
                    .as_ref()
                    .ok_or(TransportError::NotPaired)?;
                publish_pairing(
                    &sync,
                    &cfg.confirmation,
                    &cfg.tombstone,
                    PairingWrite::Bound {
                        binding: crate::ack::JournalIdentity::from_credential(credential)
                            .client_cert_sha256,
                        label: credential.home_label.clone(),
                        mark: mark_spec_for_jid(&credential.instance_id),
                        kind: BoundKind::Paired,
                    },
                );
                paired
            }
            Err(error) => {
                let pending = PairedState::load(&cfg.state_path)
                    .is_ok_and(|state| state.retirement_intent.is_some());
                if pending {
                    slot.stop().await;
                    if let Some(access) = access_guard.take() {
                        access.retire();
                    }
                    publish_pairing(
                        &sync,
                        &cfg.confirmation,
                        &cfg.tombstone,
                        PairingWrite::Failed {
                            detail: crate::transport_error_code(&error),
                        },
                    );
                } else if let Some(access) = access_guard.as_ref() {
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
                } else {
                    publish_pairing(
                        &sync,
                        &cfg.confirmation,
                        &cfg.tombstone,
                        PairingWrite::Failed {
                            detail: crate::transport_error_code(&error),
                        },
                    );
                }
                return Err(error);
            }
        }
    } else {
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

        match crate::service::pair(link, cfg, sync.clone()).await {
            Ok(paired) => paired,
            Err(error) => {
                return Err(error);
            }
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

async fn replace_existing_pair_from_link<P, PFut, M, B, BFut, R, RFut>(
    state_path: &std::path::Path,
    link: &str,
    device_label: &str,
    pair_candidate: P,
    marker_probe: M,
    before_retire: B,
    retire: R,
) -> Result<PairedState, TransportError>
where
    P: FnOnce(String, String) -> PFut,
    PFut: std::future::Future<Output = Result<Credential, TransportError>>,
    M: FnOnce() -> crate::device_marker::MarkerResult,
    B: FnOnce() -> BFut,
    BFut: std::future::Future<Output = ()>,
    R: FnOnce(Credential, String) -> RFut,
    RFut: std::future::Future<Output = Result<(), TransportError>>,
{
    let candidate = pair_candidate(link.to_owned(), device_label.to_owned()).await?;
    let marker = marker_probe();
    let outcome =
        crate::retirement::replace_pair_with(state_path, candidate, marker, before_retire, retire)
            .await?;
    if outcome != crate::retirement::ReconcileOutcome::Completed {
        return Err(TransportError::ReplayUnsafe);
    }
    PairedState::load(state_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential::{Credential, PairedState};
    use crate::device_marker::{DeviceMarker, MarkerConfidence, MarkerResult, MarkerSource};
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
    async fn confirmation_does_not_open_before_baseline_is_durable() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "session-baseline-failure-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state_path = dir.join("pairing.json");
        let credential = Credential {
            client_key_pem: "legacy-key".to_owned(),
            client_cert_pem: "certificate".to_owned(),
            ca_chain_pem: vec!["ca".to_owned()],
            ca_fp_prefix: vec![1, 2, 3, 4],
            instance_id: "instance".to_owned(),
            home_label: "journal".to_owned(),
            endpoints: Vec::new(),
            relay_origin: None,
            device_token: None,
            device_token_expires_at: None,
        };
        let paired = PairedState {
            credential: Some(credential.clone()),
            ..Default::default()
        };
        std::fs::write(&state_path, serde_json::to_vec(&paired).unwrap()).unwrap();
        std::fs::create_dir(crate::migration::path_for_state(&state_path)).unwrap();

        let binding = crate::ack::JournalIdentity::from_credential(&credential).client_cert_sha256;
        let mut snapshot = SyncSnapshot::default();
        snapshot.pairing.binding = binding.clone();
        snapshot.pairing.phase = PairingPhase::AwaitingConfirmation;
        let sync = Arc::new(Mutex::new(snapshot));
        let confirmation = Arc::new(Mutex::new(String::new()));
        let cfg = SyncConfig {
            device_label: "device".to_owned(),
            period_secs: 300,
            state_path: state_path.clone(),
            segments_root: dir.join("segments"),
            local_offset: Arc::new(FixedOffset),
            journal_version: Arc::new(JournalVersionController::new(
                dir.join("journal-version.json"),
            )),
            facts_fn: Arc::new(|| RawDeviceFacts {
                name: Some("device".to_owned()),
                platform: Some("windows".to_owned()),
                device_type: None,
                app_id: Some("test".to_owned()),
                app_version: Some("0.1.0".to_owned()),
            }),
            confirmation: confirmation.clone(),
            tombstone: Arc::new(Mutex::new(None)),
            #[cfg(feature = "awaiting-hold")]
            awaiting_hold: None,
        };
        let marker = MarkerResult::Available(DeviceMarker {
            digest: "a".repeat(64),
            source: MarkerSource::PublisherSystemId,
            confidence: MarkerConfidence::Primary,
        });
        let access = tokio::sync::Mutex::new(None);
        let slot = tokio::sync::Mutex::new(UploaderSlot::new());

        let result = answer(
            PairingAction::Confirm,
            &binding,
            &cfg,
            &sync,
            &access,
            &slot,
            Some(marker),
        )
        .await;

        assert!(result.is_err());
        assert!(confirmation.lock().unwrap().is_empty());
        assert_eq!(
            read_answer(&answer_path(&state_path))
                .unwrap()
                .unwrap()
                .confirmed,
            binding
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn gui_pair_replacement_persists_intent_before_retirement_and_rebinds_candidate() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "session-pair-replacement-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state_path = dir.join("pairing.json");
        let incumbent = test_credential("incumbent");
        let candidate = test_credential("candidate");
        let candidate_cert = candidate.client_cert_pem.clone();
        let old_binding =
            crate::ack::JournalIdentity::from_credential(&incumbent).client_cert_sha256;
        let old_generation = crate::credential::pairing_generation(&incumbent.client_cert_pem);
        PairedState {
            credential: Some(incumbent.clone()),
            access_mutation_generation: 4,
            ..Default::default()
        }
        .save(&state_path)
        .unwrap();
        write_answer(
            &answer_path(&state_path),
            &crate::answer::AnswerState {
                confirmed: old_binding.clone(),
                rejected: String::new(),
            },
        )
        .unwrap();
        let marker = MarkerResult::Available(DeviceMarker {
            digest: "b".repeat(64),
            source: MarkerSource::PublisherSystemId,
            confidence: MarkerConfidence::Primary,
        });
        let before_delete_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let delete_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let callback_state_path = state_path.clone();
        let callback_candidate_cert = candidate_cert.clone();
        let marker_for_probe = marker.clone();
        let expected_old_cert = incumbent.client_cert_pem.clone();
        let expected_old_cid = crate::migration::credential_cid(&incumbent).unwrap();
        let before_delete_counter = before_delete_calls.clone();
        let delete_counter = delete_calls.clone();

        let paired = replace_existing_pair_from_link(
            &state_path,
            "injected-link",
            "device",
            move |link, device| async move {
                assert_eq!(link, "injected-link");
                assert_eq!(device, "device");
                Ok(candidate)
            },
            move || marker_for_probe,
            move || async move {
                before_delete_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            },
            move |retiring, cid| async move {
                assert_eq!(retiring.client_cert_pem, expected_old_cert);
                assert_eq!(cid, expected_old_cid);
                let raw = std::fs::read_to_string(&callback_state_path).unwrap();
                assert!(raw.contains("dpapi:v1:"));
                assert!(!raw.contains("PRIVATE KEY"));
                let pending = PairedState::load(&callback_state_path).unwrap();
                assert_eq!(
                    pending.credential.unwrap().client_cert_pem,
                    retiring.client_cert_pem
                );
                let intent = PairedState::load(&callback_state_path)
                    .unwrap()
                    .retirement_intent
                    .unwrap();
                assert_eq!(intent.phase, crate::credential::RetirementPhase::Prepared);
                assert_eq!(intent.owner_generation, old_generation);
                assert_eq!(intent.access_mutation_generation, 4);
                assert_eq!(intent.candidate.client_cert_pem, callback_candidate_cert);
                assert_eq!(
                    read_answer(&answer_path(&callback_state_path))
                        .unwrap()
                        .unwrap()
                        .confirmed,
                    old_binding
                );
                delete_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            },
        )
        .await
        .unwrap();

        assert_eq!(
            before_delete_calls.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(delete_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let committed = paired.credential.unwrap();
        assert_eq!(committed.client_cert_pem, candidate_cert);
        assert_eq!(paired.access_mutation_generation, 0);
        assert!(paired.retirement_intent.is_none());
        assert_eq!(
            read_answer(&answer_path(&state_path)).unwrap().unwrap(),
            crate::answer::AnswerState::default()
        );
        let migration = crate::migration::load(&state_path).unwrap().unwrap();
        assert_eq!(
            migration.pairing_generation,
            crate::credential::pairing_generation(&candidate_cert)
        );
        assert_eq!(
            migration.baseline_marker,
            Some(match marker {
                MarkerResult::Available(marker) => marker,
                _ => unreachable!(),
            })
        );
        assert!(migration.fresh_pair_offer_pending);
        let _ = std::fs::remove_dir_all(dir);
    }
}
