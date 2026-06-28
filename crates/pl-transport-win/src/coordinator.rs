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
//! [`SyncSnapshot`] the engine folds into the health dump. Tick results also
//! maintain the diagnostics-only health beacon fields: consecutive failure code
//! and last successful sync epoch milliseconds.

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use observer_model::SyncSnapshot;
use observer_pl::ca;
use observer_pl::civil;
use observer_pl::multipart::FilePart;
use observer_retention::RetentionConfig;

use crate::client::ObserverClient;
use crate::sealed::{content_type_for, SealedStore};
use crate::{transport_error_code, TransportError, DEFAULT_UPLOAD_INTERVAL_SECS};

const MAX_BACKOFF_SECS: u64 = 300;

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn now_epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Drives sealed segments to the journal and reconciles them.
pub struct UploadCoordinator {
    client: Arc<ObserverClient>,
    store: Box<dyn SealedStore>,
    sync: Arc<Mutex<SyncSnapshot>>,
    platform: String,
    period_secs: u64,
    /// Owner cache-retention policy (shared, edited over IPC). Decides whether a
    /// confirmed segment is deleted on confirmation (don't-keep) or retained and
    /// pruned past the window.
    retention: Arc<RwLock<RetentionConfig>>,
}

impl UploadCoordinator {
    pub fn new(
        client: Arc<ObserverClient>,
        store: Box<dyn SealedStore>,
        sync: Arc<Mutex<SyncSnapshot>>,
        platform: impl Into<String>,
        period_secs: u64,
        retention: Arc<RwLock<RetentionConfig>>,
    ) -> Self {
        Self {
            client,
            store,
            sync,
            platform: platform.into(),
            period_secs: period_secs.max(1),
            retention,
        }
    }

    /// The current retention policy (defaulting on a poisoned lock).
    fn retention(&self) -> RetentionConfig {
        self.retention.read().map(|r| *r).unwrap_or_default()
    }

    /// Prune confirmed-and-retained local segments older than the retention
    /// window. Only ever removes **confirmed-uploaded** segments — unsynced local
    /// data is never deleted (the covenant guard). No-op for don't-keep (nothing
    /// is retained) and forever.
    fn prune_retained(&self) {
        let policy = self.retention();
        if policy.delete_on_confirm() || policy.is_forever() {
            return;
        }
        let now = now_epoch_secs();
        if let Ok(confirmed) = self.store.confirmed() {
            for segment in confirmed {
                if policy.should_prune(segment.boundary_epoch_secs, now) {
                    let _ = self.store.remove(segment.index);
                }
            }
        }
    }

    /// One pass: upload + reconcile every sealed segment currently on disk.
    /// Returns the number of segments confirmed landed this pass.
    pub async fn tick(&self) -> Result<usize, TransportError> {
        let result = self.tick_inner().await;
        match &result {
            Ok(_) => self.note_tick_success(),
            Err(error) => self.note_tick_failure(error),
        }
        result
    }

