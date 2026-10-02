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

const RETIRE_CLIENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

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
    let mut answer_state = read_answer(&ans_path).ok().flatten().unwrap_or_default();

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

            // 1. Write rejected = binding. On failure, log and continue. Do not blank confirmed.
            answer_state.rejected = binding.to_string();
            if let Err(e) = write_answer(&ans_path, &answer_state) {
                tracing::warn!(target: "sync", error = %e, "failed to write rejected state to answer file");
            }

            // 2. slot.stop() (Quiesce) so current tick finishes and client stays live
            slot.stop().await;

            // 3. DELETE sha256: + hex of DER from spl_transport::tls::parse_certs
            if let Some(access) = access_guard.as_ref() {
                let client = access.client_slot().load();
                if client.journal_identity().client_cert_sha256 == binding {
                    if let Ok(certs) =
                        spl_transport::tls::parse_certs(&client.credential().client_cert_pem)
                    {
                        if let Some(cert) = certs.first() {
                            let der_hex = spl_core::ca::sha256_hex(cert.as_ref());
                            let client_id = format!("sha256:{der_hex}");
                            let _ = tokio::time::timeout(
                                RETIRE_CLIENT_TIMEOUT,
                                client.retire_client(&client_id),
                            )
                            .await;
                        }
                    }
                }
            }

            // 4. CredentialAccess::retire
            if let Some(access) = access_guard.take() {
                access.retire();
            }

            // 5. Delete state_path and state_path.with_extension("json.tmp")
            let tmp = cfg.state_path.with_extension("json.tmp");
            let mut primary_deleted = true;
            if cfg.state_path.exists() && std::fs::remove_file(&cfg.state_path).is_err() {
                primary_deleted = false;
            }
            if tmp.exists() {
                let _ = std::fs::remove_file(&tmp);
            }

            // Clear rejected only when primary path is gone
            if primary_deleted {
                answer_state.rejected.clear();
                let _ = write_answer(&ans_path, &answer_state);
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
