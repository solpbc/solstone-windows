// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Pairing session coordination and confirmation gate actions.

use std::sync::{Arc, Mutex};

use observer_model::{
    MarkRenderSpec, SyncSnapshot, MARK_REJECTED_DETAIL, PAIRING_CANCELLED_DETAIL,
};

use crate::access::CredentialAccess;
use crate::answer::{answer_path, read_answer, write_answer};
use crate::credential::{PairedState, StorageError};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerEffect {
    Ignored,
    Confirmed,
    Closed,
}

pub async fn answer(
    action: PairingAction,
    binding: &str,
    cfg: &SyncConfig,
    sync: &Arc<Mutex<SyncSnapshot>>,
    access_mutex: &tokio::sync::Mutex<Option<CredentialAccess>>,
    slot_mutex: &tokio::sync::Mutex<UploaderSlot>,
) -> Result<AnswerEffect, StorageError> {
    let mut slot = slot_mutex.lock().await;
    let mut access_guard = access_mutex.lock().await;
    answer_locked(action, binding, cfg, sync, &mut access_guard, &mut slot).await
}

async fn answer_locked(
    action: PairingAction,
    binding: &str,
    cfg: &SyncConfig,
    sync: &Arc<Mutex<SyncSnapshot>>,
    access_guard: &mut Option<CredentialAccess>,
    slot: &mut UploaderSlot,
) -> Result<AnswerEffect, StorageError> {
    // Stale: if binding is empty, doesn't match snapshot binding, or phase is not
    // AwaitingConfirmation, return before any file write, cache write, open_gate, kick,
    // retire, DELETE, or credential delete.
    if binding.is_empty() {
        return Ok(AnswerEffect::Ignored);
    }
    let (snapshot_binding, snapshot_phase) = {
        let snap = sync.lock().unwrap();
        (snap.pairing.binding.clone(), snap.pairing.phase)
    };
    if snapshot_binding != binding
        || snapshot_phase != observer_model::PairingPhase::AwaitingConfirmation
    {
        return Ok(AnswerEffect::Ignored);
    }

    if action == PairingAction::Confirm {
        return confirm_current(binding, cfg, sync, access_guard.as_ref(), slot);
    }
    let ans_path = answer_path(&cfg.state_path);
    let mut answer_state = crate::answer::read_answer_or_reset(&ans_path)?.unwrap_or_default();

    match action {
        PairingAction::Confirm => unreachable!("confirmation returns above"),
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

            Ok(match outcome {
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
                    AnswerEffect::Closed
                }
                Ok(crate::retirement::ReconcileOutcome::Superseded) => AnswerEffect::Ignored,
                Ok(crate::retirement::ReconcileOutcome::None) | Err(_) => {
                    publish_pairing(
                        sync,
                        &cfg.confirmation,
                        &cfg.tombstone,
                        PairingWrite::Failed {
                            detail: "client_retirement_pending".to_owned(),
                        },
                    );
                    AnswerEffect::Ignored
                }
            })
        }
    }
}

/// The caller owns slot/access. Token publication precedes the durable owner
/// lock everywhere in the transport; keep that order and perform no await while
/// either synchronous guard is held.
fn confirm_current(
    binding: &str,
    cfg: &SyncConfig,
    sync: &Arc<Mutex<SyncSnapshot>>,
    access: Option<&CredentialAccess>,
    slot: &UploaderSlot,
) -> Result<AnswerEffect, StorageError> {
    let Some(access) = access else {
        return Ok(AnswerEffect::Ignored);
    };
    let clients = access.client_slot();
    let publication = clients.publication_owner();
    let _publication = publication.lock().unwrap();
    let _owner = crate::credential::owner_state_write_guard();
    let current = PairedState::load(&cfg.state_path).map_err(StorageError::Transport)?;
    let Some(credential) = current.credential.as_ref() else {
        return Ok(AnswerEffect::Ignored);
    };
    let client = clients.load();
    let current_cas = crate::credential::CasKey {
        pairing_generation: crate::credential::pairing_generation(&credential.client_cert_pem),
        access_mutation_generation: current.access_mutation_generation,
    };
    if clients.is_retired()
        || current.retirement_intent.is_some()
        || crate::ack::JournalIdentity::from_credential(credential).client_cert_sha256 != binding
        || client.journal_identity().client_cert_sha256 != binding
        || client.current_cas_key() != Some(current_cas)
    {
        return Ok(AnswerEffect::Ignored);
    }
    let mut answer_state =
        crate::answer::read_answer_or_reset_with_owner_lock(&answer_path(&cfg.state_path))?
            .unwrap_or_default();
    answer_state.confirmed = binding.to_string();
    crate::answer::write_answer_with_owner_lock(&answer_path(&cfg.state_path), &answer_state)?;
    *cfg.confirmation.lock().unwrap() = binding.to_string();
    client.open_gate();
    slot.kick();
    publish_pairing(
        sync,
        &cfg.confirmation,
        &cfg.tombstone,
        PairingWrite::Bound {
            binding: binding.to_string(),
            label: client.home_label().to_string(),
            mark: mark_spec_for_jid(&credential.instance_id),
            kind: BoundKind::Paired {
                finish_connecting: false,
                setup_browser_ready: current.setup_activation.as_ref().is_some_and(|activation| {
                    activation.binding == binding && !current.setup_browser_opened
                }),
            },
        },
    );
    Ok(AnswerEffect::Confirmed)
}

#[allow(clippy::too_many_arguments)]
pub async fn pair<FBridge, FWin, FutBridge, FutWin>(
    link: &str,
    cfg: &SyncConfig,
    sync: Arc<Mutex<SyncSnapshot>>,
    slot_mutex: &tokio::sync::Mutex<UploaderSlot>,
    access_mutex: &tokio::sync::Mutex<Option<CredentialAccess>>,
    pairing_admission: &std::sync::atomic::AtomicBool,
    shutdown_bridge: FBridge,
    close_journal_window: FWin,
) -> Result<crate::credential::PairedState, TransportError>
where
    FBridge: FnOnce() -> FutBridge,
    FutBridge: std::future::Future<Output = ()>,
    FWin: FnOnce() -> FutWin,
    FutWin: std::future::Future<Output = ()>,
{
    let data_root = cfg
        .state_path
        .parent()
        .ok_or_else(|| TransportError::Io(std::io::Error::other("missing data root")))?;
    let (_admission, ()) =
        crate::handoff::claim_pairing_admission(data_root, pairing_admission, || Ok(()))?;
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
            if settle_failed_pair(cfg, &sync, &mut access_guard, &mut slot).await {
                shutdown_bridge().await;
                close_journal_window().await;
            }
            return Err(error);
        }
    };

    let access = match CredentialAccess::bind(&paired, cfg, sync.clone(), None) {
        Ok(access) => access,
        Err(error) => {
            settle_failed_pair(cfg, &sync, &mut access_guard, &mut slot).await;
            shutdown_bridge().await;
            close_journal_window().await;
            return Err(error);
        }
    };
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