    async fn tick_inner(&self) -> Result<usize, TransportError> {
        // Prune retained-and-confirmed segments past the window first (cheap, local).
        self.prune_retained();

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
                        // Honor retention: delete the local copy now (don't-keep)
                        // or retain it (mark confirmed so it isn't re-uploaded) for
                        // the prune pass to remove once it's past the window.
                        if self.retention().delete_on_confirm() {
                            self.store.remove(segment.index)?;
                        } else {
                            self.store.mark_confirmed(segment.index)?;
                        }
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

    fn note_tick_success(&self) {
        if let Ok(mut snapshot) = self.sync.lock() {
            snapshot.upload.record_success(now_epoch_millis());
        }
    }

    fn note_tick_failure(&self, err: &TransportError) {
        if let Ok(mut snapshot) = self.sync.lock() {
            snapshot.upload.record_failure(&transport_error_code(err));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use observer_model::RECENT_ERROR_COUNT_MAX;
    use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};

    use crate::credential::{Credential, EndpointAddr};

    struct EmptyStore;

    impl SealedStore for EmptyStore {
        fn scan(&self) -> std::io::Result<Vec<crate::sealed::SealedSegment>> {
            Ok(Vec::new())
        }

        fn read_file(&self, _index: u64, _name: &str) -> std::io::Result<Vec<u8>> {
            unreachable!("empty store has no files")
        }

        fn remove(&self, _index: u64) -> std::io::Result<()> {
            Ok(())
        }

        fn mark_confirmed(&self, _index: u64) -> std::io::Result<()> {
            Ok(())
        }

        fn confirmed(&self) -> std::io::Result<Vec<crate::sealed::SealedSegment>> {
            Ok(Vec::new())
        }
    }

    struct FailingStore;

    impl SealedStore for FailingStore {
        fn scan(&self) -> std::io::Result<Vec<crate::sealed::SealedSegment>> {
            Err(std::io::Error::other("C:\\Users\\me\\seg.mp4"))
        }

        fn read_file(&self, _index: u64, _name: &str) -> std::io::Result<Vec<u8>> {
            unreachable!("scan fails before files are read")
        }

        fn remove(&self, _index: u64) -> std::io::Result<()> {
            Ok(())
        }

        fn mark_confirmed(&self, _index: u64) -> std::io::Result<()> {
            Ok(())
        }

        fn confirmed(&self) -> std::io::Result<Vec<crate::sealed::SealedSegment>> {
            Ok(Vec::new())
        }
    }

    fn dummy_client() -> Arc<ObserverClient> {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let params = CertificateParams::new(vec!["spl.local".to_string()]).unwrap();
        let cert = params.self_signed(&key).unwrap();
        let credential = Credential {
            client_key_pem: key.serialize_pem(),
            client_cert_pem: cert.pem(),
            ca_chain_pem: vec![cert.pem()],
            ca_fp_prefix: vec![0; 16],
            instance_id: "test".into(),
            home_label: "Home".into(),
            endpoints: vec![EndpointAddr {
                host: "127.0.0.1".into(),
                port: 9,
            }],
            relay_origin: None,
            device_token: None,
            device_token_expires_at: None,
        };
        Arc::new(
            ObserverClient::new(credential)
                .unwrap()
                .with_observer_key(Some("observer-key".into())),
        )
    }

    fn coordinator(
        store: Box<dyn SealedStore>,
        sync: Arc<Mutex<SyncSnapshot>>,
    ) -> UploadCoordinator {
        UploadCoordinator::new(
            dummy_client(),
            store,
            sync,
            "windows",
            300,
            Arc::new(RwLock::new(RetentionConfig::default())),
        )
    }

    #[tokio::test]
    async fn failed_tick_records_bounded_sanitized_reason() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let coordinator = coordinator(Box::new(FailingStore), sync.clone());

        for _ in 0..100 {
            assert!(coordinator.tick().await.is_err());
        }

        let snapshot = sync.lock().unwrap().clone();
        assert_eq!(snapshot.upload.recent_error_count, RECENT_ERROR_COUNT_MAX);
        assert_eq!(snapshot.upload.last_error_reason.as_deref(), Some("io"));
        assert_eq!(snapshot.upload.last_successful_sync, None);
    }

    #[tokio::test]
    async fn no_work_successful_tick_resets_reason_and_stamps_sync_time() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        {
            let mut snapshot = sync.lock().unwrap();
            snapshot.upload.record_failure("tls");
        }
        let coordinator = coordinator(Box::new(EmptyStore), sync.clone());

        let confirmed = coordinator.tick().await.unwrap();

        let snapshot = sync.lock().unwrap().clone();
        assert_eq!(confirmed, 0);
        assert_eq!(snapshot.upload.recent_error_count, 0);
        assert_eq!(snapshot.upload.last_error_reason, None);
        assert!(snapshot.upload.last_successful_sync.is_some());
    }
}
