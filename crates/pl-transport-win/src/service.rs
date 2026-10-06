// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Sync orchestration: pair -> upload.
//!
//! This is the thin composition the binary drives. `pair` runs the one-shot
//! handshake from a pasted link and persists the credential; `run_uploader` spins
//! the upload coordinator for an already paired observer and runs until shutdown.
//! Both publish honest pairing/upload state into the shared [`SyncSnapshot`] so
//! the health dump reflects reality.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use observer_model::{LocalOffset, MarkRenderSpec, PairingPhase, PairingState, SyncSnapshot};
use tokio::sync::watch;
use tokio::task::{JoinError, JoinHandle};

use crate::access::CredentialAccess;
use crate::coordinator::UploadCoordinator;
use crate::credential::PairedState;
use crate::device_metadata::RawDeviceFacts;
use crate::journal_version::JournalVersionController;
use crate::sealed::{LocalSealedStore, SealedStore};
use crate::unknown_journals::mark_spec_for_jid;
use crate::{cancelled, pairing, transport_error_code, TransportError};

/// The paired journal's own mark, for the given instance id. Reuses the same
/// primitive the unknown-journal comparison uses for "your journal" — `None`
/// only for a placeholder/test instance id that isn't a real journal id.
pub(crate) fn journal_mark(instance_id: &str) -> Option<MarkRenderSpec> {
    mark_spec_for_jid(instance_id)
}

/// Static identity + paths the sync layer needs.
#[derive(Clone)]
pub struct SyncConfig {
    /// CN to put on the pairing CSR.
    pub device_label: String,
    /// Segment rotation period (must match the capture engine's).
    pub period_secs: u64,
    /// Where the paired credential persists.
    pub state_path: PathBuf,
    /// The sealed-segments root the uploader drains.
    pub segments_root: PathBuf,
    /// Device-local UTC-offset provider used to derive journal segment keys.
    pub local_offset: Arc<dyn LocalOffset>,
    /// Journal version state owner.
    pub journal_version: Arc<JournalVersionController>,
    /// Device facts resampler for post-connect publication.
    pub facts_fn: Arc<dyn Fn() -> RawDeviceFacts + Send + Sync>,
    /// Process-local confirmation digest cache.
    pub confirmation: Arc<Mutex<String>>,
    /// Tombstone digest of the most recently rejected/cancelled pairing.
    pub tombstone: Arc<Mutex<Option<String>>>,
    #[cfg(feature = "awaiting-hold")]
    pub awaiting_hold: Option<crate::coordinator::AwaitingHold>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundKind {
    Paired,
    Failed { detail: Option<String> },
}

// The mark spec is large. This value is moved once into publish_pairing and is not stored.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum PairingWrite {
    BeginCeremony,
    Bound {
        binding: String,
        label: String,
        mark: Option<MarkRenderSpec>,
        kind: BoundKind,
    },
    NotPaired {
        detail: Option<String>,
    },
    Failed {
        detail: String,
    },
}

