// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Sync orchestration: pair -> register -> upload + heartbeat.
//!
//! This is the thin composition the binary drives. `pair_and_register` runs the
//! one-shot handshake from a pasted link and persists the credential;
//! `run_uploader` spins the upload coordinator and heartbeat for an already
//! paired observer and runs until shutdown. Both publish honest pairing/upload
//! state into the shared [`SyncSnapshot`] so the health dump reflects reality.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use observer_model::{HealthDump, LocalOffset, PairingPhase, PairingState, SyncSnapshot};
use observer_retention::RetentionConfig;
use tokio::sync::watch;
use tokio::task::{JoinError, JoinHandle};

use crate::client::ObserverClient;
use crate::coordinator::UploadCoordinator;
use crate::credential::PairedState;
use crate::heartbeat::run_heartbeat;
use crate::sealed::{LocalSealedStore, SealedStore};
use crate::{cancelled, pairing, transport_error_code, TransportError};

/// Static identity + paths the sync layer needs.
#[derive(Debug, Clone)]
pub struct SyncConfig {
    /// Observer platform string sent on register/ingest (`"windows"`).
    pub platform: String,
    /// Device hostname registered with the journal.
    pub hostname: String,
    /// Observer build version.
    pub version: String,
    /// Observer stream type (`"desktop"`).
    pub stream_type: String,
    /// CN to put on the pairing CSR.
    pub device_label: String,
    /// Segment rotation period (must match the capture engine's).
    pub period_secs: u64,
    /// Where the paired credential + observer handle persist.
    pub state_path: PathBuf,
    /// The sealed-segments root the uploader drains.
    pub segments_root: PathBuf,
    /// Owner cache-retention policy (shared, edited over IPC) the upload
    /// coordinator honors when a segment's upload is confirmed.
    pub retention: Arc<RwLock<RetentionConfig>>,
    /// Device-local UTC-offset provider used to derive journal segment keys.
    pub local_offset: Arc<dyn LocalOffset>,
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

/// Pair from a pasted/scanned link, register the observer, persist, and update
/// the sync snapshot. Returns the persisted [`PairedState`].
pub async fn pair_and_register(
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

    match pair_and_register_inner(link, cfg).await {
        Ok((paired, journal_label, observer_name)) => {
            set_pairing(
                &sync,
                PairingState {
                    phase: PairingPhase::Paired,
                    journal_label: Some(journal_label),
                    observer_name: Some(observer_name),
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

async fn pair_and_register_inner(
    link: &str,
    cfg: &SyncConfig,
) -> Result<(PairedState, String, String), TransportError> {
    let credential = pairing::pair_from_link(link, &cfg.device_label).await?;
    let journal_label = credential.home_label.clone();
    let mut client =
        ObserverClient::new(credential.clone())?.with_state_path(cfg.state_path.clone());
    let registration = client
        .register(
            &cfg.platform,
            &cfg.hostname,
            &cfg.stream_type,
            &cfg.version,
            None,
        )
        .await?;
    let paired = PairedState {
        credential: Some(credential),
        observer_key: Some(registration.key.clone()),
        observer_name: Some(registration.name.clone()),
    };
    paired.save(&cfg.state_path)?;
    Ok((paired, journal_label, registration.name))
}

struct UploaderParts {
    client: Arc<ObserverClient>,
    coordinator: UploadCoordinator,
    stream_type: String,
    version: String,
}

/// Run the upload coordinator + heartbeat for an already-paired observer until
/// `cancel` fires. Registers first if the stored state has no observer handle.
pub async fn run_uploader(
    paired: PairedState,
    cfg: SyncConfig,
    health: Arc<Mutex<HealthDump>>,
    sync: Arc<Mutex<SyncSnapshot>>,
    cancel: watch::Receiver<bool>,
) {
    let parts = match setup_uploader(paired, cfg, sync.clone()).await {
        Ok(parts) => parts,
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

    let (inner_tx, inner_rx) = watch::channel(false);
    let coordinator_task = tokio::spawn(parts.coordinator.run(inner_rx.clone()));
    let heartbeat_task = tokio::spawn(run_heartbeat(
        parts.client,
        health,
        sync.clone(),
        parts.stream_type,
        parts.version,
        inner_rx.clone(),
    ));
    drop(inner_rx);

    supervise(&sync, cancel, inner_tx, coordinator_task, heartbeat_task).await;
}

async fn setup_uploader(
    paired: PairedState,
    cfg: SyncConfig,
    sync: Arc<Mutex<SyncSnapshot>>,
) -> Result<UploaderParts, TransportError> {
    let credential = paired.credential.clone().ok_or(TransportError::NotPaired)?;
    let journal_label = credential.home_label.clone();
    let mut client =
        ObserverClient::new(credential.clone())?.with_state_path(cfg.state_path.clone());

    let observer_name = if let Some(key) = paired.observer_key.clone() {
        client = client.with_observer_key(Some(key));
        paired.observer_name.clone()
    } else {
        let registration = client
            .register(
                &cfg.platform,
                &cfg.hostname,
                &cfg.stream_type,
                &cfg.version,
                None,
            )
            .await?;
        PairedState {
            credential: Some(credential),
            observer_key: Some(registration.key.clone()),
            observer_name: Some(registration.name.clone()),
        }
        .save(&cfg.state_path)?;
        Some(registration.name)
    };

    set_pairing(
        &sync,
        PairingState {
            phase: PairingPhase::Paired,
            journal_label: Some(journal_label),
            observer_name,
            detail: None,
        },
    );

    let client = Arc::new(client);
    let store: Box<dyn SealedStore> =
        Box::new(LocalSealedStore::new(&cfg.segments_root, cfg.period_secs));
    let coordinator = UploadCoordinator::new(
        client.clone(),
        store,
        sync.clone(),
        cfg.platform.clone(),
        cfg.period_secs,
        cfg.retention.clone(),
        cfg.local_offset.clone(),
    );

    Ok(UploaderParts {
        client,
        coordinator,
        stream_type: cfg.stream_type,
        version: cfg.version,
    })
}

enum Outcome {
    Cancelled,
    Coordinator(Result<(), JoinError>),
    Heartbeat(Result<(), JoinError>),
}

pub(crate) async fn supervise(
    sync: &Arc<Mutex<SyncSnapshot>>,
    mut external_cancel: watch::Receiver<bool>,
    inner_tx: watch::Sender<bool>,
    mut coordinator_task: JoinHandle<()>,
    mut heartbeat_task: JoinHandle<()>,
) {
    let outcome = tokio::select! {
        _ = cancelled(&mut external_cancel) => Outcome::Cancelled,
        res = &mut coordinator_task => Outcome::Coordinator(res),
        res = &mut heartbeat_task => Outcome::Heartbeat(res),
    };
    let _ = inner_tx.send(true);
    match outcome {
        Outcome::Cancelled => {
            let _ = coordinator_task.await;
            let _ = heartbeat_task.await;
        }
        Outcome::Coordinator(res) => {
            let code = dead_code(&res);
            mark_uploader_dead(sync, code);
            warn_dead("coordinator", code);
            let _ = heartbeat_task.await;
        }
        Outcome::Heartbeat(res) => {
            let code = dead_code(&res);
            mark_uploader_dead(sync, code);
            warn_dead("heartbeat", code);
            let _ = coordinator_task.await;
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
        snap.upload.heartbeat_ok = false;
        snap.upload.record_failure(code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

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
    async fn supervise_marks_panicked_coordinator_dead_and_cancels_heartbeat() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let (_external_tx, external_rx) = watch::channel(false);
        let (inner_tx, mut inner_rx) = watch::channel(false);
        let heartbeat_cancelled = Arc::new(AtomicBool::new(false));
        let heartbeat_cancelled_for_task = heartbeat_cancelled.clone();

        let coordinator_task = tokio::spawn(async {
            panic!("boom");
        });
        let heartbeat_task = tokio::spawn(async move {
            crate::cancelled(&mut inner_rx).await;
            heartbeat_cancelled_for_task.store(true, Ordering::SeqCst);
        });

        supervise(
            &sync,
            external_rx,
            inner_tx,
            coordinator_task,
            heartbeat_task,
        )
        .await;

        let snapshot = sync.lock().unwrap().clone();
        assert_eq!(
            snapshot.upload.last_error.as_deref(),
            Some("uploader_panicked")
        );
        assert!(!snapshot.upload.heartbeat_ok);
        assert_eq!(snapshot.upload.recent_error_count, 1);
        let last_error = snapshot.upload.last_error.unwrap();
        assert_eq!(last_error, "uploader_panicked");
        assert!(!last_error.contains("SECRET"));
        assert!(!last_error.contains("token"));
        assert!(!last_error.contains("Users"));
        assert!(heartbeat_cancelled.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn supervise_marks_clean_early_coordinator_exit_as_stopped() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let (_external_tx, external_rx) = watch::channel(false);
        let (inner_tx, mut inner_rx) = watch::channel(false);

        let coordinator_task = tokio::spawn(async {});
        let heartbeat_task = tokio::spawn(async move {
            crate::cancelled(&mut inner_rx).await;
        });

        supervise(
            &sync,
            external_rx,
            inner_tx,
            coordinator_task,
            heartbeat_task,
        )
        .await;

        let snapshot = sync.lock().unwrap().clone();
        assert_eq!(
            snapshot.upload.last_error.as_deref(),
            Some("uploader_stopped")
        );
        assert!(!snapshot.upload.heartbeat_ok);
        assert_eq!(snapshot.upload.recent_error_count, 1);
    }
}