/// Full SHA-256 of `der` equals `expected_hex`. A shared 32-hex prefix is not enough.
pub fn ca_fingerprint_matches(expected_hex: &str, der: &[u8]) -> bool {
    if expected_hex.len() != 64
        || !expected_hex
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return false;
    }
    crate::credential::hex_lower(&spl_core::ca::sha256(der)) == expected_hex
}

pub(crate) fn check_native_post_dial(
    expected_ca_fingerprint: &str,
    expected_instance_id: &str,
    ca_pem: &str,
    instance_id: &str,
    identity: &crate::client::LinkIdentity,
) -> Result<MarkRenderSpec, TransportError> {
    let der = crate::pairing::pem_to_der(ca_pem)
        .ok_or_else(|| TransportError::Pairing("failed to parse CA certificate DER".to_string()))?;
    if !ca_fingerprint_matches(expected_ca_fingerprint, &der) {
        return Err(TransportError::Pairing(
            "full CA fingerprint mismatch".to_string(),
        ));
    }
    if instance_id != expected_instance_id {
        return Err(TransportError::Pairing("instance ID mismatch".to_string()));
    }
    crate::client::accept_link_identity(identity, expected_instance_id)
}

/// Binding to confirm when the saved activation matches the live certificate and identity.
///
/// This does not write.
pub fn finish_native_binding(
    paired: &PairedState,
    live_binding: &str,
    identity: &crate::client::LinkIdentity,
) -> Result<String, TransportError> {
    let activation = paired
        .setup_activation
        .as_ref()
        .ok_or_else(|| TransportError::Pairing("no setup activation".to_string()))?;
    if activation.binding != live_binding {
        return Err(TransportError::Pairing(
            "setup activation binding does not match".to_string(),
        ));
    }
    let credential = paired
        .credential
        .as_ref()
        .ok_or(TransportError::NotPaired)?;
    if paired.retirement_intent.is_some()
        || crate::ack::JournalIdentity::from_credential(credential).client_cert_sha256
            != live_binding
    {
        return Err(TransportError::NotPaired);
    }
    crate::client::accept_link_identity(identity, &credential.instance_id)?;
    Ok(activation.binding.clone())
}

/// Recover the saved native pairing without using its expired one-time link.
/// The displayed binding fences the whole operation, including the identity GET.
pub async fn finish_connecting(
    expected_binding: &str,
    cfg: &SyncConfig,
    sync: Arc<Mutex<SyncSnapshot>>,
    slot_mutex: &tokio::sync::Mutex<UploaderSlot>,
    access_mutex: &tokio::sync::Mutex<Option<CredentialAccess>>,
    pairing_admission: &std::sync::atomic::AtomicBool,
) -> Result<(), TransportError> {
    let root = cfg
        .state_path
        .parent()
        .ok_or_else(|| TransportError::Io(std::io::Error::other("missing data root")))?;
    let (_admission, ()) =
        crate::handoff::claim_pairing_admission(root, pairing_admission, || Ok(()))?;
    let mut slot = slot_mutex.lock().await;
    let mut access_guard = access_mutex.lock().await;
    let captured = PairedState::load(&cfg.state_path)?;
    let mut credential = captured
        .credential
        .clone()
        .ok_or(TransportError::NotPaired)?;
    let binding = crate::ack::JournalIdentity::from_credential(&credential).client_cert_sha256;
    if expected_binding.is_empty()
        || binding != expected_binding
        || captured.retirement_intent.is_some()
        || captured
            .setup_activation
            .as_ref()
            .is_none_or(|activation| activation.binding != binding)
    {
        return Err(TransportError::Pairing("setup connection changed".into()));
    }
    // Post-connect may have refreshed saved LAN endpoints before confirmation.
    // Recovery uses only the retained same-PC endpoints; the initial ceremony
    // still requires every candidate in the accepted link to be loopback.
    credential.endpoints.retain(|endpoint| {
        endpoint
            .host
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback())
    });
    credential.relay_origin = None;
    credential.device_token = None;
    credential.device_token_expires_at = None;
    let gate = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let client = crate::ObserverClient::new(credential, gate)?;
    let identity = client.get_link_identity().await?;
    finish_native_binding(&captured, &binding, &identity)?;

    let (access, current) = restore_native_access(&captured, cfg, &sync, &mut access_guard)?;
    let wake = slot.wake();
    let upload_cfg = cfg.clone();
    let upload_sync = sync.clone();
    slot.replace(move |cancel| async move {
        crate::run_uploader(access, upload_cfg, upload_sync, cancel, wake).await;
    })
    .await;
    // Recover the projection too: a bind failure may have left no live access.
    publish_saved_binding(&current, cfg, &sync);
    match confirm_current(&binding, cfg, &sync, access_guard.as_ref(), &slot)? {
        AnswerEffect::Confirmed => Ok(()),
        _ => Err(TransportError::Pairing("setup connection changed".into())),
    }
}

fn restore_native_access(
    captured: &PairedState,
    cfg: &SyncConfig,
    sync: &Arc<Mutex<SyncSnapshot>>,
    access: &mut Option<CredentialAccess>,
) -> Result<(CredentialAccess, PairedState), TransportError> {
    let publication = access
        .as_ref()
        .map(|access| access.client_slot().publication_owner());
    let _publication = publication.as_ref().map(|owner| owner.lock().unwrap());
    let _owner = crate::credential::owner_state_write_guard();
    let current = PairedState::load(&cfg.state_path)?;
    current
        .credential
        .as_ref()
        .ok_or(TransportError::NotPaired)?;
    if current.retirement_intent.is_some()
        || current.setup_activation != captured.setup_activation
        || current.access_mutation_generation != captured.access_mutation_generation
        || current
            .credential
            .as_ref()
            .map(|credential| &credential.client_cert_pem)
            != captured
                .credential
                .as_ref()
                .map(|credential| &credential.client_cert_pem)
    {
        return Err(TransportError::Pairing("setup connection changed".into()));
    }
    // Client construction is the fallible part of bind; shared controller
    // sessions change only after that succeeds. Retire the old publisher while
    // its publication mutex is held so queued work cannot touch this same CAS.
    let replacement = CredentialAccess::bind(&current, cfg, sync.clone(), None)?;
    if let Some(previous) = access.take() {
        previous.retire();
    }
    *access = Some(replacement.clone());
    Ok((replacement, current))
}

fn publish_saved_binding(paired: &PairedState, cfg: &SyncConfig, sync: &Arc<Mutex<SyncSnapshot>>) {
    let Some(credential) = paired.credential.as_ref() else {
        return;
    };
    let binding = crate::ack::JournalIdentity::from_credential(credential).client_cert_sha256;
    let confirmed = *cfg.confirmation.lock().unwrap() == binding;
    let native = paired
        .setup_activation
        .as_ref()
        .is_some_and(|activation| activation.binding == binding);
    publish_pairing(
        sync,
        &cfg.confirmation,
        &cfg.tombstone,
        PairingWrite::Bound {
            binding,
            label: credential.home_label.clone(),
            mark: mark_spec_for_jid(&credential.instance_id),
            kind: BoundKind::Paired {
                finish_connecting: native && !confirmed,
                setup_browser_ready: native && confirmed && !paired.setup_browser_opened,
            },
        },
    );
}

