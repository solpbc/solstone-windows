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

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use observer_model::{SyncSnapshot, TransportPath};
use observer_pl::ca;
use observer_pl::civil;
use observer_pl::multipart::FilePart;
use observer_pl::wire::{IngestResponse, SegmentsResponse};
use observer_retention::RetentionConfig;

use crate::client::{ObserverClient, SendMetadata};
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

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

type IngestFuture<'a> = Pin<
    Box<dyn Future<Output = Result<(IngestResponse, SendMetadata), TransportError>> + Send + 'a>,
>;
type ListSegmentsFuture<'a> =
    Pin<Box<dyn Future<Output = Result<SegmentsResponse, TransportError>> + Send + 'a>>;

trait UploadClient: Send + Sync {
    fn ingest<'a>(
        &'a self,
        segment: &'a str,
        day: &'a str,
        platform: &'a str,
        files: &'a [FilePart],
    ) -> IngestFuture<'a>;

    fn list_segments<'a>(&'a self, day: &'a str) -> ListSegmentsFuture<'a>;
}

impl UploadClient for ObserverClient {
    fn ingest<'a>(
        &'a self,
        segment: &'a str,
        day: &'a str,
        platform: &'a str,
        files: &'a [FilePart],
    ) -> IngestFuture<'a> {
        Box::pin(ObserverClient::ingest(self, segment, day, platform, files))
    }

    fn list_segments<'a>(&'a self, day: &'a str) -> ListSegmentsFuture<'a> {
        Box::pin(ObserverClient::list_segments(self, day))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UploadOutcome {
    Confirmed,
    AcceptedUnconfirmed,
    Failed,
}

impl UploadOutcome {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::AcceptedUnconfirmed => "accepted_unconfirmed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UploadEvent {
    segment_key: String,
    bytes: u64,
    duration_ms: u64,
    outcome: UploadOutcome,
    path: Option<TransportPath>,
    reason: Option<String>,
}

impl UploadEvent {
    fn new(
        segment_key: impl Into<String>,
        bytes: u64,
        duration_ms: u64,
        outcome: UploadOutcome,
        path: Option<TransportPath>,
        reason: Option<String>,
    ) -> Self {
        Self {
            segment_key: segment_key.into(),
            bytes,
            duration_ms,
            outcome,
            path,
            reason,
        }
    }

    fn emit(&self) {
        let outcome = self.outcome.as_str();
        match (self.outcome, self.path, self.reason.as_deref()) {
            (UploadOutcome::Failed, Some(path), Some(reason)) => tracing::warn!(
                target: "pl_upload",
                segment = self.segment_key.as_str(),
                bytes = self.bytes,
                duration_ms = self.duration_ms,
                outcome,
                path = path.as_str(),
                reason,
                "upload event"
            ),
            (UploadOutcome::Failed, None, Some(reason)) => tracing::warn!(
                target: "pl_upload",
                segment = self.segment_key.as_str(),
                bytes = self.bytes,
                duration_ms = self.duration_ms,
                outcome,
                reason,
                "upload event"
            ),
            (UploadOutcome::Failed, Some(path), None) => tracing::warn!(
                target: "pl_upload",
                segment = self.segment_key.as_str(),
                bytes = self.bytes,
                duration_ms = self.duration_ms,
                outcome,
                path = path.as_str(),
                "upload event"
            ),
            (UploadOutcome::Failed, None, None) => tracing::warn!(
                target: "pl_upload",
                segment = self.segment_key.as_str(),
                bytes = self.bytes,
                duration_ms = self.duration_ms,
                outcome,
                "upload event"
            ),
            (_, Some(path), _) => tracing::info!(
                target: "pl_upload",
                segment = self.segment_key.as_str(),
                bytes = self.bytes,
                duration_ms = self.duration_ms,
                outcome,
                path = path.as_str(),
                "upload event"
            ),
            _ => tracing::info!(
                target: "pl_upload",
                segment = self.segment_key.as_str(),
                bytes = self.bytes,
                duration_ms = self.duration_ms,
                outcome,
                "upload event"
            ),
        }
    }
}

/// Drives sealed segments to the journal and reconciles them.
pub struct UploadCoordinator {
    client: Arc<dyn UploadClient>,
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
        Self::new_with_client(client, store, sync, platform, period_secs, retention)
    }

    fn new_with_client(
        client: Arc<dyn UploadClient>,
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
            let bytes = parts.iter().map(|part| part.bytes.len() as u64).sum();

            let started = Instant::now();
            match self
                .client
                .ingest(&segment_key, &day, &self.platform, &parts)
                .await
            {
                Ok((response, metadata)) if response.is_accepted() => {
                    let duration_ms = elapsed_ms(started);
                    let server_key = response.segment.clone().unwrap_or(segment_key.clone());
                    let listed = self.client.list_segments(&day).await;
                    let confirmed = match &listed {
                        Ok(listed) => shas
                            .iter()
                            .all(|(_, sha)| listed.has_segment_sha(&server_key, sha)),
                        Err(_) => false,
                    };
                    UploadEvent::new(
                        &segment_key,
                        bytes,
                        duration_ms,
                        if confirmed {
                            UploadOutcome::Confirmed
                        } else {
                            UploadOutcome::AcceptedUnconfirmed
                        },
                        Some(metadata.path),
                        None,
                    )
                    .emit();
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
                        self.on_confirmed(
                            &segment_key,
                            bytes,
                            duration_ms,
                            metadata.path,
                            metadata.attempts,
                        );
                    }
                    match listed {
                        Ok(_) => {}
                        Err(error) => return Err(error),
                    }
                    // If not yet confirmed, leave it on disk for the next tick.
                }
                Ok((response, metadata)) => {
                    let duration_ms = elapsed_ms(started);
                    let error = TransportError::Rejected {
                        status: 200,
                        body: format!("unexpected ingest status: {}", response.status),
                    };
                    UploadEvent::new(
                        &segment_key,
                        bytes,
                        duration_ms,
                        UploadOutcome::Failed,
                        Some(metadata.path),
                        Some(transport_error_code(&error)),
                    )
                    .emit();
                    return Err(error);
                }
                Err(e) => {
                    let duration_ms = elapsed_ms(started);
                    UploadEvent::new(
                        &segment_key,
                        bytes,
                        duration_ms,
                        UploadOutcome::Failed,
                        None,
                        Some(transport_error_code(&e)),
                    )
                    .emit();
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

    fn on_confirmed(
        &self,
        segment_key: &str,
        bytes: u64,
        duration_ms: u64,
        path: TransportPath,
        attempts: u32,
    ) {
        if let Ok(mut snapshot) = self.sync.lock() {
            snapshot.upload.uploaded_segments += 1;
            // Walk pending down live, in lockstep with delivered, so the home pane
            // doesn't freeze pending at its tick-start count while delivered climbs.
            // The end-of-tick rescan (set_pending after the loop) stays authoritative.
            snapshot.upload.pending_segments = snapshot.upload.pending_segments.saturating_sub(1);
            snapshot.upload.last_uploaded_segment = Some(segment_key.to_string());
            snapshot.upload.last_error = None;
            snapshot.upload.last_upload_duration_ms = Some(duration_ms);
            snapshot.upload.last_upload_bytes = Some(bytes);
            snapshot.upload.last_upload_path = Some(path);
            snapshot.upload.last_upload_dial_attempts = Some(attempts);
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
    use std::collections::VecDeque;

    use observer_model::RECENT_ERROR_COUNT_MAX;
    use observer_pl::wire::{ServerFile, ServerSegment};
    use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};

    use crate::credential::{Credential, EndpointAddr};
    use crate::sealed::SealedSegment;

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

    struct OneSegmentStore {
        removed: Mutex<bool>,
        segment: SealedSegment,
        file_name: String,
        bytes: Vec<u8>,
    }

    impl OneSegmentStore {
        fn new(boundary_epoch_secs: u64, file_name: &str, bytes: Vec<u8>) -> Self {
            Self {
                removed: Mutex::new(false),
                segment: SealedSegment {
                    index: 1,
                    boundary_epoch_secs,
                    files: vec![file_name.to_string()],
                },
                file_name: file_name.to_string(),
                bytes,
            }
        }
    }

    impl SealedStore for OneSegmentStore {
        fn scan(&self) -> std::io::Result<Vec<SealedSegment>> {
            if *self.removed.lock().unwrap() {
                Ok(Vec::new())
            } else {
                Ok(vec![self.segment.clone()])
            }
        }

        fn read_file(&self, _index: u64, name: &str) -> std::io::Result<Vec<u8>> {
            assert_eq!(name, self.file_name);
            Ok(self.bytes.clone())
        }

        fn remove(&self, _index: u64) -> std::io::Result<()> {
            *self.removed.lock().unwrap() = true;
            Ok(())
        }

        fn mark_confirmed(&self, _index: u64) -> std::io::Result<()> {
            *self.removed.lock().unwrap() = true;
            Ok(())
        }

        fn confirmed(&self) -> std::io::Result<Vec<SealedSegment>> {
            Ok(Vec::new())
        }
    }

    struct FakeClient {
        ingests: Mutex<VecDeque<Result<(IngestResponse, SendMetadata), TransportError>>>,
        lists: Mutex<VecDeque<Result<SegmentsResponse, TransportError>>>,
    }

    impl FakeClient {
        fn new(
            ingests: Vec<Result<(IngestResponse, SendMetadata), TransportError>>,
            lists: Vec<Result<SegmentsResponse, TransportError>>,
        ) -> Arc<Self> {
            Arc::new(Self {
                ingests: Mutex::new(VecDeque::from(ingests)),
                lists: Mutex::new(VecDeque::from(lists)),
            })
        }
    }

    impl UploadClient for FakeClient {
        fn ingest<'a>(
            &'a self,
            _segment: &'a str,
            _day: &'a str,
            _platform: &'a str,
            _files: &'a [FilePart],
        ) -> IngestFuture<'a> {
            let result = self
                .ingests
                .lock()
                .unwrap()
                .pop_front()
                .expect("scripted ingest result");
            Box::pin(async move { result })
        }

        fn list_segments<'a>(&'a self, _day: &'a str) -> ListSegmentsFuture<'a> {
            let result = self
                .lists
                .lock()
                .unwrap()
                .pop_front()
                .expect("scripted list result");
            Box::pin(async move { result })
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

    fn coordinator_with_client(
        client: Arc<dyn UploadClient>,
        store: Box<dyn SealedStore>,
        sync: Arc<Mutex<SyncSnapshot>>,
    ) -> UploadCoordinator {
        UploadCoordinator::new_with_client(
            client,
            store,
            sync,
            "windows",
            300,
            Arc::new(RwLock::new(RetentionConfig::default())),
        )
    }

    fn accepted_ingest(attempts: u32) -> Result<(IngestResponse, SendMetadata), TransportError> {
        Ok((
            IngestResponse {
                status: "ok".into(),
                segment: None,
                existing_segment: None,
                files: None,
                bytes: None,
            },
            SendMetadata {
                path: TransportPath::Direct,
                attempts,
            },
        ))
    }

    fn empty_segments() -> Result<SegmentsResponse, TransportError> {
        Ok(SegmentsResponse {
            items: Vec::new(),
            total: Some(0),
            protocol_version: Some(2),
        })
    }

    fn confirmed_segments(
        segment_key: String,
        file_name: &str,
        sha: String,
        size: u64,
    ) -> Result<SegmentsResponse, TransportError> {
        Ok(SegmentsResponse {
            items: vec![ServerSegment {
                key: segment_key,
                files: vec![ServerFile {
                    name: file_name.to_string(),
                    sha256: Some(sha),
                    size: Some(size),
                }],
            }],
            total: Some(1),
            protocol_version: Some(2),
        })
    }

    #[test]
    fn upload_event_records_three_outcomes() {
        let confirmed = UploadEvent::new(
            "120000_300",
            1024,
            12,
            UploadOutcome::Confirmed,
            Some(TransportPath::Direct),
            None,
        );
        assert_eq!(confirmed.outcome.as_str(), "confirmed");
        assert_eq!(confirmed.path, Some(TransportPath::Direct));
        assert_eq!(confirmed.reason, None);

        let accepted_unconfirmed = UploadEvent::new(
            "120000_300",
            2048,
            20,
            UploadOutcome::AcceptedUnconfirmed,
            Some(TransportPath::Relay),
            None,
        );
        assert_eq!(
            accepted_unconfirmed.outcome.as_str(),
            "accepted_unconfirmed"
        );
        assert_eq!(accepted_unconfirmed.path, Some(TransportPath::Relay));
        assert_eq!(accepted_unconfirmed.reason, None);

        let failed = UploadEvent::new(
            "120000_300",
            4096,
            30,
            UploadOutcome::Failed,
            None,
            Some("http_503".into()),
        );
        assert_eq!(failed.outcome.as_str(), "failed");
        assert_eq!(failed.path, None);
        assert_eq!(failed.reason.as_deref(), Some("http_503"));
    }

    #[test]
    fn upload_event_failed_reason_uses_redacted_error_codes() {
        let errors = [
            TransportError::Rejected {
                status: 503,
                body: "SECRET https://10.0.0.5/y?token=abc C:\\Users\\me\\seg.mp4 sha256:abc"
                    .into(),
            },
            TransportError::Io(std::io::Error::other("C:\\Users\\me\\token")),
        ];

        for error in errors {
            let event = UploadEvent::new(
                "120000_300",
                1,
                1,
                UploadOutcome::Failed,
                None,
                Some(transport_error_code(&error)),
            );
            event.emit();
            let reason = event.reason.as_deref().unwrap();
            assert!(!reason.contains("SECRET"));
            assert!(!reason.contains("Users"));
            assert!(!reason.contains("https://"));
            assert!(!reason.contains("token"));
            assert!(!reason.contains("sha256"));
            assert!(!reason.contains("10.0.0.5"));
        }
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

    #[tokio::test]
    async fn accepted_unconfirmed_does_not_set_earned_upload_fields_until_confirmed_once() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let boundary = 1_700_000_100;
        let file_name = "display_1_screen.mp4";
        let bytes = b"segment bytes".to_vec();
        let sha = ca::sha256_hex(&bytes);
        let segment_key = civil::segment_key_string(boundary, 300);
        let client = FakeClient::new(
            vec![accepted_ingest(2), accepted_ingest(3)],
            vec![
                empty_segments(),
                confirmed_segments(segment_key.clone(), file_name, sha, bytes.len() as u64),
            ],
        );
        let coordinator = coordinator_with_client(
            client,
            Box::new(OneSegmentStore::new(boundary, file_name, bytes.clone())),
            sync.clone(),
        );

        let first = coordinator.tick().await.unwrap();
        let first_snapshot = sync.lock().unwrap().clone();
        assert_eq!(first, 0);
        assert_eq!(first_snapshot.upload.uploaded_segments, 0);
        assert_eq!(first_snapshot.upload.last_upload_duration_ms, None);
        assert_eq!(first_snapshot.upload.last_upload_bytes, None);
        assert_eq!(first_snapshot.upload.last_upload_path, None);
        assert_eq!(first_snapshot.upload.last_upload_dial_attempts, None);

        let second = coordinator.tick().await.unwrap();
        let second_snapshot = sync.lock().unwrap().clone();
        assert_eq!(second, 1);
        assert_eq!(second_snapshot.upload.uploaded_segments, 1);
        assert_eq!(
            second_snapshot.upload.last_uploaded_segment.as_deref(),
            Some(segment_key.as_str())
        );
        assert_eq!(
            second_snapshot.upload.last_upload_bytes,
            Some(bytes.len() as u64)
        );
        assert_eq!(
            second_snapshot.upload.last_upload_path,
            Some(TransportPath::Direct)
        );
        assert_eq!(second_snapshot.upload.last_upload_dial_attempts, Some(3));
        let duration = second_snapshot.upload.last_upload_duration_ms;
        assert!(duration.is_some());

        let third = coordinator.tick().await.unwrap();
        let third_snapshot = sync.lock().unwrap().clone();
        assert_eq!(third, 0);
        assert_eq!(third_snapshot.upload.uploaded_segments, 1);
        assert_eq!(
            third_snapshot.upload.last_upload_bytes,
            Some(bytes.len() as u64)
        );
        assert_eq!(third_snapshot.upload.last_upload_duration_ms, duration);
    }
}
