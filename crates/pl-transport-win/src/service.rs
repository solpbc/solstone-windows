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
}

pub fn publish_pairing(
    sync: &Arc<Mutex<SyncSnapshot>>,
    confirmation: &Arc<Mutex<String>>,
    write: PairingWrite,
) {
    if let Ok(mut snapshot) = sync.lock() {
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
                snapshot.pairing = PairingState {
                    phase: PairingPhase::NotPaired,
                    journal_label: None,
                    detail,
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
                let snapshot_binding = snapshot.pairing.binding.clone();
                let confirmed = confirmation.lock().unwrap();
                let matches_confirmed = !confirmed.is_empty() && *confirmed == binding;

                if !snapshot_binding.is_empty() {
                    if snapshot_binding != binding {
                        return;
                    }
                    if !matches_confirmed {
                        return;
                    }
                    match kind {
                        BoundKind::Paired => {
                            snapshot.pairing = PairingState {
                                phase: PairingPhase::Paired,
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
                } else {
                    match kind {
                        BoundKind::Paired => {
                            if matches_confirmed {
                                snapshot.pairing = PairingState {
                                    phase: PairingPhase::Paired,
                                    journal_label: Some(label),
                                    detail: None,
                                    mark,
                                    binding,
                                };
                            } else {
                                snapshot.pairing = PairingState {
                                    phase: PairingPhase::AwaitingConfirmation,
                                    journal_label: Some(label),
                                    detail: None,
                                    mark,
                                    binding,
                                };
                            }
                        }
                        BoundKind::Failed { detail } => {
                            snapshot.pairing = PairingState {
                                phase: PairingPhase::Failed,
                                journal_label: None,
                                detail,
                                mark: None,
                                binding: String::new(),
                            };
                        }
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
    publish_pairing(&sync, &cfg.confirmation, PairingWrite::BeginCeremony);

    match pair_inner(link, cfg).await {
        Ok((paired, journal_label, mark, binding)) => {
            cfg.journal_version.clear(&sync);
            publish_pairing(
                &sync,
                &cfg.confirmation,
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
                PairingWrite::NotPaired {
                    detail: Some(transport_error_code(&e)),
                },
            );
            Err(e)
        }
    }
}

async fn pair_inner(
    link: &str,
    cfg: &SyncConfig,
) -> Result<(PairedState, String, Option<MarkRenderSpec>, String), TransportError> {
    let credential = pairing::pair_from_link(link, &cfg.device_label).await?;
    let journal_label = credential.home_label.clone();
    let mark = journal_mark(&credential.instance_id);
    let binding = crate::ack::JournalIdentity::from_credential(&credential).client_cert_sha256;
    let paired = PairedState {
        credential: Some(credential),
        ..Default::default()
    };
    paired.save(&cfg.state_path)?;
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

    let coordinator_task = tokio::spawn(coordinator.run(cancel.clone(), wake));
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
    Ok(UploadCoordinator::new_with_slot(
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
    ))
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

        // 1. BeginCeremony
        publish_pairing(&sync, &confirmation, PairingWrite::BeginCeremony);
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

        // 5. NotPaired
        publish_pairing(
            &sync,
            &confirmation,
            PairingWrite::NotPaired {
                detail: Some("pair_link".to_string()),
            },
        );
        let s = sync.lock().unwrap().clone();
        assert_eq!(s.pairing.phase, PairingPhase::NotPaired);
        assert_eq!(s.pairing.binding, "");
        assert_eq!(s.pairing.detail.as_deref(), Some("pair_link"));
    }
}
