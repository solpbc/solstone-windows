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
use std::sync::{Arc, Mutex};

use observer_model::{HealthDump, PairingPhase, PairingState, SyncSnapshot};
use tokio::sync::oneshot;

use crate::client::ObserverClient;
use crate::coordinator::UploadCoordinator;
use crate::credential::PairedState;
use crate::heartbeat::run_heartbeat;
use crate::sealed::{LocalSealedStore, SealedStore};
use crate::{pairing, TransportError};

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
}

fn set_pairing(sync: &Arc<Mutex<SyncSnapshot>>, state: PairingState) {
    if let Ok(mut snapshot) = sync.lock() {
        snapshot.pairing = state;
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
            set_pairing(
                &sync,
                PairingState {
                    phase: PairingPhase::Failed,
                    detail: Some(e.to_string()),
                    ..Default::default()
                },
            );
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
    let mut client = ObserverClient::new(credential.clone())?;
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
    };
    paired.save(&cfg.state_path)?;
    Ok((paired, journal_label, registration.name))
}

/// Run the upload coordinator + heartbeat for an already-paired observer until
/// `shutdown` fires. Registers first if the stored state has no observer handle.
pub async fn run_uploader(
    paired: PairedState,
    cfg: SyncConfig,
    health: Arc<Mutex<HealthDump>>,
    sync: Arc<Mutex<SyncSnapshot>>,
    shutdown: oneshot::Receiver<()>,
) -> Result<(), TransportError> {
    let credential = paired.credential.clone().ok_or(TransportError::NotPaired)?;
    let journal_label = credential.home_label.clone();
    let mut client = ObserverClient::new(credential.clone())?;

    let observer_name = if let Some(key) = paired.observer_key.clone() {
        client = client.with_observer_key(Some(key));
        None
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
    );

    let (co_shutdown_tx, co_shutdown_rx) = oneshot::channel();
    let (hb_shutdown_tx, hb_shutdown_rx) = oneshot::channel();
    let coordinator_task = tokio::spawn(coordinator.run(co_shutdown_rx));
    let heartbeat_task = tokio::spawn(run_heartbeat(
        client.clone(),
        health,
        sync.clone(),
        hb_shutdown_rx,
    ));

    let _ = shutdown.await;
    let _ = co_shutdown_tx.send(());
    let _ = hb_shutdown_tx.send(());
    let _ = coordinator_task.await;
    let _ = heartbeat_task.await;
    Ok(())
}