/// Restore only a still-current incumbent. A save can fail after publication;
/// in that case stop its obsolete delivery and expose the saved continuation.
async fn settle_failed_pair(
    cfg: &SyncConfig,
    sync: &Arc<Mutex<SyncSnapshot>>,
    access: &mut Option<CredentialAccess>,
    slot: &mut UploaderSlot,
) -> bool {
    let current = {
        let publication = access
            .as_ref()
            .map(|access| access.client_slot().publication_owner());
        let _publication = publication.as_ref().map(|owner| owner.lock().unwrap());
        let _owner = crate::credential::owner_state_write_guard();
        let current = PairedState::load(&cfg.state_path);
        if let (Ok(paired), Some(existing)) = (&current, access.as_ref()) {
            let clients = existing.client_slot();
            let binding = clients.load().journal_identity().client_cert_sha256;
            if !clients.is_retired()
                && paired.retirement_intent.is_none()
                && paired.credential.as_ref().is_some_and(|credential| {
                    crate::ack::JournalIdentity::from_credential(credential).client_cert_sha256
                        == binding
                })
            {
                publish_saved_binding(paired, cfg, sync);
                return false;
            }
        }
        if let Some(previous) = access.take() {
            previous.retire();
        }
        current
    };
    slot.stop().await;
    if let Ok(paired) = current {
        if paired.retirement_intent.is_none() && paired.setup_activation.is_some() {
            publish_saved_binding(&paired, cfg, sync);
            return true;
        }
    }
    publish_pairing(
        sync,
        &cfg.confirmation,
        &cfg.tombstone,
        PairingWrite::Failed {
            detail: "pairing_recovery_required".into(),
        },
    );
    true
}

