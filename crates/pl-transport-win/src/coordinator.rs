// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The upload/sync coordinator — the macOS `UploadCoordinator`/`SyncService`
//! analog.
//!
//! On each tick it scans sealed segments, ships each to `/app/observer/ingest`,
//! then **reconciles**: it lists the journal's segments for the day and confirms
//! the journal recorded the same sha256 for every uploaded file before deleting
//! the local copy. A segment counts as `uploaded` only after that confirmation —
//! honest state, earned not asserted. Failures leave the segment on disk and
//! grow an exponential backoff (5s → 5m), so a transient journal outage retries
//! without losing data. Pairing/upload counts are published into the shared
//! [`SyncSnapshot`] the engine folds into the health dump.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use observer_model::SyncSnapshot;
use observer_pl::ca;
use observer_pl::civil;
use observer_pl::multipart::FilePart;

use crate::client::ObserverClient;
use crate::sealed::{content_type_for, SealedStore};
use crate::{TransportError, DEFAULT_UPLOAD_INTERVAL_SECS};

const MAX_BACKOFF_SECS: u64 = 300;

/// Drives sealed segments to the journal and reconciles them.
pub struct UploadCoordinator {
    client: Arc<ObserverClient>,
    store: Box<dyn SealedStore>,
    sync: Arc<Mutex<SyncSnapshot>>,
    platform: String,
    period_secs: u64,
}

impl UploadCoordinator {
    pub fn new(
        client: Arc<ObserverClient>,
        store: Box<dyn SealedStore>,
        sync: Arc<Mutex<SyncSnapshot>>,
        platform: impl Into<String>,
        period_secs: u64,
    ) -> Self {
        Self {
            client,
            store,
            sync,
            platform: platform.into(),
            period_secs: period_secs.max(1),
        }
    }

    /// One pass: upload + reconcile every sealed segment currently on disk.
    /// Returns the number of segments confirmed landed this pass.
    pub async fn tick(&self) -> Result<usize, TransportError> {
        let segments = self.store.scan()?;
        self.set_pending(segments.len() as u64);
        let mut confirmed_now = 0usize;

        for segment in segments {
            let day = civil::day_string(segment.boundary_epoch_secs);
            let segment_key =
                civil::segment_key_string(segment.boundary_epoch_secs, self.period_secs);

            // Read the per-source files + compute their sha256 for reconcile.
            let mut parts = Vec::with_capacity(segment.files.len());
            let mut shas = Vec::with_capacity(segment.files.len());
            for name in &segment.files {
                let bytes = self.store.read_file(segment.index, name)?;
                shas.push((name.clone(), ca::sha256_hex(&bytes)));
                parts.push(FilePart {
                    filename: name.clone(),
                    content_type: content_type_for(name),
                    bytes,
                });
            }
            if parts.is_empty() {
                // An empty sealed dir holds no data; drop it so it can't wedge.
                let _ = self.store.remove(segment.index);
                continue;
            }

            match self
                .client
                .ingest(&segment_key, &day, &self.platform, &parts)
                .await
            {
                Ok(response) if response.is_accepted() => {
                    let server_key = response.segment.clone().unwrap_or(segment_key.clone());
                    let listed = self.client.list_segments(&day).await?;
                    let confirmed = shas
                        .iter()
                        .all(|(_, sha)| listed.has_segment_sha(&server_key, sha));
                    if confirmed {
                        self.store.remove(segment.index)?;
                        confirmed_now += 1;
                        self.on_confirmed(&segment_key);
                    }
                    // If not yet confirmed, leave it on disk for the next tick.
                }
                Ok(response) => {
                    return Err(TransportError::Rejected {
                        status: 200,
                        body: format!("unexpected ingest status: {}", response.status),
                    });
                }
                Err(e) => {
                    self.on_error(&e);
                    return Err(e);
                }
            }
        }

        self.set_pending(self.store.scan().map(|s| s.len() as u64).unwrap_or(0));
        Ok(confirmed_now)
    }

    /// Run forever (until `shutdown`), ticking with exponential backoff on error.
    pub async fn run(self, mut shutdown: tokio::sync::oneshot::Receiver<()>) {
        let mut backoff = DEFAULT_UPLOAD_INTERVAL_SECS;
        loop {
            tokio::select! {
                _ = &mut shutdown => break,
                _ = tokio::time::sleep(Duration::from_secs(backoff)) => {
                    match self.tick().await {
                        Ok(_) => backoff = DEFAULT_UPLOAD_INTERVAL_SECS,
                        Err(_) => backoff = (backoff * 2).min(MAX_BACKOFF_SECS),
                    }
                }
            }
        }
    }

    fn set_pending(&self, pending: u64) {
        if let Ok(mut snapshot) = self.sync.lock() {
            snapshot.upload.pending_segments = pending;
        }
    }

    fn on_confirmed(&self, segment_key: &str) {
        if let Ok(mut snapshot) = self.sync.lock() {
            snapshot.upload.uploaded_segments += 1;
            snapshot.upload.last_uploaded_segment = Some(segment_key.to_string());
            snapshot.upload.last_error = None;
        }
    }

    fn on_error(&self, err: &TransportError) {
        if let Ok(mut snapshot) = self.sync.lock() {
            snapshot.upload.failed_segments += 1;
            snapshot.upload.last_error = Some(err.to_string());
        }
    }
}
