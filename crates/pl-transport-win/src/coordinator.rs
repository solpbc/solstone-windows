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

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use observer_model::{LocalOffset, SyncSnapshot, TransportPath};
use observer_pl::ca;
use observer_pl::civil;
use observer_pl::multipart::FilePart;
use observer_pl::wire::{IngestResponse, SegmentsResponse};
use observer_retention::RetentionConfig;
use tokio::sync::watch;

use crate::client::{ObserverClient, SendMetadata};
use crate::sealed::{content_type_for, SealedStore};
use crate::{cancelled, transport_error_code, TransportError, DEFAULT_UPLOAD_INTERVAL_SECS};

const MAX_BACKOFF_SECS: u64 = 300;
const QUARANTINE_AFTER_REJECTS: u32 = 5;

fn is_reject_class(err: &TransportError) -> bool {
    match err {
        TransportError::Io(_) => false,
        TransportError::Tls(_) => false,
        TransportError::Crypto(_) => false,
        TransportError::Mux(_) => false,
        TransportError::Http(_) => false,
        TransportError::Json(_) => true,
        TransportError::PairLink(_) => false,
        TransportError::Pairing(_) => false,
        TransportError::Rejected { .. } => true,
        TransportError::Relay(_) => false,
        TransportError::RelayControlRejected { .. } => true,
        TransportError::NoEndpoint => false,
        TransportError::NotPaired => false,
        TransportError::LocalOffset => false,
    }
}

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
    local_offset: Arc<dyn LocalOffset>,
    quarantine_counts: Mutex<HashMap<u64, u32>>,
}

impl UploadCoordinator {
    pub fn new(
        client: Arc<ObserverClient>,
        store: Box<dyn SealedStore>,
        sync: Arc<Mutex<SyncSnapshot>>,
        platform: impl Into<String>,
        period_secs: u64,
        retention: Arc<RwLock<RetentionConfig>>,
        local_offset: Arc<dyn LocalOffset>,
    ) -> Self {
        Self::new_with_client(
            client,
            store,
            sync,
            platform,
            period_secs,
            retention,
            local_offset,
        )
    }

    fn new_with_client(
        client: Arc<dyn UploadClient>,
        store: Box<dyn SealedStore>,
        sync: Arc<Mutex<SyncSnapshot>>,
        platform: impl Into<String>,
        period_secs: u64,
        retention: Arc<RwLock<RetentionConfig>>,
        local_offset: Arc<dyn LocalOffset>,
    ) -> Self {
        Self {
            client,
            store,
            sync,
            platform: platform.into(),
            period_secs: period_secs.max(1),
            retention,
            local_offset,
            quarantine_counts: Mutex::new(HashMap::new()),
        }
    }

    /// The current retention policy (defaulting on a poisoned lock).
    fn retention(&self) -> RetentionConfig {
        self.retention.read().map(|r| *r).unwrap_or_default()
    }

    fn register_reject(&self, index: u64) {
        let should_quarantine = {
            let mut counts = self
                .quarantine_counts
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let count = counts.entry(index).or_insert(0);
            *count = count.saturating_add(1);
            *count >= QUARANTINE_AFTER_REJECTS
        };

        if !should_quarantine {
            return;
        }

        match self.store.quarantine(index) {
            Ok(()) => {
                self.on_quarantined();
                self.quarantine_counts
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .remove(&index);
            }
            Err(error) => tracing::warn!(
                target: "pl_upload",
                index,
                reason = "quarantine_failed",
                kind = ?error.kind(),
                "quarantine failed"
            ),
        }
    }