#[allow(clippy::too_many_arguments)]
pub async fn connect_native_handoff<FBridge, FWin, FutBridge, FutWin>(
    handoff_id: &str,
    revision: u64,
    cfg: &SyncConfig,
    sync: Arc<Mutex<SyncSnapshot>>,
    slot_mutex: &tokio::sync::Mutex<UploaderSlot>,
    access_mutex: &tokio::sync::Mutex<Option<CredentialAccess>>,
    pairing_admission: &std::sync::atomic::AtomicBool,
    shutdown_bridge: FBridge,
    close_journal_window: FWin,
) -> Result<(), TransportError>
where
    FBridge: FnOnce() -> FutBridge,
    FutBridge: std::future::Future<Output = ()>,
    FWin: FnOnce() -> FutWin,
    FutWin: std::future::Future<Output = ()>,
{
    connect_native_handoff_with_clock(
        handoff_id,
        revision,
        cfg,
        sync,
        slot_mutex,
        access_mutex,
        pairing_admission,
        shutdown_bridge,
        close_journal_window,
        || {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn connect_native_handoff_with_clock<FBridge, FWin, FutBridge, FutWin, FClock>(
    handoff_id: &str,
    revision: u64,
    cfg: &SyncConfig,
    sync: Arc<Mutex<SyncSnapshot>>,
    slot_mutex: &tokio::sync::Mutex<UploaderSlot>,
    access_mutex: &tokio::sync::Mutex<Option<CredentialAccess>>,
    pairing_admission: &std::sync::atomic::AtomicBool,
    shutdown_bridge: FBridge,
    close_journal_window: FWin,
    clock: FClock,
) -> Result<(), TransportError>
where
    FClock: Fn() -> u64,
    FBridge: FnOnce() -> FutBridge,
    FutBridge: std::future::Future<Output = ()>,
    FWin: FnOnce() -> FutWin,
    FutWin: std::future::Future<Output = ()>,
{
    let data_root = cfg.state_path.parent().ok_or_else(|| {
        TransportError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "missing data root",
        ))
    })?;
    let (_admission, progress) =
        crate::handoff::claim_pairing_admission(data_root, pairing_admission, || {
            let progress = match crate::handoff::read_progress(data_root) {
                Ok(Some(progress)) => progress,
                Ok(None) => {
                    return Err(TransportError::Pairing(
                        "no handoff progress found".to_string(),
                    ))
                }
                Err(error) => return Err(TransportError::Io(error)),
            };
            let now_unix_ms = clock();
            if progress.is_expired(now_unix_ms) {
                return Err(TransportError::Pairing(
                    "handoff progress has expired".to_string(),
                ));
            }
            if progress.handoff_id != handoff_id || progress.accepted_unix_ms != revision {
                return Err(TransportError::Pairing(
                    "handoff id or revision does not match accepted progress".to_string(),
                ));
            }
            if PairedState::setup_handoff_history(&cfg.state_path)?.contains(&progress.handoff_id) {
                return Err(TransportError::Pairing(
                    "setup handoff already finished".into(),
                ));
            }
            Ok(progress)
        })?;

    let mut slot = slot_mutex.lock().await;
    let mut access_guard = access_mutex.lock().await;

    if progress.is_expired(clock()) {
        return Err(TransportError::Pairing(
            "handoff progress has expired".into(),
        ));
    }
    crate::service::ensure_pairable(&cfg.state_path)?;

    publish_pairing(
        &sync,
        &cfg.confirmation,
        &cfg.tombstone,
        PairingWrite::BeginCeremony,
    );

    let result: Result<(PairedState, String, Option<MarkRenderSpec>, String), TransportError> =
        async {
            // 1. Parse
            let parsed = spl_core::pairlink::parse(&progress.pairing_link)
                .map_err(|e| TransportError::PairLink(e.to_string()))?;

            // 2. Direct loopback candidates only
            let direct = match parsed {
                spl_core::pairlink::ParsedPairLink::Direct(d) => d,
                spl_core::pairlink::ParsedPairLink::Relay(_) => {
                    return Err(TransportError::NoEndpoint);
                }
            };
            if direct.candidates.is_empty() {
                return Err(TransportError::NoEndpoint);
            }
            for candidate in &direct.candidates {
                match candidate.host.parse::<std::net::Ipv4Addr>() {
                    Ok(ip) if ip.is_loopback() => {}
                    _ => return Err(TransportError::NoEndpoint),
                }
            }

            // 3. 16-byte prefix of expected_ca_fingerprint
            if progress.expected_ca_fingerprint.len() != 64 {
                return Err(TransportError::Pairing(
                    "malformed expected CA fingerprint".to_string(),
                ));
            }
            let expected_prefix = progress.expected_ca_fingerprint.get(..32).ok_or_else(|| {
                TransportError::Pairing("malformed expected CA fingerprint".to_string())
            })?;
            let direct_prefix_hex = crate::credential::hex_lower(&direct.ca_fp_prefix);
            if !direct_prefix_hex.eq_ignore_ascii_case(expected_prefix) {
                return Err(TransportError::Pairing(
                    "CA fingerprint prefix mismatch".to_string(),
                ));
            }

            // 4. In-memory pairing::pair_from_link
            let credential =
                crate::pairing::pair_from_link(&progress.pairing_link, &cfg.device_label).await?;

            let ca_pem = credential
                .ca_chain_pem
                .first()
                .ok_or_else(|| TransportError::Pairing("empty CA chain".to_string()))?;
            let gate = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let identity_client = crate::ObserverClient::new(credential.clone(), gate)?;
            let identity = identity_client.get_link_identity().await?;
            let mark = check_native_post_dial(
                &progress.expected_ca_fingerprint,
                &progress.expected_instance_id,
                ca_pem,
                &credential.instance_id,
                &identity,
            )?;

            // 8. Only after all checks: existing fresh-pair record_fresh_pair_offer, then PairedState::save
            let fresh_pair = crate::credential::PairedState::load(&cfg.state_path)
                .map_or(true, |paired| !paired.is_paired());
            if fresh_pair {
                crate::migration::record_fresh_pair_offer(&cfg.state_path, &credential)?;
            }
            let binding =
                crate::ack::JournalIdentity::from_credential(&credential).client_cert_sha256;
            let paired = crate::credential::PairedState {
                credential: Some(credential.clone()),
                setup_activation: Some(crate::credential::SetupActivation {
                    handoff_id: progress.handoff_id.clone(),
                    binding: binding.clone(),
                }),
                setup_browser_opened: false,
                ..Default::default()
            };
            paired.save(&cfg.state_path)?;

            let journal_label = credential.home_label.clone();
            Ok((paired, journal_label, Some(mark), binding))
        }
        .await;

    let (paired, journal_label, mark, binding) = match result {
        Ok(t) => t,
        Err(e) => {
            if settle_failed_pair(cfg, &sync, &mut access_guard, &mut slot).await {
                shutdown_bridge().await;
                close_journal_window().await;
            }
            return Err(e);
        }
    };

    let access = match CredentialAccess::bind(&paired, cfg, sync.clone(), None) {
        Ok(access) => access,
        Err(e) => {
            settle_failed_pair(cfg, &sync, &mut access_guard, &mut slot).await;
            shutdown_bridge().await;
            close_journal_window().await;
            return Err(e);
        }
    };
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

    publish_pairing(
        &sync,
        &cfg.confirmation,
        &cfg.tombstone,
        PairingWrite::Bound {
            binding: binding.clone(),
            label: journal_label,
            mark,
            kind: BoundKind::Paired {
                finish_connecting: true,
                setup_browser_ready: false,
            },
        },
    );

    match answer_locked(
        PairingAction::Confirm,
        &binding,
        cfg,
        &sync,
        &mut access_guard,
        &mut slot,
    )
    .await
    {
        Ok(AnswerEffect::Confirmed) => Ok(()),
        Ok(_) => Err(TransportError::Pairing(
            "native handoff confirmation was ignored".to_string(),
        )),
        Err(error) => Err(TransportError::from(error)),
    }
}

#[cfg(test)]
mod pure_session_tests {
    use super::*;
    use crate::handoff::SetupHandoffProgress;
    use crate::journal_version::JournalVersionController;
    use crate::RawDeviceFacts;
    use observer_model::{LocalOffset, LocalOffsetError, LocalZone, SyncSnapshot};

    #[derive(Debug)]
    struct FixedOffset;

    impl LocalOffset for FixedOffset {
        fn local_zone(&self, _epoch_secs: u64) -> Result<LocalZone, LocalOffsetError> {
            Ok(LocalZone {
                tz: Some("UTC".to_owned()),
                utc_offset_seconds: 0,
            })
        }
    }

    pub(super) struct TestRoot(std::path::PathBuf);
    impl TestRoot {
        pub(super) fn new() -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root =
                std::env::temp_dir().join(format!("session-caller-{}-{nonce}", std::process::id()));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
        pub(super) fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub(super) fn test_config(root: &std::path::Path) -> SyncConfig {
        SyncConfig {
            device_label: "device".into(),
            period_secs: 300,
            state_path: root.join("pairing.json"),
            segments_root: root.join("segments"),
            local_offset: Arc::new(FixedOffset),
            journal_version: Arc::new(JournalVersionController::new(
                root.join("journal-version.json"),
            )),
            facts_fn: Arc::new(RawDeviceFacts::default),
            confirmation: Arc::new(Mutex::new(String::new())),
            tombstone: Arc::new(Mutex::new(None)),
            #[cfg(feature = "awaiting-hold")]
            awaiting_hold: None,
        }
    }

    #[tokio::test]
    async fn native_connect_rechecks_deadline_after_each_session_lock_wait() {
        use std::future::Future;
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        for wait_for_access in [false, true] {
            let root = TestRoot::new();
            let cfg = test_config(root.path());
            let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
            let progress = SetupHandoffProgress {
                schema: observer_contract::PROGRESS_SCHEMA.into(),
                handoff_id: "11111111-1111-4111-8111-111111111111".into(),
                pairing_link: "https://go.solstone.app/p#0G0W000258DSX8DJRFAEBXG7308J4CT4ANK7F26YNPZEZJQYQAZ028T5CY4TQKFF".into(),
                expected_ca_fingerprint: "ab".repeat(32),
                expected_instance_id: "inst".into(),
                accepted_unix_ms: 1_000,
                expires_at_unix_ms: 2_000,
            };
            let path = root.path().join(observer_contract::PROGRESS_FILE_NAME);
            let before = serde_json::to_vec(&progress).unwrap();
            std::fs::write(&path, &before).unwrap();
            let slot = tokio::sync::Mutex::new(UploaderSlot::new());
            let access = tokio::sync::Mutex::new(None);
            let slot_guard = if wait_for_access {
                None
            } else {
                Some(slot.lock().await)
            };
            let access_guard = if wait_for_access {
                Some(access.lock().await)
            } else {
                None
            };
            let clock = AtomicU64::new(1_000);
            let admission = AtomicBool::new(false);
            let mut connect = Box::pin(connect_native_handoff_with_clock(
                &progress.handoff_id,
                progress.accepted_unix_ms,
                &cfg,
                sync.clone(),
                &slot,
                &access,
                &admission,
                || async { panic!("expired link closed incumbent bridge") },
                || async { panic!("expired link closed incumbent window") },
                || clock.load(Ordering::Acquire),
            ));
            std::future::poll_fn(|cx| {
                assert!(connect.as_mut().poll(cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            assert!(admission.load(Ordering::Acquire));
            clock.store(2_000, Ordering::Release);
            drop(slot_guard);
            drop(access_guard);
            let error = connect.await.unwrap_err();
            assert!(
                matches!(error, TransportError::Pairing(ref detail) if detail == "handoff progress has expired")
            );
            assert!(!admission.load(Ordering::Acquire));
            assert!(!cfg.state_path.exists());
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert_eq!(*sync.lock().unwrap(), SyncSnapshot::default());
        }
    }

    #[tokio::test]
    async fn native_admission_survives_delayed_bound_while_waiting_and_releases_on_cancel() {
        use std::future::Future;
        use std::sync::atomic::{AtomicBool, Ordering};
        let root = TestRoot::new();
        let cfg = test_config(root.path());
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let progress = SetupHandoffProgress {
            expires_at_unix_ms: u64::MAX,
            schema: observer_contract::PROGRESS_SCHEMA.into(),
            handoff_id: "11111111-1111-4111-8111-111111111111".into(),
            pairing_link: "https://go.solstone.app/p#0R0J6HB7H6NWVVR1VTPVXVYAZTXBW0938NKRKAYDXWAPGX3ME1SKMBSFE9JPRRBS5SJQGRBDE1P6A".into(),
            expected_ca_fingerprint: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            expected_instance_id: "inst".into(), accepted_unix_ms: now,
        };
        let progress_path = root.path().join(observer_contract::PROGRESS_FILE_NAME);
        let before = serde_json::to_vec(&progress).unwrap();
        std::fs::write(&progress_path, &before).unwrap();
        let slot = tokio::sync::Mutex::new(UploaderSlot::new());
        let held = slot.lock().await;
        let access = tokio::sync::Mutex::new(None);
        let admission = AtomicBool::new(false);
        let mut native = Box::pin(connect_native_handoff(
            &progress.handoff_id,
            now,
            &cfg,
            sync.clone(),
            &slot,
            &access,
            &admission,
            || async {},
            || async {},
        ));
        std::future::poll_fn(|cx| {
            assert!(native.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        assert!(admission.load(Ordering::Acquire));
        publish_pairing(
            &sync,
            &cfg.confirmation,
            &cfg.tombstone,
            PairingWrite::Bound {
                binding: "incumbent".into(),
                label: "incumbent".into(),
                mark: None,
                kind: BoundKind::Paired {
                    finish_connecting: false,
                    setup_browser_ready: false,
                },
            },
        );
        let mailbox = crate::handoff::SetupHandoffMailbox {
            expires_at_unix_ms: u64::MAX,
            schema: observer_contract::MAILBOX_SCHEMA.into(),
            handoff_id: "22222222-2222-4222-8222-222222222222".into(),
            step: observer_contract::HANDOFF_REQUIRED_STEP,
            expected_instance_id: progress.expected_instance_id.clone(),
            expected_ca_fingerprint: progress.expected_ca_fingerprint.clone(),
            pairing_link: progress.pairing_link.clone(),
        };
        std::fs::write(
            root.path().join(observer_contract::MAILBOX_FILE_NAME),
            serde_json::to_vec(&mailbox).unwrap(),
        )
        .unwrap();
        let expired = now + observer_contract::UNPAIRED_DRAFT_MAX_AGE_SECS * 1000;
        assert_eq!(
            crate::handoff::process_mailbox_live(
                root.path(),
                || expired,
                || admission.load(Ordering::Acquire)
            )
            .unwrap(),
            crate::handoff::MailboxOutcome::Deferred
        );
        assert_eq!(std::fs::read(&progress_path).unwrap(), before);
        drop(native);
        assert!(!admission.load(Ordering::Acquire));
        drop(held);
        assert_eq!(
            crate::handoff::process_mailbox_live(
                root.path(),
                || expired,
                || admission.load(Ordering::Acquire)
            )
            .unwrap(),
            crate::handoff::MailboxOutcome::Accepted
        );
        assert_eq!(
            crate::handoff::read_progress(root.path())
                .unwrap()
                .unwrap()
                .handoff_id,
            mailbox.handoff_id
        );
    }

    #[tokio::test]
    async fn matching_confirmation_without_access_creates_no_answer() {
        let root = TestRoot::new();
        let cfg = test_config(root.path());
        let binding = "a".repeat(64);
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        {
            let mut sync = sync.lock().unwrap();
            sync.pairing.binding = binding.clone();
            sync.pairing.phase = observer_model::PairingPhase::AwaitingConfirmation;
        }
        let access = tokio::sync::Mutex::new(None);
        let slot = tokio::sync::Mutex::new(UploaderSlot::new());
        assert_eq!(
            answer(
                PairingAction::Confirm,
                &binding,
                &cfg,
                &sync,
                &access,
                &slot
            )
            .await
            .unwrap(),
            AnswerEffect::Ignored
        );
        assert!(!answer_path(&cfg.state_path).exists());
        assert!(cfg.confirmation.lock().unwrap().is_empty());
        std::fs::write(answer_path(&cfg.state_path), b"corrupt answer evidence").unwrap();
        assert_eq!(
            answer(
                PairingAction::Confirm,
                &binding,
                &cfg,
                &sync,
                &access,
                &slot
            )
            .await
            .unwrap(),
            AnswerEffect::Ignored
        );
        assert_eq!(
            std::fs::read(answer_path(&cfg.state_path)).unwrap(),
            b"corrupt answer evidence"
        );
    }

    #[tokio::test]
    async fn connect_native_handoff_refuses_relay_candidate_before_dial() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "session-refuse-relay-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state_path = dir.join("pairing.json");

        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
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
            tombstone: Arc::new(Mutex::new(None)),
            #[cfg(feature = "awaiting-hold")]
            awaiting_hold: None,
        };
        let access_mutex = tokio::sync::Mutex::new(None);
        let slot = tokio::sync::Mutex::new(UploaderSlot::new());

        let relay_link = "https://go.solstone.app/p#0R0J6HB7H6NWVVR1VTPVXVYAZTXBW0938NKRKAYDXWAPGX3ME1SKMBSFE9JPRRBS5SJQGRBDE1P6A";
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let progress = SetupHandoffProgress {
            expires_at_unix_ms: u64::MAX,
            schema: observer_contract::PROGRESS_SCHEMA.to_string(),
            handoff_id: "11111111-1111-4111-8111-111111111111".to_string(),
            pairing_link: relay_link.to_string(),
            expected_ca_fingerprint:
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
            expected_instance_id: "inst".to_string(),
            accepted_unix_ms: now_ms,
        };
        let progress_bytes = serde_json::to_vec_pretty(&progress).unwrap();
        std::fs::write(
            dir.join(observer_contract::PROGRESS_FILE_NAME),
            progress_bytes,
        )
        .unwrap();

        let err = connect_native_handoff(
            &progress.handoff_id,
            progress.accepted_unix_ms,
            &cfg,
            sync.clone(),
            &slot,
            &access_mutex,
            &std::sync::atomic::AtomicBool::new(false),
            || async {},
            || async {},
        )
        .await
        .unwrap_err();
        assert!(matches!(err, TransportError::NoEndpoint));
        assert!(
            !state_path.exists(),
            "pairing file must not be created on refusal"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn connect_native_handoff_refuses_non_loopback_candidate_before_dial() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "session-refuse-non-loopback-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state_path = dir.join("pairing.json");

        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
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
            tombstone: Arc::new(Mutex::new(None)),
            #[cfg(feature = "awaiting-hold")]
            awaiting_hold: None,
        };
        let access_mutex = tokio::sync::Mutex::new(None);
        let slot = tokio::sync::Mutex::new(UploaderSlot::new());

        // 192.0.2.42 candidate
        let direct_link = "https://go.solstone.app/p#0G0W000258DSX8DJRFAEBXG7308J4CT4ANK7F26YNPZEZJQYQAZ028T5CY4TQKFF";
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let progress = SetupHandoffProgress {
            expires_at_unix_ms: u64::MAX,
            schema: observer_contract::PROGRESS_SCHEMA.to_string(),
            handoff_id: "11111111-1111-4111-8111-111111111111".to_string(),
            pairing_link: direct_link.to_string(),
            expected_ca_fingerprint:
                "deadbeefcafebabe0123456789abcdef00000000000000000000000000000000".to_string(),
            expected_instance_id: "inst".to_string(),
            accepted_unix_ms: now_ms,
        };
        let progress_bytes = serde_json::to_vec_pretty(&progress).unwrap();
        std::fs::write(
            dir.join(observer_contract::PROGRESS_FILE_NAME),
            progress_bytes,
        )
        .unwrap();

        let err = connect_native_handoff(
            &progress.handoff_id,
            progress.accepted_unix_ms,
            &cfg,
            sync.clone(),
            &slot,
            &access_mutex,
            &std::sync::atomic::AtomicBool::new(false),
            || async {},
            || async {},
        )
        .await
        .unwrap_err();
        assert!(matches!(&err, TransportError::NoEndpoint));
        assert!(
            !matches!(
                &err,
                TransportError::Pairing(msg)
                    if msg == "handoff id or revision does not match accepted progress"
            ),
            "matching id and revision is not a fence refusal"
        );
        assert!(
            !state_path.exists(),
            "pairing file must not be created on refusal"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn connect_native_handoff_refuses_expired_progress_before_dial() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "session-refuse-expired-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state_path = dir.join("pairing.json");

        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
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
            tombstone: Arc::new(Mutex::new(None)),
            #[cfg(feature = "awaiting-hold")]
            awaiting_hold: None,
        };
        let access_mutex = tokio::sync::Mutex::new(None);
        let slot = tokio::sync::Mutex::new(UploaderSlot::new());

        let direct_link = "https://go.solstone.app/p#0G0W000258DSX8DJRFAEBXG7308J4CT4ANK7F26YNPZEZJQYQAZ028T5CY4TQKFF";
        let progress = SetupHandoffProgress {
            expires_at_unix_ms: u64::MAX,
            schema: observer_contract::PROGRESS_SCHEMA.to_string(),
            handoff_id: "11111111-1111-4111-8111-111111111111".to_string(),
            pairing_link: direct_link.to_string(),
            expected_ca_fingerprint:
                "deadbeefcafebabe0123456789abcdef00000000000000000000000000000000".to_string(),
            expected_instance_id: "inst".to_string(),
            accepted_unix_ms: 1000,
        };
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        for absolute_deadline in [false, true] {
            let mut progress = progress.clone();
            if absolute_deadline {
                progress.accepted_unix_ms = now_ms;
                progress.expires_at_unix_ms = now_ms.saturating_sub(1);
            }
            let progress_bytes = serde_json::to_vec_pretty(&progress).unwrap();
            std::fs::write(
                dir.join(observer_contract::PROGRESS_FILE_NAME),
                progress_bytes,
            )
            .unwrap();

            let err = connect_native_handoff(
                &progress.handoff_id,
                progress.accepted_unix_ms,
                &cfg,
                sync.clone(),
                &slot,
                &access_mutex,
                &std::sync::atomic::AtomicBool::new(false),
                || async {},
                || async {},
            )
            .await
            .unwrap_err();
            assert!(
                matches!(err, TransportError::Pairing(ref msg) if msg == "handoff progress has expired")
            );
            assert!(
                !state_path.exists(),
                "pairing file must not be created on refusal"
            );

            assert_eq!(crate::handoff::read_progress(&dir).unwrap(), Some(progress));
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn connect_native_handoff_refuses_mismatched_link_before_dial() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "session-refuse-mismatch-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state_path = dir.join("pairing.json");

        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
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
            tombstone: Arc::new(Mutex::new(None)),
            #[cfg(feature = "awaiting-hold")]
            awaiting_hold: None,
        };
        let access_mutex = tokio::sync::Mutex::new(None);
        let slot = tokio::sync::Mutex::new(UploaderSlot::new());

        let direct_link = "https://go.solstone.app/p#0G0W000258DSX8DJRFAEBXG7308J4CT4ANK7F26YNPZEZJQYQAZ028T5CY4TQKFF";
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let progress = SetupHandoffProgress {
            expires_at_unix_ms: u64::MAX,
            schema: observer_contract::PROGRESS_SCHEMA.to_string(),
            handoff_id: "22222222-2222-4222-8222-222222222222".to_string(),
            pairing_link: direct_link.to_string(),
            expected_ca_fingerprint:
                "deadbeefcafebabe0123456789abcdef00000000000000000000000000000000".to_string(),
            expected_instance_id: "inst".to_string(),
            accepted_unix_ms: now_ms,
        };
        let progress_bytes = serde_json::to_vec_pretty(&progress).unwrap();
        std::fs::write(
            dir.join(observer_contract::PROGRESS_FILE_NAME),
            progress_bytes,
        )
        .unwrap();

        let err = connect_native_handoff(
            "11111111-1111-4111-8111-111111111111",
            progress.accepted_unix_ms,
            &cfg,
            sync.clone(),
            &slot,
            &access_mutex,
            &std::sync::atomic::AtomicBool::new(false),
            || async {},
            || async {},
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, TransportError::Pairing(ref msg) if msg == "handoff id or revision does not match accepted progress")
        );
        assert!(
            !state_path.exists(),
            "pairing file must not be created on refusal"
        );
        assert!(!crate::answer::answer_path(&state_path).exists());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn ordinary_equal_link_is_not_a_native_function() {
        let source = include_str!("session.rs");
        assert!(!source.contains(&["classify", "_submit"].concat()));
        assert!(!source.contains(&["pair_native", "_handoff"].concat()));
    }

    #[tokio::test]
    async fn answer_empty_or_mismatched_binding_is_ignored() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "session-answer-ignored-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state_path = dir.join("pairing.json");
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        {
            let mut snap = sync.lock().unwrap();
            snap.pairing.phase = observer_model::PairingPhase::AwaitingConfirmation;
            snap.pairing.binding = "expected-binding".to_string();
        }
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
            tombstone: Arc::new(Mutex::new(None)),
            #[cfg(feature = "awaiting-hold")]
            awaiting_hold: None,
        };
        let access_mutex = tokio::sync::Mutex::new(None);
        let slot = tokio::sync::Mutex::new(UploaderSlot::new());

        let empty = answer(
            PairingAction::Confirm,
            "",
            &cfg,
            &sync,
            &access_mutex,
            &slot,
        )
        .await
        .unwrap();
        let mismatched = answer(
            PairingAction::Confirm,
            "other-binding",
            &cfg,
            &sync,
            &access_mutex,
            &slot,
        )
        .await
        .unwrap();
        assert_eq!(empty, AnswerEffect::Ignored);
        assert_eq!(mismatched, AnswerEffect::Ignored);
        assert_ne!(empty, AnswerEffect::Confirmed);
        assert!(!crate::answer::answer_path(&state_path).exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn same_prefix_different_full_hash_is_not_a_fingerprint_match() {
        let generated = rcgen::generate_simple_self_signed(vec!["ca.example".to_string()]).unwrap();
        let der = generated.cert.der();
        let full = crate::credential::hex_lower(&spl_core::ca::sha256(der.as_ref()));
        let mut mismatched = full.clone();
        mismatched.replace_range(32..64, "00000000000000000000000000000000");
        assert_eq!(&full[..32], &mismatched[..32]);
        assert!(ca_fingerprint_matches(&full, der.as_ref()));
        assert!(!ca_fingerprint_matches(&mismatched, der.as_ref()));

        let err = check_native_post_dial(
            &mismatched,
            "inst",
            &generated.cert.pem(),
            "inst",
            &crate::client::LinkIdentity {
                committed: true,
                instance_id: Some("inst".to_string()),
                mark: None,
            },
        )
        .unwrap_err();
        assert!(
            matches!(err, TransportError::Pairing(ref msg) if msg == "full CA fingerprint mismatch")
        );
    }

    #[test]
    fn finish_native_binding_accepts_only_the_matching_binding_and_mark() {
        let jid = "f30ed159-ef46-8e9c-913f-e49f0fe7d201";
        let mark = crate::unknown_journals::mark_spec_for_jid(jid).expect("valid journal id");
        let credential = crate::credential::Credential {
            client_key_pem: "key".into(),
            client_cert_pem: "cert".into(),
            ca_chain_pem: vec!["ca".into()],
            ca_fp_prefix: vec![1, 2, 3, 4],
            instance_id: jid.to_string(),
            home_label: "Home".into(),
            endpoints: vec![crate::credential::EndpointAddr {
                host: "127.0.0.1".into(),
                port: 1,
            }],
            relay_origin: None,
            device_token: None,
            device_token_expires_at: None,
        };
        let binding = crate::ack::JournalIdentity::from_credential(&credential).client_cert_sha256;
        let paired = PairedState {
            credential: Some(credential),
            setup_activation: Some(crate::credential::SetupActivation {
                handoff_id: "11111111-1111-4111-8111-111111111111".into(),
                binding: binding.clone(),
            }),
            ..Default::default()
        };
        let good = crate::client::LinkIdentity {
            committed: true,
            instance_id: Some(jid.to_string()),
            mark: Some(mark),
        };
        let bad_mark = crate::client::LinkIdentity {
            committed: true,
            instance_id: Some(jid.to_string()),
            mark: None,
        };
        assert!(finish_native_binding(&paired, &binding, &bad_mark).is_err());
        let mut other = paired.clone();
        other.setup_activation.as_mut().unwrap().binding = "other-binding".into();
        assert!(finish_native_binding(&other, &binding, &good).is_err());
        assert_eq!(
            finish_native_binding(&paired, &binding, &good).unwrap(),
            binding
        );
    }
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

    use rcgen::{
        BasicConstraints, CertificateParams, CertificateSigningRequestParams,
        ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose, PKCS_ECDSA_P256_SHA256,
    };
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use rustls::ServerConfig;
    use spl_core::frame::{Frame, FrameDecoder, FLAG_CLOSE, FLAG_DATA};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;

    fn direct_loopback_link(port: u16, ca_fp: &[u8; 16]) -> String {
        let mut blob = Vec::with_capacity(40);
        blob.push(4);
        blob.push(1);
        blob.extend_from_slice(&[127, 0, 0, 1]);
        blob.extend_from_slice(&port.to_be_bytes());
        blob.extend_from_slice(&[0x11; 16]);
        blob.extend_from_slice(ca_fp);
        let fragment = spl_core::crockford::encode(&blob);
        format!("https://go.solstone.app/p#{fragment}")
    }

    async fn serve_one_pair_response(
        listener: TcpListener,
        acceptor: TlsAcceptor,
        signing_cert: rcgen::Certificate,
        signing_key: KeyPair,
    ) {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut tls = acceptor.accept(tcp).await.unwrap();
        let mut decoder = FrameDecoder::new();
        let mut request = Vec::new();
        let mut stream_id = 1u32;
        let mut buf = [0u8; 4096];
        let mut closed = false;
        while !closed {
            let n = tls.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            decoder.feed(&buf[..n]);
            for frame in decoder.drain().unwrap() {
                stream_id = frame.stream_id;
                if frame.flags & FLAG_DATA != 0 {
                    request.extend_from_slice(&frame.payload);
                }
                if frame.flags & FLAG_CLOSE != 0 {
                    closed = true;
                }
            }
        }

        let body_str = String::from_utf8_lossy(&request);
        let body_idx = body_str.find("\r\n\r\n").unwrap() + 4;
        let body_json: serde_json::Value = serde_json::from_str(&body_str[body_idx..]).unwrap();
        let csr = body_json["csr"].as_str().unwrap();

        let client_cert = CertificateSigningRequestParams::from_pem(csr)
            .unwrap()
            .signed_by(&signing_cert, &signing_key)
            .unwrap();

        let response_body = serde_json::json!({
            "client_cert": client_cert.pem(),
            "ca_chain": [signing_cert.pem()],
            "instance_id": "fixture-journal",
            "home_label": "Fixture Journal",
            "fingerprint": format!("sha256:{}", spl_core::ca::sha256_hex(client_cert.der())),
        });
        let body_bytes = serde_json::to_vec(&response_body).unwrap();
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body_bytes.len()
        );
        let mut full = head.into_bytes();
        full.extend_from_slice(&body_bytes);

        let frame = Frame::new(stream_id, FLAG_DATA | FLAG_CLOSE, full);
        tls.write_all(&frame.encode().unwrap()).await.unwrap();
        tls.flush().await.unwrap();
        let mut drain = [0u8; 1024];
        while tls.read(&mut drain).await.unwrap_or(0) != 0 {}
    }

    #[tokio::test]
    async fn partial_pair_save_closes_old_bridges_but_prewrite_failure_preserves_them() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        for failpoint in [1, 2] {
            let root = super::pure_session_tests::TestRoot::new();
            let cfg = super::pure_session_tests::test_config(root.path());
            let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
            let incumbent = PairedState {
                credential: Some(test_credential("incumbent")),
                ..Default::default()
            };
            incumbent.save(&cfg.state_path).unwrap();
            let old_binding = crate::ack::JournalIdentity::from_credential(
                incumbent.credential.as_ref().unwrap(),
            )
            .client_cert_sha256;
            *cfg.confirmation.lock().unwrap() = old_binding.clone();
            write_answer(
                &answer_path(&cfg.state_path),
                &crate::answer::AnswerState {
                    confirmed: old_binding.clone(),
                    rejected: String::new(),
                },
            )
            .unwrap();
            let old_access = CredentialAccess::bind(&incumbent, &cfg, sync.clone(), None).unwrap();
            let mut handles = Vec::new();
            for _ in 0..2 {
                handles.push(
                    crate::journal_bridge::start_with_facts(old_access.clone())
                        .await
                        .unwrap(),
                );
            }
            let ports: Vec<_> = handles.iter().map(|handle| handle.port()).collect();
            let bridges = Arc::new(Mutex::new(handles));
            let shutdown_bridges = bridges.clone();
            let closed = AtomicUsize::new(0);
            let access = tokio::sync::Mutex::new(Some(old_access));
            let slot = tokio::sync::Mutex::new(UploaderSlot::new());
            let (signing_cert, signing_key) = {
                let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
                params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
                params.key_usages = vec![
                    rcgen::KeyUsagePurpose::DigitalSignature,
                    rcgen::KeyUsagePurpose::KeyCertSign,
                ];
                let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
                (params.self_signed(&key).unwrap(), key)
            };
            let tls_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
            let tls_cert = CertificateParams::new(vec!["spl.local".into()])
                .unwrap()
                .self_signed(&tls_key)
                .unwrap();
            let pin: [u8; 16] = spl_core::ca::sha256(tls_cert.der())[..16]
                .try_into()
                .unwrap();
            let server_cfg = ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(tls_cert.der().to_vec())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(tls_key.serialize_der())),
            )
            .unwrap();
            let acceptor = TlsAcceptor::from(Arc::new(server_cfg));
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let link = direct_loopback_link(listener.local_addr().unwrap().port(), &pin);
            let server = tokio::spawn(serve_one_pair_response(
                listener,
                acceptor,
                signing_cert,
                signing_key,
            ));
            crate::credential::FS_FAIL_POINT.with(|point| point.set(failpoint));
            let result = pair(
                &link,
                &cfg,
                sync.clone(),
                &slot,
                &access,
                &AtomicBool::new(false),
                move || async move {
                    let handles = std::mem::take(&mut *shutdown_bridges.lock().unwrap());
                    for handle in handles {
                        handle.shutdown_and_wait().await;
                    }
                },
                || async {
                    closed.fetch_add(1, Ordering::SeqCst);
                },
            )
            .await;
            crate::credential::FS_FAIL_POINT.with(|point| point.set(0));
            assert!(result.is_err());
            let changed = failpoint == 2;
            assert_eq!(closed.load(Ordering::SeqCst), usize::from(changed));
            assert_eq!(access.lock().await.is_none(), changed);
            for port in ports {
                assert_eq!(
                    tokio::net::TcpStream::connect(("127.0.0.1", port))
                        .await
                        .is_err(),
                    changed
                );
            }
            if !changed {
                assert_eq!(
                    PairedState::load(&cfg.state_path)
                        .unwrap()
                        .credential
                        .unwrap()
                        .client_cert_pem,
                    incumbent.credential.unwrap().client_cert_pem
                );
            }
            let handles = std::mem::take(&mut *bridges.lock().unwrap());
            for handle in handles {
                handle.shutdown_and_wait().await;
            }
            tokio::time::timeout(std::time::Duration::from_secs(5), server)
                .await
                .unwrap()
                .unwrap();
        }
    }

    #[tokio::test]
    async fn connect_native_handoff_same_prefix_different_full_fingerprint_fails_without_saving() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "session-fingerprint-mismatch-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state_path = dir.join("pairing.json");

        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages.push(KeyUsagePurpose::DigitalSignature);
        params.key_usages.push(KeyUsagePurpose::KeyCertSign);
        let signing_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let signing_cert = params.self_signed(&signing_key).unwrap();

        let ca_der = signing_cert.der();
        let full_der_sha256 = spl_core::ca::sha256_hex(ca_der);

        let mut leaf_params = CertificateParams::new(vec!["127.0.0.1".to_string()]).unwrap();
        leaf_params.is_ca = IsCa::NoCa;
        leaf_params
            .extended_key_usages
            .push(ExtendedKeyUsagePurpose::ServerAuth);
        let leaf_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let leaf_cert = leaf_params
            .signed_by(&leaf_key, &signing_cert, &signing_key)
            .unwrap();

        let server_leaf_der = CertificateDer::from(leaf_cert.der().to_vec());
        let server_ca_der = CertificateDer::from(ca_der.to_vec());
        let server_key_der =
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der()));
        let mut link_pin = [0u8; 16];
        link_pin.copy_from_slice(&spl_core::ca::sha256(ca_der)[..16]);

        let server_cfg =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![server_leaf_der, server_ca_der], server_key_der)
                .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(server_cfg));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let _server = tokio::spawn(serve_one_pair_response(
            listener,
            acceptor,
            signing_cert,
            signing_key,
        ));

        let link = direct_loopback_link(port, &link_pin);

        // Same 16-byte (32 hex char) prefix, but different second half
        let mut mismatched_full_fp = full_der_sha256.clone();
        mismatched_full_fp.replace_range(32..64, "00000000000000000000000000000000");
        assert_eq!(&mismatched_full_fp[..32], &full_der_sha256[..32]);
        assert_ne!(mismatched_full_fp, full_der_sha256);

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let progress = crate::handoff::SetupHandoffProgress {
            expires_at_unix_ms: u64::MAX,
            schema: observer_contract::PROGRESS_SCHEMA.to_string(),
            handoff_id: "11111111-1111-4111-8111-111111111111".to_string(),
            pairing_link: link,
            expected_ca_fingerprint: mismatched_full_fp,
            expected_instance_id: "fixture-journal".to_string(),
            accepted_unix_ms: now_ms,
        };
        let progress_bytes = serde_json::to_vec_pretty(&progress).unwrap();
        std::fs::write(
            dir.join(observer_contract::PROGRESS_FILE_NAME),
            progress_bytes,
        )
        .unwrap();

        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
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
            tombstone: Arc::new(Mutex::new(None)),
            #[cfg(feature = "awaiting-hold")]
            awaiting_hold: None,
        };
        let access_mutex = tokio::sync::Mutex::new(None);
        let slot = tokio::sync::Mutex::new(UploaderSlot::new());

        let err = connect_native_handoff(
            &progress.handoff_id,
            progress.accepted_unix_ms,
            &cfg,
            sync.clone(),
            &slot,
            &access_mutex,
            &std::sync::atomic::AtomicBool::new(false),
            || async {},
            || async {},
        )
        .await
        .unwrap_err();
        assert!(matches!(err, TransportError::Pairing(_)));
        assert!(
            !state_path.exists(),
            "pairing file must not be created when full fingerprint mismatches"
        );

        let _ = std::fs::remove_dir_all(dir);
    }
}