pub fn publish_pairing(
    sync: &Arc<Mutex<SyncSnapshot>>,
    confirmation: &Arc<Mutex<String>>,
    tombstone: &Arc<Mutex<Option<String>>>,
    write: PairingWrite,
) {
    if let Ok(mut snapshot) = sync.lock() {
        let mut tombstone_guard = tombstone.lock().unwrap();
        match write {
            PairingWrite::BeginCeremony => {
                snapshot.pairing = PairingState {
                    phase: PairingPhase::Pairing,
                    journal_label: None,
                    detail: None,
                    mark: None,
                    binding: String::new(),
                };
            }
            PairingWrite::NotPaired { detail } => {
                let prev_binding = std::mem::take(&mut snapshot.pairing.binding);
                if let Some(ref d) = detail {
                    if (d == observer_model::MARK_REJECTED_DETAIL
                        || d == observer_model::PAIRING_CANCELLED_DETAIL)
                        && !prev_binding.is_empty()
                    {
                        *tombstone_guard = Some(prev_binding);
                    }
                }
                snapshot.pairing = PairingState {
                    phase: PairingPhase::NotPaired,
                    journal_label: None,
                    detail,
                    mark: None,
                    binding: String::new(),
                };
            }
            PairingWrite::Failed { detail } => {
                snapshot.pairing = PairingState {
                    phase: PairingPhase::Failed,
                    journal_label: None,
                    detail: Some(detail),
                    mark: None,
                    binding: String::new(),
                };
            }
            PairingWrite::Bound {
                binding,
                label,
                mark,
                kind,
            } => {
                if tombstone_guard.as_deref() == Some(&binding) {
                    return;
                }

                let snapshot_binding = snapshot.pairing.binding.clone();
                if !snapshot_binding.is_empty() && snapshot_binding != binding {
                    return;
                }

                // An empty snapshot binding still publishes a bound failure for
                // the current pairing: paired, awaiting confirmation, or
                // unpaired while confirmation names this binding. A ceremony,
                // an already-failed attempt, and an unpaired snapshot that
                // does not name this binding are not that pairing.
                if matches!(kind, BoundKind::Failed { .. }) && snapshot_binding.is_empty() {
                    let confirmed_this = {
                        let confirmed = confirmation.lock().unwrap();
                        !confirmed.is_empty() && *confirmed == binding
                    };
                    let current = matches!(
                        snapshot.pairing.phase,
                        PairingPhase::Paired | PairingPhase::AwaitingConfirmation
                    ) || (snapshot.pairing.phase == PairingPhase::NotPaired
                        && confirmed_this);
                    if !current {
                        return;
                    }
                }

                let confirmed = confirmation.lock().unwrap();
                let matches_confirmed = !confirmed.is_empty() && *confirmed == binding;

                match kind {
                    BoundKind::Paired => {
                        let phase = if matches_confirmed {
                            PairingPhase::Paired
                        } else {
                            PairingPhase::AwaitingConfirmation
                        };
                        snapshot.pairing = PairingState {
                            phase,
                            journal_label: Some(label),
                            detail: None,
                            mark,
                            binding,
                        };
                    }
                    BoundKind::Failed { detail } => {
                        snapshot.pairing = PairingState {
                            phase: PairingPhase::Failed,
                            journal_label: Some(label),
                            detail,
                            mark,
                            binding,
                        };
                    }
                }
            }
        }
    }
}

#[cfg(test)]
fn failed_pairing_state(error: &TransportError) -> PairingState {
    PairingState {
        phase: PairingPhase::Failed,
        detail: Some(transport_error_code(error)),
        ..Default::default()
    }
}

/// Pair from a pasted/scanned link, persist the credential, and update the sync
/// snapshot. Returns the persisted [`PairedState`].
pub async fn pair(
    link: &str,
    cfg: &SyncConfig,
    sync: Arc<Mutex<SyncSnapshot>>,
) -> Result<PairedState, TransportError> {
    ensure_pairable(&cfg.state_path)?;
    let fresh_pair = !PairedState::load(&cfg.state_path)?.is_paired();
    publish_pairing(
        &sync,
        &cfg.confirmation,
        &cfg.tombstone,
        PairingWrite::BeginCeremony,
    );

    match pair_inner(link, cfg, fresh_pair).await {
        Ok((paired, journal_label, mark, binding)) => {
            cfg.journal_version.clear(&sync);
            publish_pairing(
                &sync,
                &cfg.confirmation,
                &cfg.tombstone,
                PairingWrite::Bound {
                    binding,
                    label: journal_label,
                    mark,
                    kind: BoundKind::Paired,
                },
            );
            Ok(paired)
        }
        Err(e) => {
            publish_pairing(
                &sync,
                &cfg.confirmation,
                &cfg.tombstone,
                PairingWrite::Failed {
                    detail: transport_error_code(&e),
                },
            );
            Err(e)
        }
    }
}