    fn clear_reject(&self, index: u64) {
        self.quarantine_counts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&index);
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
        match self.store.confirmed() {
            Ok(confirmed) => {
                for segment in confirmed {
                    if policy.should_prune(segment.boundary_epoch_secs, now) {
                        if let Err(error) = self.store.remove(segment.index) {
                            tracing::warn!(
                                target: "pl_upload",
                                index = segment.index,
                                reason = "retention_prune_failed",
                                kind = ?error.kind(),
                                "prune remove failed"
                            );
                        }
                    }
                }
            }
            Err(error) => tracing::warn!(
                target: "pl_upload",
                reason = "retention_scan_failed",
                kind = ?error.kind(),
                "prune scan failed"
            ),
        }
    }

    /// One pass: upload + reconcile every sealed segment currently on disk.
    /// Returns the number of segments confirmed landed this pass.
    pub async fn tick(&self) -> Result<usize, TransportError> {
        let (_tx, rx) = watch::channel(false);
        self.tick_with_cancel(&rx).await
    }

    async fn tick_with_cancel(
        &self,
        cancel: &watch::Receiver<bool>,
    ) -> Result<usize, TransportError> {
        let result = self.tick_inner(cancel).await;
        match &result {
            Ok(_) => self.note_tick_success(),
            Err(error) => self.note_tick_failure(error),
        }
        result
    }

    async fn tick_inner(&self, cancel: &watch::Receiver<bool>) -> Result<usize, TransportError> {
        // Prune retained-and-confirmed segments past the window first (cheap, local).
        self.prune_retained();

        let segments = self.store.scan()?;
        self.set_pending(segments.len() as u64);
        let mut confirmed_now = 0usize;

        'segments: for segment in segments {
            if *cancel.borrow() {
                break 'segments;
            }
            let offset_started = Instant::now();
            let offset = match self
                .local_offset
                .local_offset_secs(segment.boundary_epoch_secs)
            {
                Ok(offset) => offset,
                Err(_) => {
                    let error = TransportError::LocalOffset;
                    UploadEvent::new(
                        format!("idx_{}", segment.index),
                        0,
                        elapsed_ms(offset_started),
                        UploadOutcome::Failed,
                        None,
                        Some(transport_error_code(&error)),
                    )
                    .emit();
                    self.on_error(&error);
                    return Err(error);
                }
            };
            let day = civil::day_string_local(segment.boundary_epoch_secs, offset);
            let segment_key = civil::segment_key_string_local(
                segment.boundary_epoch_secs,
                offset,
                segment.len_secs.unwrap_or(self.period_secs),
            );

            // Read the per-source files + compute their sha256 for reconcile.
            let read_started = Instant::now();
            let mut parts = Vec::with_capacity(segment.files.len());
            let mut shas = Vec::with_capacity(segment.files.len());
            for name in &segment.files {
                let bytes = match self.store.read_file(segment.index, name) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        let error = TransportError::Io(error);
                        UploadEvent::new(
                            &segment_key,
                            0,
                            elapsed_ms(read_started),
                            UploadOutcome::Failed,
                            None,
                            Some(transport_error_code(&error)),
                        )
                        .emit();
                        self.on_error(&error);
                        continue 'segments;
                    }
                };
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
                    let server_key = response
                        .segment
                        .clone()
                        .or_else(|| response.existing_segment.clone())
                        .unwrap_or_else(|| segment_key.clone());
                    let listed = self.client.list_segments(&day).await;
                    let confirmed = match &listed {
                        Ok(listed) => shas
                            .iter()
                            .all(|(name, sha)| listed.proves_file_held(&server_key, name, sha)),
                        Err(_) => false,
                    };
                    self.clear_reject(segment.index);
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
                            if let Err(error) = self.store.remove(segment.index) {
                                tracing::warn!(
                                    target: "pl_upload",
                                    segment = segment_key.as_str(),
                                    reason = "confirmed_remove_failed",
                                    kind = ?error.kind(),
                                    "confirmed cleanup failed"
                                );
                            }
                        } else {
                            if let Err(error) = self.store.mark_confirmed(segment.index) {
                                tracing::warn!(
                                    target: "pl_upload",
                                    segment = segment_key.as_str(),
                                    reason = "confirmed_mark_failed",
                                    kind = ?error.kind(),
                                    "confirmed cleanup failed"
                                );
                            }
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
                        Err(error) => {
                            if is_reject_class(&error) {
                                continue;
                            }
                            return Err(error);
                        }
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
                    self.on_error(&error);
                    self.register_reject(segment.index);
                    continue;
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
                    if is_reject_class(&e) {
                        self.register_reject(segment.index);
                        continue;
                    }
                    return Err(e);
                }
            }
        }

        self.set_pending(self.store.scan().map(|s| s.len() as u64).unwrap_or(0));
        Ok(confirmed_now)
    }

    /// Run forever (until `cancel`), ticking with exponential backoff on error.
    pub async fn run(self, mut cancel: watch::Receiver<bool>) {
        let tick_cancel = cancel.clone();
        let mut backoff = DEFAULT_UPLOAD_INTERVAL_SECS;
        loop {
            tokio::select! {
                _ = cancelled(&mut cancel) => break,
                _ = tokio::time::sleep(Duration::from_secs(backoff)) => {
                    match self.tick_with_cancel(&tick_cancel).await {
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
            snapshot.upload.last_error = Some(transport_error_code(err));
        }
    }

    fn on_quarantined(&self) {
        if let Ok(mut snapshot) = self.sync.lock() {
            snapshot.upload.quarantined_segments += 1;
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
    use std::collections::{HashSet, VecDeque};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use observer_model::RECENT_ERROR_COUNT_MAX;
    use observer_pl::wire::{ServerFile, ServerSegment};
    use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};

    use crate::credential::{Credential, EndpointAddr};
    use crate::sealed::SealedSegment;

    #[derive(Debug)]
    struct FixedOffset(i64);

    impl observer_model::LocalOffset for FixedOffset {
        fn local_offset_secs(
            &self,
            _epoch_secs: u64,
        ) -> Result<i64, observer_model::LocalOffsetError> {
            Ok(self.0)
        }
    }

    #[derive(Debug)]
    struct FailingOffset;

    impl observer_model::LocalOffset for FailingOffset {
        fn local_offset_secs(
            &self,
            _epoch_secs: u64,
        ) -> Result<i64, observer_model::LocalOffsetError> {
            Err(observer_model::LocalOffsetError::Lookup)
        }
    }

    #[derive(Debug)]
    struct FailOnceOffset {
        failed: Mutex<bool>,
        offset: i64,
    }

    impl FailOnceOffset {
        fn new(offset: i64) -> Self {
            Self {
                failed: Mutex::new(false),
                offset,
            }
        }
    }

    impl observer_model::LocalOffset for FailOnceOffset {
        fn local_offset_secs(
            &self,
            _epoch_secs: u64,
        ) -> Result<i64, observer_model::LocalOffsetError> {
            let mut failed = self.failed.lock().unwrap();
            if !*failed {
                *failed = true;
                Err(observer_model::LocalOffsetError::Lookup)
            } else {
                Ok(self.offset)
            }
        }
    }

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

        fn quarantine(&self, _index: u64) -> std::io::Result<()> {
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

        fn quarantine(&self, _index: u64) -> std::io::Result<()> {
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
        removed: Arc<Mutex<bool>>,
        segment: SealedSegment,
        file_name: String,
        bytes: Vec<u8>,
    }

    impl OneSegmentStore {
        fn new(boundary_epoch_secs: u64, file_name: &str, bytes: Vec<u8>) -> Self {
            Self {
                removed: Arc::new(Mutex::new(false)),
                segment: SealedSegment {
                    index: 1,
                    boundary_epoch_secs,
                    len_secs: None,
                    files: vec![file_name.to_string()],
                },
                file_name: file_name.to_string(),
                bytes,
            }
        }

        fn removed_handle(&self) -> Arc<Mutex<bool>> {
            self.removed.clone()
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

        fn quarantine(&self, _index: u64) -> std::io::Result<()> {
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

    #[derive(Clone)]
    struct MultiSegmentStore {
        state: Arc<Mutex<MultiSegmentState>>,
    }

    struct MultiSegmentState {
        segments: Vec<SealedSegment>,
        bytes: HashMap<u64, Vec<u8>>,
        read_errors: HashMap<u64, String>,
        removed: HashSet<u64>,
        confirmed: HashSet<u64>,
        quarantined: HashSet<u64>,
        remove_fails_once: HashSet<u64>,
        mark_confirmed_fails_once: HashSet<u64>,
    }

    impl MultiSegmentStore {
        fn new(segments: Vec<(u64, u64, &str, Vec<u8>)>) -> Self {
            let mut sealed = Vec::with_capacity(segments.len());
            let mut bytes = HashMap::new();
            for (index, boundary_epoch_secs, file_name, data) in segments {
                sealed.push(SealedSegment {
                    index,
                    boundary_epoch_secs,
                    len_secs: None,
                    files: vec![file_name.to_string()],
                });
                bytes.insert(index, data);
            }
            sealed.sort_by_key(|segment| segment.index);
            Self {
                state: Arc::new(Mutex::new(MultiSegmentState {
                    segments: sealed,
                    bytes,
                    read_errors: HashMap::new(),
                    removed: HashSet::new(),
                    confirmed: HashSet::new(),
                    quarantined: HashSet::new(),
                    remove_fails_once: HashSet::new(),
                    mark_confirmed_fails_once: HashSet::new(),
                })),
            }
        }

        fn with_read_error(self, index: u64, message: &str) -> Self {
            self.state
                .lock()
                .unwrap()
                .read_errors
                .insert(index, message.to_string());
            self
        }

        fn with_remove_fails_once(self, index: u64) -> Self {
            self.state.lock().unwrap().remove_fails_once.insert(index);
            self
        }

        fn with_mark_confirmed_fails_once(self, index: u64) -> Self {
            self.state
                .lock()
                .unwrap()
                .mark_confirmed_fails_once
                .insert(index);
            self
        }

        fn removed(&self, index: u64) -> bool {
            self.state.lock().unwrap().removed.contains(&index)
        }

        fn quarantined(&self, index: u64) -> bool {
            self.state.lock().unwrap().quarantined.contains(&index)
        }

        fn pending_indices(&self) -> Vec<u64> {
            self.scan()
                .unwrap()
                .into_iter()
                .map(|segment| segment.index)
                .collect()
        }
    }

    impl SealedStore for MultiSegmentStore {
        fn scan(&self) -> std::io::Result<Vec<SealedSegment>> {
            let state = self.state.lock().unwrap();
            Ok(state
                .segments
                .iter()
                .filter(|segment| {
                    !state.removed.contains(&segment.index)
                        && !state.confirmed.contains(&segment.index)
                        && !state.quarantined.contains(&segment.index)
                })
                .cloned()
                .collect())
        }

        fn read_file(&self, index: u64, _name: &str) -> std::io::Result<Vec<u8>> {
            let state = self.state.lock().unwrap();
            if let Some(message) = state.read_errors.get(&index) {
                return Err(std::io::Error::other(message.clone()));
            }
            state
                .bytes
                .get(&index)
                .cloned()
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "missing bytes"))
        }

        fn remove(&self, index: u64) -> std::io::Result<()> {
            let mut state = self.state.lock().unwrap();
            if state.remove_fails_once.remove(&index) {
                return Err(std::io::Error::other("C:\\Users\\me\\seg.mp4"));
            }
            state.removed.insert(index);
            Ok(())
        }

        fn quarantine(&self, index: u64) -> std::io::Result<()> {
            self.state.lock().unwrap().quarantined.insert(index);
            Ok(())
        }

        fn mark_confirmed(&self, index: u64) -> std::io::Result<()> {
            let mut state = self.state.lock().unwrap();
            if state.mark_confirmed_fails_once.remove(&index) {
                return Err(std::io::Error::other("C:\\Users\\me\\seg.mp4"));
            }
            state.confirmed.insert(index);
            Ok(())
        }

        fn confirmed(&self) -> std::io::Result<Vec<SealedSegment>> {
            let state = self.state.lock().unwrap();
            Ok(state
                .segments
                .iter()
                .filter(|segment| state.confirmed.contains(&segment.index))
                .cloned()
                .collect())
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

    struct CancelAfterFirstListClient {
        ingests: Mutex<VecDeque<Result<(IngestResponse, SendMetadata), TransportError>>>,
        lists: Mutex<VecDeque<Result<SegmentsResponse, TransportError>>>,
        cancel: watch::Sender<bool>,
        ingest_count: Arc<AtomicUsize>,
        list_count: AtomicUsize,
    }

    impl CancelAfterFirstListClient {
        fn new(
            ingests: Vec<Result<(IngestResponse, SendMetadata), TransportError>>,
            lists: Vec<Result<SegmentsResponse, TransportError>>,
            cancel: watch::Sender<bool>,
        ) -> Arc<Self> {
            Arc::new(Self {
                ingests: Mutex::new(VecDeque::from(ingests)),
                lists: Mutex::new(VecDeque::from(lists)),
                cancel,
                ingest_count: Arc::new(AtomicUsize::new(0)),
                list_count: AtomicUsize::new(0),
            })
        }

        fn ingest_count(&self) -> usize {
            self.ingest_count.load(Ordering::SeqCst)
        }
    }

    impl UploadClient for CancelAfterFirstListClient {
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
            let ingest_count = self.ingest_count.clone();
            Box::pin(async move {
                tokio::time::sleep(Duration::from_secs(1)).await;
                ingest_count.fetch_add(1, Ordering::SeqCst);
                result
            })
        }

        fn list_segments<'a>(&'a self, _day: &'a str) -> ListSegmentsFuture<'a> {
            let result = self
                .lists
                .lock()
                .unwrap()
                .pop_front()
                .expect("scripted list result");
            let should_cancel = self.list_count.fetch_add(1, Ordering::SeqCst) == 0;
            let cancel = self.cancel.clone();
            Box::pin(async move {
                if should_cancel {
                    let _ = cancel.send(true);
                }
                result
            })
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
        coordinator_with_offset(store, sync, Arc::new(FixedOffset(0)))
    }

    fn coordinator_with_offset(
        store: Box<dyn SealedStore>,
        sync: Arc<Mutex<SyncSnapshot>>,
        local_offset: Arc<dyn LocalOffset>,
    ) -> UploadCoordinator {
        UploadCoordinator::new(
            dummy_client(),
            store,
            sync,
            "windows",
            300,
            Arc::new(RwLock::new(RetentionConfig::default())),
            local_offset,
        )
    }

    fn coordinator_with_client(
        client: Arc<dyn UploadClient>,
        store: Box<dyn SealedStore>,
        sync: Arc<Mutex<SyncSnapshot>>,
    ) -> UploadCoordinator {
        coordinator_with_client_and_offset(client, store, sync, Arc::new(FixedOffset(0)))
    }

    fn coordinator_with_client_and_retention(
        client: Arc<dyn UploadClient>,
        store: Box<dyn SealedStore>,
        sync: Arc<Mutex<SyncSnapshot>>,
        retention: RetentionConfig,
    ) -> UploadCoordinator {
        UploadCoordinator::new_with_client(
            client,
            store,
            sync,
            "windows",
            300,
            Arc::new(RwLock::new(retention)),
            Arc::new(FixedOffset(0)),
        )
    }

    fn coordinator_with_client_and_offset(
        client: Arc<dyn UploadClient>,
        store: Box<dyn SealedStore>,
        sync: Arc<Mutex<SyncSnapshot>>,
        local_offset: Arc<dyn LocalOffset>,
    ) -> UploadCoordinator {
        UploadCoordinator::new_with_client(
            client,
            store,
            sync,
            "windows",
            300,
            Arc::new(RwLock::new(RetentionConfig::default())),
            local_offset,
        )
    }

    fn accepted_ingest(attempts: u32) -> Result<(IngestResponse, SendMetadata), TransportError> {
        scripted_ingest("ok", None, None, attempts)
    }

    fn scripted_ingest(
        status: &str,
        segment: Option<&str>,
        existing_segment: Option<&str>,
        attempts: u32,
    ) -> Result<(IngestResponse, SendMetadata), TransportError> {
        Ok((
            IngestResponse {
                status: status.into(),
                segment: segment.map(ToOwned::to_owned),
                existing_segment: existing_segment.map(ToOwned::to_owned),
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
        listed_segments(segment_key, file_name, sha, size, Some("present"))
    }

    fn listed_segments(
        segment_key: String,
        file_name: &str,
        sha: String,
        size: u64,
        status: Option<&str>,
    ) -> Result<SegmentsResponse, TransportError> {
        Ok(SegmentsResponse {
            items: vec![ServerSegment {
                key: segment_key,
                files: vec![ServerFile {
                    name: file_name.to_string(),
                    sha256: Some(sha),
                    size: Some(size),
                    status: status.map(ToOwned::to_owned),
                    submitted_name: None,
                }],
            }],
            total: Some(1),
            protocol_version: Some(2),
        })
    }

    fn adversarial_body() -> String {
        "SECRET https://10.0.0.5/y?token=abc C:\\Users\\me\\seg.mp4 sha256:abc".into()
    }

    #[test]
    fn is_reject_class_partitions_transport_errors() {
        let json_error = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        let cases = [
            (
                TransportError::Io(std::io::Error::other("C:\\Users\\me\\seg.mp4")),
                false,
            ),
            (TransportError::Tls("tls secret".into()), false),
            (TransportError::Crypto("crypto secret".into()), false),
            (
                TransportError::Mux(observer_pl::mux::MuxError::Incomplete),
                false,
            ),
            (
                TransportError::Http(observer_pl::http::HttpError::BadStatusLine(
                    "HTTP/1.1 SECRET".into(),
                )),
                false,
            ),
            (TransportError::Json(json_error), true),
            (TransportError::PairLink("token=abc".into()), false),
            (TransportError::Pairing("sha256:abc".into()), false),
            (
                TransportError::Rejected {
                    status: 503,
                    body: adversarial_body(),
                },
                true,
            ),
            (TransportError::Relay(crate::RelayError::HomeOffline), false),
            (
                TransportError::RelayControlRejected {
                    endpoint: crate::RelayControlEndpoint::EnrollDevice,
                    status: 409,
                },
                true,
            ),
            (TransportError::NoEndpoint, false),
            (TransportError::NotPaired, false),
            (TransportError::LocalOffset, false),
        ];

        for (error, expected) in cases {
            assert_eq!(is_reject_class(&error), expected, "{error:?}");
        }
    }

    #[tokio::test]
    async fn reject_isolation_processes_later_segments() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let file_name = "display_1_screen.mp4";
        let boundary1 = 1_700_000_100;
        let boundary2 = boundary1 + 300;
        let bytes1 = b"poison segment".to_vec();
        let bytes2 = b"healthy segment".to_vec();
        let key2 = civil::segment_key_string_local(boundary2, 0, 300);
        let sha2 = ca::sha256_hex(&bytes2);
        let store = MultiSegmentStore::new(vec![
            (1, boundary1, file_name, bytes1),
            (2, boundary2, file_name, bytes2.clone()),
        ]);
        let handle = store.clone();
        let client = FakeClient::new(
            vec![
                Err(TransportError::Rejected {
                    status: 503,
                    body: adversarial_body(),
                }),
                accepted_ingest(1),
            ],
            vec![confirmed_segments(
                key2,
                file_name,
                sha2,
                bytes2.len() as u64,
            )],
        );
        let coordinator = coordinator_with_client(client, Box::new(store), sync.clone());

        let confirmed = coordinator.tick().await.unwrap();
        let snapshot = sync.lock().unwrap().clone();

        assert_eq!(confirmed, 1);
        assert_eq!(handle.pending_indices(), vec![1]);
        assert!(handle.removed(2));
        assert_eq!(snapshot.upload.pending_segments, 1);
        assert!(snapshot.upload.failed_segments >= 1);
        assert_eq!(snapshot.upload.quarantined_segments, 0);
        assert_eq!(snapshot.upload.recent_error_count, 0);
    }

    #[tokio::test]
    async fn quarantines_segment_after_five_consecutive_rejects() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let file_name = "display_1_screen.mp4";
        let boundary = 1_700_000_100;
        let bytes = b"poison segment".to_vec();
        let store = MultiSegmentStore::new(vec![(1, boundary, file_name, bytes)]);
        let handle = store.clone();
        let client = FakeClient::new(
            (0..QUARANTINE_AFTER_REJECTS)
                .map(|_| {
                    Err(TransportError::Rejected {
                        status: 422,
                        body: adversarial_body(),
                    })
                })
                .collect(),
            vec![],
        );
        let coordinator = coordinator_with_client(client, Box::new(store), sync.clone());

        for tick in 1..=QUARANTINE_AFTER_REJECTS {
            assert_eq!(coordinator.tick().await.unwrap(), 0);
            let snapshot = sync.lock().unwrap().clone();
            if tick < QUARANTINE_AFTER_REJECTS {
                assert_eq!(snapshot.upload.quarantined_segments, 0);
                assert_eq!(handle.pending_indices(), vec![1]);
            }
        }

        let snapshot = sync.lock().unwrap().clone();
        assert!(handle.quarantined(1));
        assert!(handle.pending_indices().is_empty());
        assert_eq!(snapshot.upload.quarantined_segments, 1);
        assert_eq!(snapshot.upload.pending_segments, 0);
        assert_eq!(snapshot.upload.recent_error_count, 0);
        assert_eq!(snapshot.upload.last_error.as_deref(), Some("http_422"));
        let last_error = snapshot.upload.last_error.unwrap();
        assert!(!last_error.contains("SECRET"));
        assert!(!last_error.contains("token"));
        assert!(!last_error.contains("Users"));
        assert!(!last_error.contains("https://"));
        assert!(!last_error.contains("sha256"));
        assert!(!last_error.contains("10.0.0.5"));
    }

    #[tokio::test]
    async fn transport_error_aborts_tick_and_leaves_rest_untried() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let file_name = "display_1_screen.mp4";
        let boundary1 = 1_700_000_100;
        let boundary2 = boundary1 + 300;
        let store = MultiSegmentStore::new(vec![
            (1, boundary1, file_name, b"first".to_vec()),
            (2, boundary2, file_name, b"second".to_vec()),
        ]);
        let client = FakeClient::new(
            vec![
                Err(TransportError::Io(std::io::Error::other(
                    "C:\\Users\\me\\seg.mp4",
                ))),
                accepted_ingest(1),
            ],
            vec![],
        );
        let coordinator = coordinator_with_client(client.clone(), Box::new(store), sync.clone());

        let result = coordinator.tick().await;
        let snapshot = sync.lock().unwrap().clone();

        assert!(matches!(result, Err(TransportError::Io(_))));
        assert_eq!(client.ingests.lock().unwrap().len(), 1);
        assert_eq!(snapshot.upload.quarantined_segments, 0);
        assert_eq!(snapshot.upload.recent_error_count, 1);
        assert_eq!(snapshot.upload.last_error.as_deref(), Some("io"));
        assert!(!snapshot
            .upload
            .last_error
            .as_deref()
            .unwrap()
            .contains("Users"));
    }

    #[tokio::test]
    async fn read_error_skips_segment_without_quarantine() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let file_name = "display_1_screen.mp4";
        let boundary1 = 1_700_000_100;
        let boundary2 = boundary1 + 300;
        let bytes2 = b"healthy segment".to_vec();
        let key2 = civil::segment_key_string_local(boundary2, 0, 300);
        let sha2 = ca::sha256_hex(&bytes2);
        let store = MultiSegmentStore::new(vec![
            (1, boundary1, file_name, b"locked".to_vec()),
            (2, boundary2, file_name, bytes2.clone()),
        ])
        .with_read_error(1, "C:\\Users\\me\\seg.mp4");
        let handle = store.clone();
        let client = FakeClient::new(
            vec![accepted_ingest(1)],
            vec![confirmed_segments(
                key2,
                file_name,
                sha2,
                bytes2.len() as u64,
            )],
        );
        let coordinator = coordinator_with_client(client, Box::new(store), sync.clone());

        let confirmed = coordinator.tick().await.unwrap();
        let snapshot = sync.lock().unwrap().clone();

        assert_eq!(confirmed, 1);
        assert_eq!(handle.pending_indices(), vec![1]);
        assert!(handle.removed(2));
        assert_eq!(snapshot.upload.quarantined_segments, 0);
        assert!(snapshot.upload.failed_segments >= 1);
    }

    #[tokio::test]
    async fn list_reject_does_not_feed_quarantine_counter() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let file_name = "display_1_screen.mp4";
        let boundary = 1_700_000_100;
        let store = MultiSegmentStore::new(vec![(1, boundary, file_name, b"segment".to_vec())]);
        let handle = store.clone();
        let mut ingests = Vec::new();
        for _ in 0..(QUARANTINE_AFTER_REJECTS - 1) {
            ingests.push(Err(TransportError::Rejected {
                status: 503,
                body: adversarial_body(),
            }));
        }
        ingests.push(accepted_ingest(1));
        let client = FakeClient::new(
            ingests,
            vec![Err(TransportError::Rejected {
                status: 403,
                body: adversarial_body(),
            })],
        );
        let coordinator = coordinator_with_client(client, Box::new(store), sync.clone());

        for _ in 0..(QUARANTINE_AFTER_REJECTS - 1) {
            assert_eq!(coordinator.tick().await.unwrap(), 0);
        }
        assert_eq!(coordinator.tick().await.unwrap(), 0);
        let snapshot = sync.lock().unwrap().clone();

        assert_eq!(handle.pending_indices(), vec![1]);
        assert!(!handle.quarantined(1));
        assert_eq!(snapshot.upload.quarantined_segments, 0);
        assert_eq!(snapshot.upload.recent_error_count, 0);
    }

    #[tokio::test]
    async fn list_transport_error_aborts_after_accepted_ingest() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let file_name = "display_1_screen.mp4";
        let boundary = 1_700_000_100;
        let store = MultiSegmentStore::new(vec![(1, boundary, file_name, b"segment".to_vec())]);
        let client = FakeClient::new(
            vec![accepted_ingest(1)],
            vec![Err(TransportError::Io(std::io::Error::other(
                "C:\\Users\\me\\seg.mp4",
            )))],
        );
        let coordinator = coordinator_with_client(client, Box::new(store), sync.clone());

        let result = coordinator.tick().await;
        let snapshot = sync.lock().unwrap().clone();

        assert!(matches!(result, Err(TransportError::Io(_))));
        assert_eq!(snapshot.upload.quarantined_segments, 0);
        assert_eq!(snapshot.upload.recent_error_count, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_between_segments_stops_before_next_ingest() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let file_name = "display_1_screen.mp4";
        let boundary1 = 1_700_000_100;
        let boundary2 = boundary1 + 300;
        let boundary3 = boundary2 + 300;
        let bytes1 = b"first segment".to_vec();
        let bytes2 = b"second segment".to_vec();
        let bytes3 = b"third segment".to_vec();
        let key1 = civil::segment_key_string_local(boundary1, 0, 300);
        let sha1 = ca::sha256_hex(&bytes1);
        let store = MultiSegmentStore::new(vec![
            (1, boundary1, file_name, bytes1.clone()),
            (2, boundary2, file_name, bytes2),
            (3, boundary3, file_name, bytes3),
        ]);
        let handle = store.clone();
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let client = CancelAfterFirstListClient::new(
            vec![accepted_ingest(1)],
            vec![confirmed_segments(
                key1,
                file_name,
                sha1,
                bytes1.len() as u64,
            )],
            cancel_tx,
        );
        let coordinator = coordinator_with_client(client.clone(), Box::new(store), sync.clone());

        let run_task = tokio::spawn(coordinator.run(cancel_rx));
        tokio::time::advance(Duration::from_secs(DEFAULT_UPLOAD_INTERVAL_SECS)).await;
        tokio::time::advance(Duration::from_secs(1)).await;
        run_task.await.unwrap();

        assert_eq!(client.ingest_count(), 1);
        assert_eq!(handle.pending_indices(), vec![2, 3]);
        assert_eq!(sync.lock().unwrap().upload.uploaded_segments, 1);
    }

    #[tokio::test]
    async fn cleanup_remove_failure_is_nonfatal_and_reconfirms_next_tick() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let file_name = "display_1_screen.mp4";
        let boundary1 = 1_700_000_100;
        let boundary2 = boundary1 + 300;
        let bytes1 = b"first segment".to_vec();
        let bytes2 = b"second segment".to_vec();
        let key1 = civil::segment_key_string_local(boundary1, 0, 300);
        let key2 = civil::segment_key_string_local(boundary2, 0, 300);
        let sha1 = ca::sha256_hex(&bytes1);
        let sha2 = ca::sha256_hex(&bytes2);
        let store = MultiSegmentStore::new(vec![
            (1, boundary1, file_name, bytes1.clone()),
            (2, boundary2, file_name, bytes2.clone()),
        ])
        .with_remove_fails_once(1);
        let handle = store.clone();
        let client = FakeClient::new(
            vec![
                accepted_ingest(1),
                accepted_ingest(1),
                scripted_ingest("duplicate", None, Some(&key1), 2),
            ],
            vec![
                confirmed_segments(key1.clone(), file_name, sha1.clone(), bytes1.len() as u64),
                confirmed_segments(key2, file_name, sha2, bytes2.len() as u64),
                confirmed_segments(key1, file_name, sha1, bytes1.len() as u64),
            ],
        );
        let coordinator = coordinator_with_client(client.clone(), Box::new(store), sync.clone());

        let first = coordinator.tick().await.unwrap();
        assert_eq!(first, 2);
        assert_eq!(client.ingests.lock().unwrap().len(), 1);
        assert!(!handle.removed(1));
        assert!(handle.removed(2));
        assert_eq!(handle.pending_indices(), vec![1]);

        let second = coordinator.tick().await.unwrap();
        assert_eq!(second, 1);
        assert!(handle.removed(1));
        assert!(handle.pending_indices().is_empty());
    }

    #[tokio::test]
    async fn cleanup_mark_confirmed_failure_is_nonfatal_and_reconfirms_next_tick() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let file_name = "display_1_screen.mp4";
        let boundary = 1_700_000_100;
        let bytes = b"retained segment".to_vec();
        let key = civil::segment_key_string_local(boundary, 0, 300);
        let sha = ca::sha256_hex(&bytes);
        let store = MultiSegmentStore::new(vec![(1, boundary, file_name, bytes.clone())])
            .with_mark_confirmed_fails_once(1);
        let handle = store.clone();
        let client = FakeClient::new(
            vec![
                accepted_ingest(1),
                scripted_ingest("duplicate", None, Some(&key), 2),
            ],
            vec![
                confirmed_segments(key.clone(), file_name, sha.clone(), bytes.len() as u64),
                confirmed_segments(key, file_name, sha, bytes.len() as u64),
            ],
        );
        let coordinator = coordinator_with_client_and_retention(
            client,
            Box::new(store),
            sync,
            RetentionConfig { keep_days: 1 },
        );

        let first = coordinator.tick().await.unwrap();
        assert_eq!(first, 1);
        assert_eq!(handle.pending_indices(), vec![1]);

        let second = coordinator.tick().await.unwrap();
        assert_eq!(second, 1);
        assert!(handle.pending_indices().is_empty());
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
        let segment_key = civil::segment_key_string_local(boundary, 0, 300);
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

    #[tokio::test]
    async fn local_offset_failure_aborts_without_submitting_key_and_retries() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let boundary = 1_700_000_100;
        let file_name = "display_1_screen.mp4";
        let bytes = b"segment bytes".to_vec();
        let sha = ca::sha256_hex(&bytes);
        let segment_key = civil::segment_key_string_local(boundary, 0, 300);
        assert!(matches!(
            FailingOffset.local_offset_secs(boundary),
            Err(observer_model::LocalOffsetError::Lookup)
        ));
        let client = FakeClient::new(
            vec![accepted_ingest(1)],
            vec![confirmed_segments(
                segment_key,
                file_name,
                sha,
                bytes.len() as u64,
            )],
        );
        let coordinator = coordinator_with_client_and_offset(
            client.clone(),
            Box::new(OneSegmentStore::new(boundary, file_name, bytes)),
            sync.clone(),
            Arc::new(FailOnceOffset::new(0)),
        );

        let first = coordinator.tick().await;
        assert!(matches!(first, Err(TransportError::LocalOffset)));
        let first_snapshot = sync.lock().unwrap().clone();
        assert_eq!(
            first_snapshot.upload.last_error_reason.as_deref(),
            Some("local_offset")
        );
        assert_eq!(first_snapshot.upload.failed_segments, 1);
        assert_eq!(client.ingests.lock().unwrap().len(), 1);

        let second = coordinator.tick().await.unwrap();
        assert_eq!(second, 1);
        let second_snapshot = sync.lock().unwrap().clone();
        assert_eq!(second_snapshot.upload.uploaded_segments, 1);
        assert_eq!(second_snapshot.upload.last_error_reason, None);
    }

    #[tokio::test]
    async fn duplicate_reconciles_against_existing_segment_key() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let boundary = 1_700_000_100;
        let file_name = "display_1_screen.mp4";
        let bytes = b"segment bytes".to_vec();
        let sha = ca::sha256_hex(&bytes);
        let local_key = civil::segment_key_string_local(boundary, 0, 300);
        let server_key = "111111_300";
        assert_ne!(local_key, server_key);
        let client = FakeClient::new(
            vec![scripted_ingest("duplicate", None, Some(server_key), 1)],
            vec![confirmed_segments(
                server_key.to_string(),
                file_name,
                sha,
                bytes.len() as u64,
            )],
        );
        let store = OneSegmentStore::new(boundary, file_name, bytes);
        let removed = store.removed_handle();
        let coordinator = coordinator_with_client(client, Box::new(store), sync.clone());

        let confirmed = coordinator.tick().await.unwrap();

        assert_eq!(confirmed, 1);
        assert!(*removed.lock().unwrap());
        assert_eq!(sync.lock().unwrap().upload.uploaded_segments, 1);
    }

    #[tokio::test]
    async fn collision_reconciles_against_remapped_segment_key() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let boundary = 1_700_000_100;
        let file_name = "display_1_screen.mp4";
        let bytes = b"segment bytes".to_vec();
        let sha = ca::sha256_hex(&bytes);
        let remapped_key = "222222_300";
        let client = FakeClient::new(
            vec![scripted_ingest("collision", Some(remapped_key), None, 1)],
            vec![confirmed_segments(
                remapped_key.to_string(),
                file_name,
                sha,
                bytes.len() as u64,
            )],
        );
        let store = OneSegmentStore::new(boundary, file_name, bytes);
        let removed = store.removed_handle();
        let coordinator = coordinator_with_client(client, Box::new(store), sync.clone());

        let confirmed = coordinator.tick().await.unwrap();

        assert_eq!(confirmed, 1);
        assert!(*removed.lock().unwrap());
        assert_eq!(sync.lock().unwrap().upload.uploaded_segments, 1);
    }

    #[tokio::test]
    async fn missing_status_does_not_confirm_or_delete_until_held() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let boundary = 1_700_000_100;
        let file_name = "display_1_screen.mp4";
        let bytes = b"segment bytes".to_vec();
        let sha = ca::sha256_hex(&bytes);
        let segment_key = civil::segment_key_string_local(boundary, 0, 300);
        let client = FakeClient::new(
            vec![accepted_ingest(1), accepted_ingest(2)],
            vec![
                listed_segments(
                    segment_key.clone(),
                    file_name,
                    sha.clone(),
                    bytes.len() as u64,
                    Some("missing"),
                ),
                confirmed_segments(segment_key, file_name, sha, bytes.len() as u64),
            ],
        );
        let store = OneSegmentStore::new(boundary, file_name, bytes);
        let removed = store.removed_handle();
        let coordinator = coordinator_with_client(client, Box::new(store), sync.clone());

        let first = coordinator.tick().await.unwrap();
        assert_eq!(first, 0);
        assert!(!*removed.lock().unwrap());
        assert_eq!(sync.lock().unwrap().upload.uploaded_segments, 0);

        let second = coordinator.tick().await.unwrap();
        assert_eq!(second, 1);
        assert!(*removed.lock().unwrap());
    }

    fn observer_contract_authority_record(
        document: &str,
        field: &str,
        id: &str,
    ) -> serde_json::Value {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../contracts/observer-client/bundle")
            .join(document);
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root).expect("read authority bundle"))
                .expect("parse authority bundle");
        value[field]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["id"] == id)
            .unwrap_or_else(|| panic!("missing authority record {id}"))
            .clone()
    }

    #[tokio::test]
    async fn observer_contract_authority_coordinator_uses_pinned_stored_key_precedence() {
        for vector_id in [
            "observer.ingestUpload.status.collision",
            "observer.ingestUpload.status.duplicate",
            "observer.ingestUpload.status.ok",
        ] {
            assert!(xtask::observer_contract::ADOPTED_VECTOR_IDS.contains(&vector_id));
            let vector = observer_contract_authority_record("vectors.json", "vectors", vector_id);
            let fixture_id = vector["fixture_id"].as_str().unwrap();
            let fixture = observer_contract_authority_record(
                "fixtures/wire-behavior.json",
                "fixtures",
                fixture_id,
            );
            let response: IngestResponse =
                serde_json::from_value(fixture["payload"].clone()).unwrap();
            assert!(response.is_accepted());
            let source = vector["decision"]["stored_key_source"].as_str().unwrap();
            let selected = match source {
                "segment" => response.segment.as_deref().unwrap(),
                "existing_segment" => response.existing_segment.as_deref().unwrap(),
                other => panic!("unsupported authority source {other}"),
            }
            .to_owned();
            let file_name = "audio.flac";
            let bytes = b"authority-coordinator".to_vec();
            let sha = ca::sha256_hex(&bytes);
            let store = OneSegmentStore::new(1_700_000_100, file_name, bytes.clone());
            let removed = store.removed_handle();
            let client = FakeClient::new(
                vec![Ok((
                    response,
                    SendMetadata {
                        path: TransportPath::Direct,
                        attempts: 1,
                    },
                ))],
                vec![confirmed_segments(
                    selected,
                    file_name,
                    sha,
                    bytes.len() as u64,
                )],
            );
            let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
            let coordinator = coordinator_with_client(client, Box::new(store), sync);

            assert_eq!(coordinator.tick().await.unwrap(), 1, "{vector_id}");
            assert!(*removed.lock().unwrap(), "{vector_id}");
        }

        let boundary = 1_700_000_100;
        let file_name = "audio.flac";
        let bytes = b"submitted-key-fallback".to_vec();
        let local_key = civil::segment_key_string_local(boundary, 0, 300);
        let sha = ca::sha256_hex(&bytes);
        let store = OneSegmentStore::new(boundary, file_name, bytes.clone());
        let removed = store.removed_handle();
        let client = FakeClient::new(
            vec![accepted_ingest(1)],
            vec![confirmed_segments(
                local_key,
                file_name,
                sha,
                bytes.len() as u64,
            )],
        );
        let coordinator = coordinator_with_client(
            client,
            Box::new(store),
            Arc::new(Mutex::new(SyncSnapshot::default())),
        );
        assert_eq!(coordinator.tick().await.unwrap(), 1);
        assert!(*removed.lock().unwrap());
    }

    #[tokio::test]
    async fn observer_contract_authority_coordinator_preserves_rejected_uploads() {
        for vector_id in [
            "observer.ingestUpload.status.conflict",
            "observer.ingestUpload.status.failed",
            "observer.ingestUpload.status_unknown_rejected",
        ] {
            let vector = observer_contract_authority_record("vectors.json", "vectors", vector_id);
            let fixture = observer_contract_authority_record(
                "fixtures/wire-behavior.json",
                "fixtures",
                vector["fixture_id"].as_str().unwrap(),
            );
            let ingest = if let Some(status) = vector["decision"]["http_status"].as_u64() {
                if status != 200 {
                    Err(TransportError::Rejected {
                        status: status as u16,
                        body: serde_json::to_string(&fixture["payload"]).unwrap(),
                    })
                } else {
                    unreachable!()
                }
            } else {
                Ok((
                    serde_json::from_value(fixture["payload"].clone()).unwrap(),
                    SendMetadata {
                        path: TransportPath::Direct,
                        attempts: 1,
                    },
                ))
            };
            let store = OneSegmentStore::new(1_700_000_100, "audio.flac", b"retained".to_vec());
            let removed = store.removed_handle();
            let client = FakeClient::new(vec![ingest], vec![]);
            let coordinator = coordinator_with_client(
                client,
                Box::new(store),
                Arc::new(Mutex::new(SyncSnapshot::default())),
            );

            assert_eq!(coordinator.tick().await.unwrap(), 0, "{vector_id}");
            assert!(!*removed.lock().unwrap(), "{vector_id}");
        }
    }
}
