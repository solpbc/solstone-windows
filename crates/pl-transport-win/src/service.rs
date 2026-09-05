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
use std::sync::{Arc, Mutex, RwLock};

use observer_model::{LocalOffset, PairingPhase, PairingState, SyncSnapshot};
use observer_retention::RetentionConfig;
use tokio::sync::watch;
use tokio::task::{JoinError, JoinHandle};

use crate::client::ObserverClient;
use crate::coordinator::UploadCoordinator;
use crate::credential::PairedState;
use crate::journal_version::JournalVersionController;
use crate::sealed::{LocalSealedStore, SealedStore};
use crate::{cancelled, pairing, transport_error_code, TransportError};

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
    /// Owner cache-retention policy (shared, edited over IPC) the upload
    /// coordinator honors when a segment's upload is confirmed.
    pub retention: Arc<RwLock<RetentionConfig>>,
    /// Device-local UTC-offset provider used to derive journal segment keys.
    pub local_offset: Arc<dyn LocalOffset>,
    /// Journal version state owner.
    pub journal_version: Arc<JournalVersionController>,
}

fn set_pairing(sync: &Arc<Mutex<SyncSnapshot>>, state: PairingState) {
    if let Ok(mut snapshot) = sync.lock() {
        snapshot.pairing = state;
    }
}

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
    set_pairing(
        &sync,
        PairingState {
            phase: PairingPhase::Pairing,
            ..Default::default()
        },
    );

    match pair_inner(link, cfg).await {
        Ok((paired, journal_label)) => {
            cfg.journal_version.clear(&sync);
            set_pairing(
                &sync,
                PairingState {
                    phase: PairingPhase::Paired,
                    journal_label: Some(journal_label),
                    detail: None,
                },
            );
            Ok(paired)
        }
        Err(e) => {
            set_pairing(&sync, failed_pairing_state(&e));
            Err(e)
        }
    }
}

async fn pair_inner(link: &str, cfg: &SyncConfig) -> Result<(PairedState, String), TransportError> {
    let credential = pairing::pair_from_link(link, &cfg.device_label).await?;
    let journal_label = credential.home_label.clone();
    let paired = PairedState {
        credential: Some(credential),
    };
    paired.save(&cfg.state_path)?;
    Ok((paired, journal_label))
}

/// Run the upload coordinator for an already-paired observer until `cancel`
/// fires.
pub async fn run_uploader(
    paired: PairedState,
    cfg: SyncConfig,
    sync: Arc<Mutex<SyncSnapshot>>,
    cancel: watch::Receiver<bool>,
) {
    let coordinator = match setup_uploader(paired, cfg, sync.clone()).await {
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

    let coordinator_task = tokio::spawn(coordinator.run(cancel.clone()));
    await_coordinator(&sync, cancel, coordinator_task).await;
}

async fn setup_uploader(
    paired: PairedState,
    cfg: SyncConfig,
    sync: Arc<Mutex<SyncSnapshot>>,
) -> Result<UploadCoordinator, TransportError> {
    let credential = paired.credential.ok_or(TransportError::NotPaired)?;
    let journal_label = credential.home_label.clone();
    let version_generation = cfg.journal_version.begin_session(&credential, &sync);
    let client = ObserverClient::new(credential)?.with_state_path(cfg.state_path.clone());

    set_pairing(
        &sync,
        PairingState {
            phase: PairingPhase::Paired,
            journal_label: Some(journal_label),
            detail: None,
        },
    );

    let client = Arc::new(client);
    cfg.journal_version
        .trigger_refresh(client.clone(), sync.clone(), version_generation);
    let store: Box<dyn SealedStore> =
        Box::new(LocalSealedStore::new(&cfg.segments_root, cfg.period_secs));
    Ok(UploadCoordinator::new(
        client,
        store,
        sync,
        cfg.period_secs,
        cfg.retention,
        cfg.local_offset,
        cfg.journal_version,
    ))
}

async fn await_coordinator(
    sync: &Arc<Mutex<SyncSnapshot>>,
    mut external_cancel: watch::Receiver<bool>,
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

    #[tokio::test]
    async fn await_coordinator_marks_panicked_task_dead() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let (_external_tx, external_rx) = watch::channel(false);

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
        let (_external_tx, external_rx) = watch::channel(false);

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
        let (external_tx, external_rx) = watch::channel(false);
        let mut task_cancel = external_rx.clone();
        let coordinator_task = tokio::spawn(async move {
            crate::cancelled(&mut task_cancel).await;
        });

        external_tx.send(true).unwrap();
        await_coordinator(&sync, external_rx, coordinator_task).await;

        let snapshot = sync.lock().unwrap().clone();
        assert_eq!(snapshot.upload.last_error, None);
        assert_eq!(snapshot.upload.recent_error_count, 0);
    }
}