/// This service path only installs a credential into an empty slot. The GUI
/// session routes explicit pairing against an incumbent through its durable
/// replacement transaction before reaching this path.
pub(crate) fn ensure_pairable(state_path: &std::path::Path) -> Result<(), TransportError> {
    let paired = PairedState::load(state_path)?;
    if paired.retirement_intent.is_some() {
        return Err(TransportError::Pairing(
            "a client retirement is still pending".to_owned(),
        ));
    }
    if paired.is_paired() {
        return Err(TransportError::Pairing(
            "an existing pairing must be retired before pairing again".to_owned(),
        ));
    }
    Ok(())
}

async fn pair_inner(
    link: &str,
    cfg: &SyncConfig,
    fresh_pair: bool,
) -> Result<(PairedState, String, Option<MarkRenderSpec>, String), TransportError> {
    let credential = pairing::pair_from_link(link, &cfg.device_label).await?;
    let journal_label = credential.home_label.clone();
    let mark = journal_mark(&credential.instance_id);
    let binding = crate::ack::JournalIdentity::from_credential(&credential).client_cert_sha256;
    if fresh_pair {
        crate::migration::record_fresh_pair_offer(&cfg.state_path, &credential)?;
    }
    let paired = PairedState {
        credential: Some(credential),
        ..Default::default()
    };
    paired.save_if_unpaired(&cfg.state_path)?;
    Ok((paired, journal_label, mark, binding))
}

/// Run the upload coordinator for an already-paired observer until `cancel`
/// fires.
pub async fn run_uploader(
    access: CredentialAccess,
    cfg: SyncConfig,
    sync: Arc<Mutex<SyncSnapshot>>,
    cancel: watch::Receiver<crate::slot::SlotExit>,
    wake: Arc<tokio::sync::Notify>,
) {
    let coordinator = match setup_uploader(access, cfg, sync.clone()).await {
        Ok(coordinator) => coordinator,
        Err(error) => {
            let code = transport_error_code(&error);
            mark_uploader_dead(&sync, "uploader_setup_failed");
            tracing::warn!(
                target: "sync",
                reason = code.as_str(),
                "uploader setup failed"
            );
            return;
        }
    };

    let coord_cancel = cancel.clone();
    let coordinator_task = tokio::spawn(async move { coordinator.run(coord_cancel, wake).await });
    await_coordinator(&sync, cancel, coordinator_task).await;
}

async fn setup_uploader(
    access: CredentialAccess,
    cfg: SyncConfig,
    sync: Arc<Mutex<SyncSnapshot>>,
) -> Result<UploadCoordinator, TransportError> {
    let client_slot = access.client_slot();
    let post_connect = access.post_connect();
    let client = client_slot.load();
    let journal_label = client.home_label().to_string();
    let mark = journal_mark(&client.credential().instance_id);
    let binding = client.journal_identity().client_cert_sha256;

    publish_pairing(
        &sync,
        &cfg.confirmation,
        &cfg.tombstone,
        PairingWrite::Bound {
            binding,
            label: journal_label,
            mark,
            kind: BoundKind::Paired,
        },
    );

    post_connect.trigger();

    let store: Box<dyn SealedStore> =
        Box::new(LocalSealedStore::new(&cfg.segments_root, cfg.period_secs));
    let coordinator = UploadCoordinator::new_with_slot(
        client_slot,
        store,
        sync,
        cfg.period_secs,
        cfg.local_offset,
        cfg.journal_version,
        Some(post_connect),
        access.journal_version_token(),
        Some(access.post_connect_token()),
        cfg.confirmation.clone(),
        cfg.tombstone.clone(),
    );
    #[cfg(feature = "awaiting-hold")]
    let coordinator = coordinator.with_awaiting_hold(cfg.awaiting_hold);
    Ok(coordinator)
}

/// Settle launch state, resume an existing pairing if present, and start the uploader.
pub async fn launch_resume(
    cfg: &SyncConfig,
    sync: &Arc<Mutex<SyncSnapshot>>,
    slot: &mut crate::slot::UploaderSlot,
) -> Option<CredentialAccess> {
    if let Err(error) = crate::retirement::reconcile_on_launch(&cfg.state_path).await {
        publish_pairing(
            sync,
            &cfg.confirmation,
            &cfg.tombstone,
            PairingWrite::Failed {
                detail: transport_error_code(&error),
            },
        );
        return None;
    }
    match PairedState::load_detailed(&cfg.state_path) {
        Err(error @ TransportError::ClientKeyProtectionRefused) => {
            publish_pairing(
                sync,
                &cfg.confirmation,
                &cfg.tombstone,
                PairingWrite::NotPaired {
                    detail: Some(transport_error_code(&error)),
                },
            );
            return None;
        }
        Err(error @ TransportError::CredentialRecoveryRequired)
        | Err(error @ TransportError::CredentialMalformed) => {
            publish_pairing(
                sync,
                &cfg.confirmation,
                &cfg.tombstone,
                PairingWrite::Failed {
                    detail: transport_error_code(&error),
                },
            );
            return None;
        }
        Err(_) | Ok(_) => {}
    }

    #[cfg(windows)]
    let _marker_result = Some(crate::device_marker::probe_platform());
    #[cfg(not(windows))]
    let _marker_result: Option<crate::device_marker::MarkerResult> = None;

    #[cfg(windows)]
    if let Some(marker) = _marker_result.clone() {
        match crate::migration::resume_on_launch(
            &cfg.state_path,
            marker,
            &cfg.device_label,
            "solpbc/solstone-windows",
        )
        .await
        {
            Ok(true) => {}
            Ok(false) => return None,
            Err(error) => {
                publish_pairing(
                    sync,
                    &cfg.confirmation,
                    &cfg.tombstone,
                    PairingWrite::Failed {
                        detail: transport_error_code(&error),
                    },
                );
                return None;
            }
        }
    }

    let _ = crate::answer::settle_grandfather(&cfg.state_path, &cfg.confirmation);
    #[cfg(windows)]
    if let (Some(marker), Ok(paired)) =
        (_marker_result.as_ref(), PairedState::load(&cfg.state_path))
    {
        if let Err(error) =
            crate::migration::commit_baseline_after_answer(&cfg.state_path, marker, &paired)
        {
            tracing::warn!(target: "sync", error = %error, "device migration baseline remains pending");
            publish_pairing(
                sync,
                &cfg.confirmation,
                &cfg.tombstone,
                PairingWrite::Failed {
                    detail: transport_error_code(&error),
                },
            );
            return None;
        }
    }
    {
        match PairedState::load(&cfg.state_path) {
            Ok(paired) if paired.is_paired() => {
                let cfg_clone = cfg.clone();
                let sync_for_sync = sync.clone();
                match CredentialAccess::bind(&paired, cfg, sync.clone(), None) {
                    Ok(access) => {
                        tracing::info!(
                            target: "sync",
                            source = "resume",
                            "uploader started"
                        );
                        let wake = slot.wake();
                        let access_clone = access.clone();
                        slot.replace(move |rx| async move {
                            run_uploader(access_clone, cfg_clone, sync_for_sync, rx, wake).await;
                        })
                        .await;
                        return Some(access);
                    }
                    Err(error) => {
                        let error = error.to_string();
                        tracing::warn!(
                            target: "sync",
                            error = %observer_log::redact_secret("pairing-load-error", &error),
                            "pairing state load failed"
                        );
                    }
                }
            }
            Ok(_) => {}
            Err(error) => {
                let error = error.to_string();
                tracing::warn!(
                    target: "sync",
                    error = %observer_log::redact_secret("pairing-load-error", &error),
                    "pairing state load failed"
                );
            }
        }
    }
    None
}

async fn await_coordinator(
    sync: &Arc<Mutex<SyncSnapshot>>,
    mut external_cancel: watch::Receiver<crate::slot::SlotExit>,
    mut coordinator_task: JoinHandle<()>,
) {
    tokio::select! {
        biased;
        _ = cancelled(&mut external_cancel) => {
            let _ = coordinator_task.await;
        }
        result = &mut coordinator_task => {
            let code = dead_code(&result);
            mark_uploader_dead(sync, code);
            warn_dead("coordinator", code);
        }
    }
}

fn dead_code(res: &Result<(), JoinError>) -> &'static str {
    match res {
        Err(error) if error.is_panic() => "uploader_panicked",
        _ => "uploader_stopped",
    }
}

fn warn_dead(which: &'static str, code: &'static str) {
    tracing::warn!(
        target: "sync",
        task = which,
        reason = code,
        "uploader task exited"
    );
}

fn mark_uploader_dead(sync: &Arc<Mutex<SyncSnapshot>>, code: &'static str) {
    if let Ok(mut snap) = sync.lock() {
        snap.upload.last_error = Some(code.to_string());
        snap.upload.record_failure(code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_pairing_state_redacts_detail() {
        let error = TransportError::Rejected {
            status: 403,
            body: "SECRET https://10.0.0.5/y?token=abc C:\\Users\\me\\seg.mp4 sha256:abc".into(),
        };

        let state = failed_pairing_state(&error);

        assert_eq!(state.phase, PairingPhase::Failed);
        assert_eq!(state.detail.as_deref(), Some("http_403"));
        let detail = state.detail.unwrap();
        assert!(!detail.contains("SECRET"));
        assert!(!detail.contains("token"));
        assert!(!detail.contains("Users"));
        assert!(!detail.contains("https://"));
        assert!(!detail.contains("sha256"));
        assert!(!detail.contains("10.0.0.5"));
    }

    #[test]
    fn journal_mark_is_some_for_a_real_instance_id_and_none_for_a_placeholder() {
        let real_jid = "f30ed159-ef46-8e9c-913f-e49f0fe7d201";
        assert!(journal_mark(real_jid).is_some());
        assert!(journal_mark("test").is_none());
    }

    #[tokio::test]
    async fn await_coordinator_marks_panicked_task_dead() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let (_external_tx, external_rx) = watch::channel(crate::slot::SlotExit::Run);

        let coordinator_task = tokio::spawn(async {
            panic!("boom");
        });
        await_coordinator(&sync, external_rx, coordinator_task).await;

        let snapshot = sync.lock().unwrap().clone();
        assert_eq!(
            snapshot.upload.last_error.as_deref(),
            Some("uploader_panicked")
        );
        assert_eq!(snapshot.upload.recent_error_count, 1);
        let last_error = snapshot.upload.last_error.unwrap();
        assert_eq!(last_error, "uploader_panicked");
        assert!(!last_error.contains("SECRET"));
        assert!(!last_error.contains("token"));
        assert!(!last_error.contains("Users"));
    }

    #[tokio::test]
    async fn await_coordinator_marks_clean_early_exit_as_stopped() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let (_external_tx, external_rx) = watch::channel(crate::slot::SlotExit::Run);

        let coordinator_task = tokio::spawn(async {});
        await_coordinator(&sync, external_rx, coordinator_task).await;

        let snapshot = sync.lock().unwrap().clone();
        assert_eq!(
            snapshot.upload.last_error.as_deref(),
            Some("uploader_stopped")
        );
        assert_eq!(snapshot.upload.recent_error_count, 1);
    }

    #[tokio::test]
    async fn await_coordinator_does_not_mark_external_cancellation_dead() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let (external_tx, external_rx) = watch::channel(crate::slot::SlotExit::Run);
        let mut task_cancel = external_rx.clone();
        let coordinator_task = tokio::spawn(async move {
            crate::cancelled(&mut task_cancel).await;
        });

        external_tx.send(crate::slot::SlotExit::Shutdown).unwrap();
        await_coordinator(&sync, external_rx, coordinator_task).await;

        let snapshot = sync.lock().unwrap().clone();
        assert_eq!(snapshot.upload.last_error, None);
        assert_eq!(snapshot.upload.recent_error_count, 0);
    }

    #[test]
    fn mark_confirmation_publish_pairing_transitions() {
        use observer_model::PairingPhase;

        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let confirmation = Arc::new(Mutex::new(String::new()));
        let tombstone = Arc::new(Mutex::new(None));

        // 1. BeginCeremony
        publish_pairing(
            &sync,
            &confirmation,
            &tombstone,
            PairingWrite::BeginCeremony,
        );
        let s = sync.lock().unwrap().clone();
        assert_eq!(s.pairing.phase, PairingPhase::Pairing);
        assert_eq!(s.pairing.binding, "");
        assert_eq!(s.pairing.journal_label, None);
        assert_eq!(s.pairing.mark, None);

        // 2. Bound when unconfirmed -> AwaitingConfirmation
        let binding =
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string();
        publish_pairing(
            &sync,
            &confirmation,
            &tombstone,
            PairingWrite::Bound {
                binding: binding.clone(),
                label: "journal-1".to_string(),
                mark: None,
                kind: BoundKind::Paired,
            },
        );
        let s = sync.lock().unwrap().clone();
        assert_eq!(s.pairing.phase, PairingPhase::AwaitingConfirmation);
        assert_eq!(s.pairing.binding, binding);
        assert_eq!(s.pairing.journal_label.as_deref(), Some("journal-1"));

        // 3. Bound when confirmed -> Paired
        *confirmation.lock().unwrap() = binding.clone();
        publish_pairing(
            &sync,
            &confirmation,
            &tombstone,
            PairingWrite::Bound {
                binding: binding.clone(),
                label: "journal-1".to_string(),
                mark: None,
                kind: BoundKind::Paired,
            },
        );
        let s = sync.lock().unwrap().clone();
        assert_eq!(s.pairing.phase, PairingPhase::Paired);
        assert_eq!(s.pairing.binding, binding);

        // 4. Mismatched binding write dropped
        publish_pairing(
            &sync,
            &confirmation,
            &tombstone,
            PairingWrite::Bound {
                binding: "other_binding".to_string(),
                label: "other-journal".to_string(),
                mark: None,
                kind: BoundKind::Paired,
            },
        );
        let s = sync.lock().unwrap().clone();
        assert_eq!(s.pairing.phase, PairingPhase::Paired);
        assert_eq!(s.pairing.binding, binding);
        assert_eq!(s.pairing.journal_label.as_deref(), Some("journal-1"));

        // 5. NotPaired with mark_rejected sets tombstone
        publish_pairing(
            &sync,
            &confirmation,
            &tombstone,
            PairingWrite::NotPaired {
                detail: Some(observer_model::MARK_REJECTED_DETAIL.to_string()),
            },
        );
        let s = sync.lock().unwrap().clone();
        assert_eq!(s.pairing.phase, PairingPhase::NotPaired);
        assert_eq!(s.pairing.binding, "");
        assert_eq!(
            s.pairing.detail.as_deref(),
            Some(observer_model::MARK_REJECTED_DETAIL)
        );
        assert_eq!(tombstone.lock().unwrap().as_deref(), Some(binding.as_str()));

        // 6. Bound matching tombstone is dropped
        publish_pairing(
            &sync,
            &confirmation,
            &tombstone,
            PairingWrite::Bound {
                binding: binding.clone(),
                label: "journal-1".to_string(),
                mark: None,
                kind: BoundKind::Paired,
            },
        );
        let s = sync.lock().unwrap().clone();
        assert_eq!(s.pairing.phase, PairingPhase::NotPaired);
        assert_eq!(s.pairing.binding, "");

        // 7. Ceremony failure sets Failed phase
        publish_pairing(
            &sync,
            &confirmation,
            &tombstone,
            PairingWrite::Failed {
                detail: "pair_link".to_string(),
            },
        );
        let s = sync.lock().unwrap().clone();
        assert_eq!(s.pairing.phase, PairingPhase::Failed);
        assert_eq!(s.pairing.detail.as_deref(), Some("pair_link"));
        assert_eq!(s.pairing.binding, "");
    }

    #[derive(Debug)]
    struct FixedOffset(i64);

    impl observer_model::LocalOffset for FixedOffset {
        fn local_zone(
            &self,
            _epoch_secs: u64,
        ) -> Result<observer_model::LocalZone, observer_model::LocalOffsetError> {
            Ok(observer_model::LocalZone {
                tz: Some("UTC".to_string()),
                utc_offset_seconds: self.0,
            })
        }
    }

    #[tokio::test]
    async fn service_pair_bad_link_publishes_failed_phase_with_pair_link_detail() {
        let dir = std::env::temp_dir().join(format!("test-pair-bad-link-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let cfg = SyncConfig {
            device_label: "test".to_string(),
            period_secs: 300,
            state_path: dir.join("pairing.json"),
            segments_root: dir.join("segments"),
            local_offset: Arc::new(FixedOffset(0)),
            journal_version: Arc::new(JournalVersionController::new(dir.join("jv.json"))),
            facts_fn: Arc::new(|| RawDeviceFacts {
                name: Some("test".into()),
                platform: Some("windows".into()),
                device_type: None,
                app_id: Some("test".into()),
                app_version: Some("0.1.0".into()),
            }),
            confirmation: Arc::new(Mutex::new(String::new())),
            tombstone: Arc::new(Mutex::new(None)),
            #[cfg(feature = "awaiting-hold")]
            awaiting_hold: None,
        };

        let result = pair("invalid-link", &cfg, sync.clone()).await;
        assert!(result.is_err());
        let s = sync.lock().unwrap().clone();
        assert_eq!(s.pairing.phase, PairingPhase::Failed);
        assert_eq!(s.pairing.detail.as_deref(), Some("pair_link"));
    }

    #[cfg(feature = "transport-tests")]
    #[tokio::test]
    async fn service_pair_unreachable_journal_publishes_failed_phase_with_error_code() {
        let dir =
            std::env::temp_dir().join(format!("test-pair-unreachable-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let cfg = SyncConfig {
            device_label: "test".to_string(),
            period_secs: 300,
            state_path: dir.join("pairing.json"),
            segments_root: dir.join("segments"),
            local_offset: Arc::new(FixedOffset(0)),
            journal_version: Arc::new(JournalVersionController::new(dir.join("jv.json"))),
            facts_fn: Arc::new(|| RawDeviceFacts {
                name: Some("test".into()),
                platform: Some("windows".into()),
                device_type: None,
                app_id: Some("test".into()),
                app_version: Some("0.1.0".into()),
            }),
            confirmation: Arc::new(Mutex::new(String::new())),
            tombstone: Arc::new(Mutex::new(None)),
            #[cfg(feature = "awaiting-hold")]
            awaiting_hold: None,
        };

        let unreachable_link = "solstone:pair?v=1&relay=127.0.0.1:9&cert=0000000000000000000000000000000000000000000000000000000000000000";
        let result = pair(unreachable_link, &cfg, sync.clone()).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        let expected_code = transport_error_code(&err);
        let s = sync.lock().unwrap().clone();
        assert_eq!(s.pairing.phase, PairingPhase::Failed);
        assert_eq!(s.pairing.detail.as_deref(), Some(expected_code.as_str()));
    }
}
