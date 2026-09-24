// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The upload/sync coordinator — the macOS `UploadCoordinator`/`SyncService`
//! analog.
//!
//! On each tick it scans sealed segments, ships each to `/app/devices/ingest`,
//! and validates the returned receipt descriptors against the exact local files.
//! A tick removes the segment in the same pass once the journal confirms it,
//! and local finish removes an already-acknowledged segment before any journal request.
//! Failures leave the segment on disk and grow an exponential backoff (5s → 5m),
//! so a transient journal outage retries without losing data. Pairing/upload
//! counts and diagnostic counters are published into the shared [`SyncSnapshot`]
//! the engine folds into the health dump. Tick results also maintain the
//! diagnostics-only health beacon fields: consecutive failure code and last
//! successful sync epoch milliseconds.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use observer_model::{LocalOffset, PairingPhase, SyncSnapshot, TransportPath};
use observer_pl::civil;
use observer_pl::ingest::{
    validate_receipt, FilePart, IngestResponse, IngestStatus, LocalFile, ReceiptFault,
    SegmentsEnvelope,
};
use spl_core::ca;
use spl_transport::handshake::HandshakeStop;
use tokio::sync::watch;

use crate::ack::{AckFile, JournalIdentity, UploadAck};
use crate::client::{ClientSlot, ObserverClient, SendMetadata};
use crate::journal_version::{JournalVersionController, JournalVersionSessionToken};
use crate::post_connect::PostConnectController;
use crate::sealed::{content_type_for, SealedStore, UPLOADED_MARKER, UPLOADED_TMP_MARKER};
use crate::{cancelled, transport_error_code, TransportError, DEFAULT_UPLOAD_INTERVAL_SECS};

const MAX_BACKOFF_SECS: u64 = 300;
const QUARANTINE_AFTER_REJECTS: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeleteGate {
    Deleted,
    Partial,
    Blocked,
    Stopped,
}

fn is_attributable_rejection(err: &TransportError) -> bool {
    match err {
        TransportError::Rejected { status: 413, .. } => true,
        TransportError::Rejected { status: 400, body } => {
            serde_json::from_str::<serde_json::Value>(body)
                .ok()
                .and_then(|value| value.get("reason_code")?.as_str().map(str::to_owned))
                .is_some_and(|reason| {
                    matches!(
                        reason.as_str(),
                        "legacy_observer_field"
                            | "legacy_stream_field"
                            | "source_not_utf8"
                            | "source_too_long"
                            | "source_contains_nul"
                            | "source_contains_path_separator"
                            | "source_contains_dot"
                            | "source_invalid_character"
                            | "day_invalid"
                            | "segment_invalid"
                            | "field_missing"
                            | "field_duplicate"
                            | "envelope_invalid"
                            | "file_metadata_invalid"
                            | "file_name_mismatch"
                            | "file_name_invalid"
                            | "file_name_duplicate"
                            | "multipart_malformed"
                            | "multipart_part_too_large"
                            | "multipart_too_many_parts"
                            | "multipart_too_many_files"
                            | "multipart_too_many_headers"
                            | "multipart_filename_too_long"
                    )
                })
        }
        _ => false,
    }
}

fn is_device_scoped_refusal(err: &TransportError) -> bool {
    match err {
        TransportError::Rejected {
            status: 401 | 403 | 404 | 426,
            ..
        } => true,
        TransportError::Rejected { status: 409, body } => {
            serde_json::from_str::<serde_json::Value>(body)
                .ok()
                .and_then(|v| {
                    let err_str = v.get("error").and_then(|e| e.as_str()).unwrap_or("");
                    let reason_str = v.get("reason_code").and_then(|e| e.as_str()).unwrap_or("");
                    if err_str == "foreign_stream_binding"
                        || err_str == "pairing_identity_unavailable"
                        || reason_str == "foreign_stream_binding"
                        || reason_str == "pairing_identity_unavailable"
                    {
                        Some(true)
                    } else {
                        None
                    }
                })
                .unwrap_or(false)
        }
        _ => false,
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
type ListSegmentsFuture<'a> = Pin<
    Box<dyn Future<Output = Result<(SegmentsEnvelope, SendMetadata), TransportError>> + Send + 'a>,
>;

trait UploadClient: Send + Sync {
    fn ingest<'a>(
        &'a self,
        segment: &'a str,
        day: &'a str,
        files: Vec<FilePart>,
    ) -> IngestFuture<'a>;

    fn list_segments<'a>(&'a self, day: &'a str) -> ListSegmentsFuture<'a>;

    /// Why this pairing stopped reaching the journal, if it has.
    fn refusal_stop(&self) -> Option<HandshakeStop> {
        None
    }

    fn journal_identity(&self) -> JournalIdentity;
}

/// The pairing detail shown when the journal refused this device, or refusals
/// went on too long. The saved pairing is kept; restarting tries again.
pub const PAIRING_REFUSED_DETAIL: &str = "journal_refused";

impl UploadClient for ObserverClient {
    fn ingest<'a>(
        &'a self,
        segment: &'a str,
        day: &'a str,
        files: Vec<FilePart>,
    ) -> IngestFuture<'a> {
        Box::pin(ObserverClient::ingest(self, segment, day, files))
    }

    fn list_segments<'a>(&'a self, day: &'a str) -> ListSegmentsFuture<'a> {
        Box::pin(ObserverClient::list_segments(self, day))
    }

    fn refusal_stop(&self) -> Option<HandshakeStop> {
        ObserverClient::refusal_stop(self)
    }

    fn journal_identity(&self) -> JournalIdentity {
        ObserverClient::journal_identity(self)
    }
}

impl UploadClient for ClientSlot {
    fn ingest<'a>(
        &'a self,
        segment: &'a str,
        day: &'a str,
        files: Vec<FilePart>,
    ) -> IngestFuture<'a> {
        let client = self.load();
        Box::pin(async move { client.ingest(segment, day, files).await })
    }

    fn list_segments<'a>(&'a self, day: &'a str) -> ListSegmentsFuture<'a> {
        let client = self.load();
        let day = day.to_string();
        Box::pin(async move { client.list_segments(&day).await })
    }

    fn refusal_stop(&self) -> Option<HandshakeStop> {
        ClientSlot::refusal_stop(self)
    }

    fn journal_identity(&self) -> JournalIdentity {
        ClientSlot::journal_identity(self)
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

#[derive(Debug, Clone)]
struct SegmentBound {
    until_epoch: u64,
    last_error_signature: String,
    streak: u32,
}

#[derive(Debug, Clone)]
struct DayListingBound {
    until_epoch: u64,
}

/// Drives sealed segments to the journal and reconciles them.
pub struct UploadCoordinator {
    client: Arc<dyn UploadClient>,
    client_slot: Option<ClientSlot>,
    journal_version: Option<Arc<JournalVersionController>>,
    post_connect: Option<Arc<PostConnectController>>,
    version_generation: JournalVersionSessionToken,
    post_connect_generation: Option<crate::post_connect::PostConnectSessionToken>,
    store: Box<dyn SealedStore>,
    sync: Arc<Mutex<SyncSnapshot>>,
    period_secs: u64,
    local_offset: Arc<dyn LocalOffset>,
    quarantine_counts: Mutex<HashMap<u64, u32>>,
    base_wall_epoch: u64,
    base_instant: tokio::time::Instant,
    segment_bounds: Mutex<HashMap<(JournalIdentity, String, String), SegmentBound>>,
    delete_holds: Mutex<HashMap<u64, u64>>,
    day_bounds: Mutex<HashMap<String, DayListingBound>>,
}

/// An upload confirmed by receipt validation or day listing.
#[derive(Debug, Clone)]
pub struct ConfirmedUpload {
    pub server_segment: String,
    pub files: Vec<AckFile>,
    pub metadata: SendMetadata,
}

impl UploadCoordinator {
    pub fn new(
        client: Arc<ObserverClient>,
        store: Box<dyn SealedStore>,
        sync: Arc<Mutex<SyncSnapshot>>,
        period_secs: u64,
        local_offset: Arc<dyn LocalOffset>,
        journal_version: Arc<JournalVersionController>,
    ) -> Self {
        Self {
            client: client.clone(),
            client_slot: Some(ClientSlot::new(client)),
            version_generation: JournalVersionSessionToken(journal_version.current_token().0),
            journal_version: Some(journal_version),
            post_connect: None,
            post_connect_generation: None,
            store,
            sync,
            period_secs: period_secs.max(1),
            local_offset,
            quarantine_counts: Mutex::new(HashMap::new()),
            base_wall_epoch: now_epoch_secs(),
            base_instant: tokio::time::Instant::now(),
            segment_bounds: Mutex::new(HashMap::new()),
            delete_holds: Mutex::new(HashMap::new()),
            day_bounds: Mutex::new(HashMap::new()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_slot(
        client_slot: ClientSlot,
        store: Box<dyn SealedStore>,
        sync: Arc<Mutex<SyncSnapshot>>,
        period_secs: u64,
        local_offset: Arc<dyn LocalOffset>,
        journal_version: Arc<JournalVersionController>,
        post_connect: Option<Arc<PostConnectController>>,
        version_generation: JournalVersionSessionToken,
        post_connect_generation: Option<crate::post_connect::PostConnectSessionToken>,
    ) -> Self {
        Self {
            client: Arc::new(client_slot.clone()),
            client_slot: Some(client_slot),
            version_generation,
            journal_version: Some(journal_version),
            post_connect,
            post_connect_generation,
            store,
            sync,
            period_secs: period_secs.max(1),
            local_offset,
            quarantine_counts: Mutex::new(HashMap::new()),
            base_wall_epoch: now_epoch_secs(),
            base_instant: tokio::time::Instant::now(),
            segment_bounds: Mutex::new(HashMap::new()),
            delete_holds: Mutex::new(HashMap::new()),
            day_bounds: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    fn new_with_client(
        client: Arc<dyn UploadClient>,
        store: Box<dyn SealedStore>,
        sync: Arc<Mutex<SyncSnapshot>>,
        period_secs: u64,
        local_offset: Arc<dyn LocalOffset>,
    ) -> Self {
        Self {
            client,
            client_slot: None,
            post_connect: None,
            post_connect_generation: None,
            journal_version: None,
            version_generation: JournalVersionSessionToken(0),
            store,
            sync,
            period_secs: period_secs.max(1),
            local_offset,
            quarantine_counts: Mutex::new(HashMap::new()),
            base_wall_epoch: now_epoch_secs(),
            base_instant: tokio::time::Instant::now(),
            segment_bounds: Mutex::new(HashMap::new()),
            delete_holds: Mutex::new(HashMap::new()),
            day_bounds: Mutex::new(HashMap::new()),
        }
    }

    fn monotonic_now_epoch_secs(&self) -> u64 {
        self.base_wall_epoch
            .saturating_add(self.base_instant.elapsed().as_secs())
    }

    fn is_held(&self, index: u64, now_epoch: u64) -> bool {
        self.delete_holds
            .lock()
            .map(|h| h.get(&index).copied().unwrap_or(0) > now_epoch)
            .unwrap_or(false)
    }

    fn set_hold(&self, index: u64, until_epoch: u64) {
        if let Ok(mut h) = self.delete_holds.lock() {
            h.insert(index, until_epoch);
        }
    }

    fn on_invalid_receipt(&self) {
        if let Ok(mut snapshot) = self.sync.lock() {
            snapshot.upload.invalid_receipts = snapshot.upload.invalid_receipts.saturating_add(1);
        }
    }

    fn on_segment_removed(&self) {
        if let Ok(mut snapshot) = self.sync.lock() {
            snapshot.upload.segment_removed_segments =
                snapshot.upload.segment_removed_segments.saturating_add(1);
            snapshot.upload.uploaded_segments = snapshot.upload.uploaded_segments.saturating_add(1);
            snapshot.upload.last_error = None;
            snapshot.upload.pending_segments = snapshot.upload.pending_segments.saturating_sub(1);
        }
    }

    fn on_unknown_kept(&self) {
        if let Ok(mut snapshot) = self.sync.lock() {
            snapshot.upload.unknown_kept_segments =
                snapshot.upload.unknown_kept_segments.saturating_add(1);
        }
    }

    fn on_listing_refusal(&self) {
        if let Ok(mut snapshot) = self.sync.lock() {
            snapshot.upload.listing_refusals = snapshot.upload.listing_refusals.saturating_add(1);
        }
    }

    fn register_segment_error(
        &self,
        day: &str,
        segment_key: &str,
        error_signature: &str,
        now: u64,
    ) {
        let key = (
            self.client.journal_identity(),
            day.to_string(),
            segment_key.to_string(),
        );
        let mut bounds = self
            .segment_bounds
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let entry = bounds.entry(key).or_insert_with(|| SegmentBound {
            until_epoch: 0,
            last_error_signature: String::new(),
            streak: 0,
        });
        if entry.last_error_signature == error_signature {
            entry.streak = entry.streak.saturating_add(1);
        } else {
            entry.last_error_signature = error_signature.to_string();
            entry.streak = 1;
        }
        let duration_secs = if entry.streak >= 3 { 86400 } else { 3600 };
        entry.until_epoch = now.saturating_add(duration_secs);
    }

    fn register_segment_bound(
        &self,
        day: &str,
        segment_key: &str,
        error_signature: &str,
        now: u64,
        duration_secs: u64,
    ) {
        let key = (
            self.client.journal_identity(),
            day.to_string(),
            segment_key.to_string(),
        );
        let mut bounds = self
            .segment_bounds
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        bounds.insert(
            key,
            SegmentBound {
                until_epoch: now.saturating_add(duration_secs),
                last_error_signature: error_signature.to_string(),
                streak: 1,
            },
        );
    }

    fn register_received_not_written(&self, day: &str, segment_key: &str, now: u64) {
        self.register_segment_bound(day, segment_key, "received_not_written", now, 86400);
    }

    /// The journal reports the owner removed this segment: remove the local copy.
    /// It counts as delivered only once the gate actually removes files; a gate
    /// that cannot remove them holds the segment for an hour instead of posting
    /// it again on the next tick.
    fn finish_segment_removed(&self, index: u64, local_files: &[(String, String, u64)], now: u64) {
        let ack_files: Vec<AckFile> = local_files
            .iter()
            .map(|(name, sha, size)| AckFile {
                submitted: name.clone(),
                written: name.clone(),
                size: *size,
                sha256: sha.clone(),
                disposition: None,
                listing_status: None,
            })
            .collect();
        match self.try_gate_delete(index, &ack_files) {
            Ok(DeleteGate::Deleted | DeleteGate::Partial) => self.on_segment_removed(),
            Ok(DeleteGate::Blocked) => self.set_hold(index, now.saturating_add(3600)),
            Ok(DeleteGate::Stopped) => {}
            Err(e) => {
                self.set_hold(index, now.saturating_add(3600));
                tracing::warn!(
                    target: "pl_upload",
                    index,
                    error = %e,
                    "segment_removed try_gate_delete failed"
                );
            }
        }
    }

    fn try_gate_delete(
        &self,
        index: u64,
        expected_files: &[AckFile],
    ) -> Result<DeleteGate, std::io::Error> {
        let entries = self.store.list_entries(index)?;

        // 1. All entries must be regular files
        for entry in &entries {
            if !entry.is_file {
                return Ok(DeleteGate::Stopped);
            }
        }

        // 2. Filter out sidecars / tmp files
        let media_entries: Vec<_> = entries
            .iter()
            .filter(|e| {
                e.name != UPLOADED_MARKER
                    && e.name != observer_model::LEN_FILE_NAME
                    && e.name != UPLOADED_TMP_MARKER
            })
            .collect();

        // 3. Check matching against expected_files
        let mut matched_media_names = Vec::new();
        let mut has_uncovered_media = false;

        for entry in &media_entries {
            let bytes = self.store.read_file(index, &entry.name)?;
            let sha = ca::sha256_hex(&bytes);

            let matches = expected_files.iter().any(|exp| {
                if exp.submitted != entry.name || exp.size != entry.size_bytes || exp.sha256 != sha
                {
                    return false;
                }
                if let Some(disp) = &exp.disposition {
                    if disp != "written" && disp != "already_held" {
                        return false;
                    }
                }
                if let Some(status) = exp.listing_status {
                    if !status.is_held() {
                        return false;
                    }
                }
                true
            });

            if matches {
                matched_media_names.push(entry.name.clone());
            } else {
                has_uncovered_media = true;
            }
        }

        // 4. Re-list immediately before delete
        let recheck_entries = self.store.list_entries(index)?;
        for entry in &recheck_entries {
            if !entry.is_file {
                return Ok(DeleteGate::Stopped);
            }
        }
        let recheck_media: Vec<_> = recheck_entries
            .iter()
            .filter(|e| {
                e.name != UPLOADED_MARKER
                    && e.name != observer_model::LEN_FILE_NAME
                    && e.name != UPLOADED_TMP_MARKER
            })
            .collect();
        if recheck_media.len() != media_entries.len() {
            return Ok(DeleteGate::Stopped);
        }
        for entry in &recheck_media {
            if !media_entries
                .iter()
                .any(|e| e.name == entry.name && e.size_bytes == entry.size_bytes)
            {
                return Ok(DeleteGate::Stopped);
            }
        }

        // 5. Delete matching media files
        for name in &matched_media_names {
            match self.store.remove_entry(index, name) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Ok(DeleteGate::Blocked),
            }
        }

        // 6. Remove sidecars in order: .len, .uploaded, .uploaded.tmp.
        // `.len` is part of the segment key, so it stays while uncovered media
        // remains: that media then uploads under the same key as its siblings.
        let remove_sidecar = |name: &str| -> Result<(), DeleteGate> {
            match self.store.remove_entry(index, name) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(_) => Err(DeleteGate::Blocked),
            }
        };

        if !has_uncovered_media && remove_sidecar(observer_model::LEN_FILE_NAME).is_err() {
            return Ok(DeleteGate::Blocked);
        }
        if remove_sidecar(UPLOADED_MARKER).is_err() {
            return Ok(DeleteGate::Blocked);
        }
        if remove_sidecar(UPLOADED_TMP_MARKER).is_err() {
            return Ok(DeleteGate::Blocked);
        }

        // 7. If all media matched (including empty dir), remove dir -> Deleted.
        // If any uncovered media remains, leave dir -> Partial.
        if !has_uncovered_media {
            match self.store.remove_dir(index) {
                Ok(()) => Ok(DeleteGate::Deleted),
                Err(_) => Ok(DeleteGate::Blocked),
            }
        } else {
            Ok(DeleteGate::Partial)
        }
    }

    fn local_finish(&self, now: u64) {
        let mut indices = Vec::new();
        if let Ok(scanned) = self.store.scan() {
            for s in scanned {
                indices.push(s.index);
            }
        }
        if let Ok(confirmed) = self.store.confirmed() {
            for s in confirmed {
                indices.push(s.index);
            }
        }
        indices.sort_unstable();
        indices.dedup();

        for index in indices {
            if self.is_held(index, now) {
                continue;
            }

            let marker_bytes = match self.store.read_file(index, UPLOADED_MARKER) {
                Ok(bytes) => bytes,
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        let entries = match self.store.list_entries(index) {
                            Ok(e) => e,
                            Err(_) => continue,
                        };
                        let has_media = entries.iter().any(|e| {
                            e.name != UPLOADED_MARKER
                                && e.name != observer_model::LEN_FILE_NAME
                                && e.name != UPLOADED_TMP_MARKER
                        });
                        if !has_media {
                            // Remove sidecars and remove_dir
                            let remove_sidecar = |name: &str| -> Result<(), ()> {
                                match self.store.remove_entry(index, name) {
                                    Ok(()) => Ok(()),
                                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                                        Ok(())
                                    }
                                    Err(_) => Err(()),
                                }
                            };
                            if remove_sidecar(observer_model::LEN_FILE_NAME).is_err() {
                                self.set_hold(index, now.saturating_add(3600));
                                continue;
                            }
                            if remove_sidecar(UPLOADED_TMP_MARKER).is_err() {
                                self.set_hold(index, now.saturating_add(3600));
                                continue;
                            }
                            if self.store.remove_dir(index).is_err() {
                                self.set_hold(index, now.saturating_add(3600));
                            }
                        }
                    }
                    continue;
                }
            };

            match UploadAck::from_bytes(&marker_bytes) {
                Ok(ack) => {
                    if ack.journal_identity.instance_id
                        == self.client.journal_identity().instance_id
                    {
                        match self.try_gate_delete(index, &ack.files) {
                            Ok(DeleteGate::Blocked) => {
                                self.set_hold(index, now.saturating_add(3600));
                            }
                            Ok(_) => {}
                            Err(e) => {
                                self.set_hold(index, now.saturating_add(3600));
                                tracing::warn!(
                                    target: "pl_upload",
                                    index,
                                    error = %e,
                                    "local_finish try_gate_delete failed"
                                );
                            }
                        }
                    } else {
                        if let Err(e) = self.store.remove_entry(index, UPLOADED_MARKER) {
                            if e.kind() != std::io::ErrorKind::NotFound {
                                self.set_hold(index, now.saturating_add(3600));
                                tracing::warn!(
                                    target: "pl_upload",
                                    index,
                                    error = %e,
                                    "failed to remove foreign uploaded marker"
                                );
                            }
                        }
                    }
                }
                Err(_) => {
                    // Mtime rule
                    let marker_mtime = match self.store.modified(index, UPLOADED_MARKER) {
                        Ok(m) => m,
                        Err(_) => continue,
                    };
                    let entries = match self.store.list_entries(index) {
                        Ok(e) => e,
                        Err(_) => continue,
                    };
                    let media_entries: Vec<_> = entries
                        .iter()
                        .filter(|e| {
                            e.name != UPLOADED_MARKER
                                && e.name != observer_model::LEN_FILE_NAME
                                && e.name != UPLOADED_TMP_MARKER
                        })
                        .collect();

                    let mut media_mtimes = Vec::new();
                    let mut mtime_failed = false;
                    for entry in &media_entries {
                        match self.store.modified(index, &entry.name) {
                            Ok(m) => media_mtimes.push((entry.name.clone(), m)),
                            Err(_) => {
                                mtime_failed = true;
                                break;
                            }
                        }
                    }
                    if mtime_failed {
                        continue;
                    }

                    let mut media_delete_blocked = false;
                    for (name, mtime) in media_mtimes {
                        if mtime <= marker_mtime {
                            match self.store.remove_entry(index, &name) {
                                Ok(()) => {}
                                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                                Err(_) => {
                                    self.set_hold(index, now.saturating_add(3600));
                                    media_delete_blocked = true;
                                    break;
                                }
                            }
                        }
                    }
                    if media_delete_blocked {
                        continue;
                    }

                    let remove_sidecar = |name: &str| -> Result<(), ()> {
                        match self.store.remove_entry(index, name) {
                            Ok(()) => Ok(()),
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                            Err(_) => Err(()),
                        }
                    };

                    if remove_sidecar(observer_model::LEN_FILE_NAME).is_err() {
                        self.set_hold(index, now.saturating_add(3600));
                        continue;
                    }
                    if remove_sidecar(UPLOADED_MARKER).is_err() {
                        self.set_hold(index, now.saturating_add(3600));
                        continue;
                    }
                    if remove_sidecar(UPLOADED_TMP_MARKER).is_err() {
                        self.set_hold(index, now.saturating_add(3600));
                        continue;
                    }

                    let remaining = self.store.list_entries(index).unwrap_or_default();
                    if remaining.is_empty() && self.store.remove_dir(index).is_err() {
                        self.set_hold(index, now.saturating_add(3600));
                    }
                }
            }
        }

        if let Ok(count) = self.store.quarantined_media_dirs() {
            if let Ok(mut snap) = self.sync.lock() {
                snap.upload.quarantined_segments = count;
            }
        }
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
                self.quarantine_counts
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .remove(&index);
                if let Ok(count) = self.store.quarantined_media_dirs() {
                    if let Ok(mut snap) = self.sync.lock() {
                        snap.upload.quarantined_segments = count;
                    }
                }
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
        if let (Some(pc), Some(token)) = (&self.post_connect, self.post_connect_generation) {
            pc.shutdown(token);
        }
    }

    fn set_pending(&self, pending: u64) {
        if let Ok(mut snapshot) = self.sync.lock() {
            snapshot.upload.pending_segments = pending;
        }
    }

    /// One pass: upload + reconcile every sealed segment currently on disk.
    /// Returns the number of segments confirmed landed this pass.
    pub async fn tick(&self) -> Result<usize, TransportError> {
        let (_tx, rx) = watch::channel(false);
        self.tick_with_cancel(&rx).await
    }

    /// One pass returning confirmed uploads.
    pub async fn tick_with_witness(&self) -> Result<Vec<ConfirmedUpload>, TransportError> {
        let (_tx, rx) = watch::channel(false);
        let result = self.tick_inner(&rx).await;
        match &result {
            Ok(_) => self.note_tick_success(),
            Err(error) => self.note_tick_failure(error),
        }
        result
    }

    async fn tick_with_cancel(
        &self,
        cancel: &watch::Receiver<bool>,
    ) -> Result<usize, TransportError> {
        let result = self
            .tick_inner(cancel)
            .await
            .map(|witnesses| witnesses.len());
        match &result {
            Ok(_) => self.note_tick_success(),
            Err(error) => self.note_tick_failure(error),
        }
        result
    }

    async fn tick_inner(
        &self,
        cancel: &watch::Receiver<bool>,
    ) -> Result<Vec<ConfirmedUpload>, TransportError> {
        let now = self.monotonic_now_epoch_secs();

        // Recount quarantined media dirs first
        if let Ok(count) = self.store.quarantined_media_dirs() {
            if let Ok(mut snap) = self.sync.lock() {
                snap.upload.quarantined_segments = count;
            }
        }

        // Run local finish pass
        self.local_finish(now);

        let segments = self.store.scan()?;
        let pending_count = segments
            .iter()
            .filter(|s| !self.is_held(s.index, now))
            .count() as u64;
        self.set_pending(pending_count);
        let mut witnesses = Vec::new();

        struct UnknownSegmentFact {
            index: u64,
            segment_key: String,
            local_files: Vec<(String, String, u64)>,
        }

        let mut unknown_by_day: HashMap<String, Vec<UnknownSegmentFact>> = HashMap::new();

        'segments: for segment in segments {
            if *cancel.borrow() {
                break 'segments;
            }

            if self.is_held(segment.index, now) {
                continue 'segments;
            }

            if segment.files.is_empty() {
                if let Ok(DeleteGate::Blocked) = self.try_gate_delete(segment.index, &[]) {
                    self.set_hold(segment.index, now.saturating_add(3600));
                }
                continue 'segments;
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

            // Check if segment is currently bound
            let bound_key = (
                self.client.journal_identity(),
                day.clone(),
                segment_key.clone(),
            );
            if let Some(bound) = self.segment_bounds.lock().unwrap().get(&bound_key) {
                if bound.until_epoch > now {
                    continue 'segments;
                }
            }

            // Read the per-source files + compute their sha256 for receipt validation.
            let read_started = Instant::now();
            let mut parts = Vec::with_capacity(segment.files.len());
            let mut local_files = Vec::with_capacity(segment.files.len());
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
                local_files.push((name.clone(), ca::sha256_hex(&bytes), bytes.len() as u64));
                parts.push(FilePart {
                    filename: name.clone(),
                    content_type: content_type_for(name),
                    bytes,
                });
            }
            if parts.is_empty() {
                continue 'segments;
            }
            let bytes = parts.iter().map(|part| part.bytes.len() as u64).sum();

            let started = Instant::now();
            match self.client.ingest(&segment_key, &day, parts).await {
                Ok((response, metadata)) if response.status.is_accepted() => {
                    let duration_ms = elapsed_ms(started);
                    let local = local_files
                        .iter()
                        .map(|(name, sha256, size)| LocalFile {
                            name,
                            sha256,
                            size: *size,
                        })
                        .collect::<Vec<_>>();

                    match validate_receipt(&response, &local) {
                        Ok(receipt) => {
                            self.clear_reject(segment.index);
                            self.segment_bounds.lock().unwrap().remove(&bound_key);

                            let status_str = match response.status {
                                IngestStatus::Ok => "ok",
                                IngestStatus::Duplicate => "duplicate",
                                IngestStatus::Collision => "collision",
                                _ => "unknown",
                            };

                            let ack = UploadAck::new_upload(
                                self.client.journal_identity(),
                                &day,
                                &segment_key,
                                receipt.server_segment(),
                                status_str,
                                receipt.files(),
                            );
                            if let Err(e) = self.store.write_ack(segment.index, &ack) {
                                tracing::warn!(
                                    target: "pl_upload",
                                    segment = segment_key.as_str(),
                                    reason = "write_ack_failed",
                                    kind = ?e.kind(),
                                    "ack write failed"
                                );
                            } else {
                                match self.try_gate_delete(segment.index, &ack.files) {
                                    Ok(DeleteGate::Blocked) => {
                                        self.set_hold(segment.index, now.saturating_add(3600));
                                    }
                                    Ok(_) => {}
                                    Err(e) => {
                                        self.set_hold(segment.index, now.saturating_add(3600));
                                        tracing::warn!(
                                            target: "pl_upload",
                                            index = segment.index,
                                            error = %e,
                                            "try_gate_delete failed"
                                        );
                                    }
                                }
                            }
                            UploadEvent::new(
                                &segment_key,
                                bytes,
                                duration_ms,
                                UploadOutcome::Confirmed,
                                Some(metadata.path),
                                None,
                            )
                            .emit();
                            self.on_confirmed(
                                &segment_key,
                                receipt.server_segment(),
                                bytes,
                                duration_ms,
                                metadata.path,
                                metadata.attempts,
                            );
                            witnesses.push(ConfirmedUpload {
                                server_segment: receipt.server_segment().to_string(),
                                files: ack.files,
                                metadata,
                            });
                        }
                        Err(ReceiptFault::Absent) => {
                            // Unknown track: absent file_descriptors
                            UploadEvent::new(
                                &segment_key,
                                bytes,
                                duration_ms,
                                UploadOutcome::AcceptedUnconfirmed,
                                Some(metadata.path),
                                None,
                            )
                            .emit();
                            unknown_by_day.entry(day.clone()).or_default().push(
                                UnknownSegmentFact {
                                    index: segment.index,
                                    segment_key,
                                    local_files,
                                },
                            );
                        }
                        Err(ReceiptFault::ReceivedNotWritten { .. }) => {
                            self.register_received_not_written(&day, &segment_key, now);
                            UploadEvent::new(
                                &segment_key,
                                bytes,
                                duration_ms,
                                UploadOutcome::Failed,
                                Some(metadata.path),
                                Some("received_not_written".to_string()),
                            )
                            .emit();
                            continue 'segments;
                        }
                        Err(fault) => {
                            self.on_invalid_receipt();
                            self.register_segment_error(&day, &segment_key, "invalid_receipt", now);
                            UploadEvent::new(
                                &segment_key,
                                bytes,
                                duration_ms,
                                UploadOutcome::Failed,
                                Some(metadata.path),
                                Some("invalid_receipt".to_string()),
                            )
                            .emit();
                            self.on_error(&TransportError::Rejected {
                                status: 200,
                                body: format!("invalid receipt: {fault:?}"),
                            });
                            continue 'segments;
                        }
                    }
                }
                Ok((response, metadata)) => {
                    let duration_ms = elapsed_ms(started);
                    let reason_code =
                        response
                            .reason_code
                            .as_deref()
                            .unwrap_or(match response.status {
                                IngestStatus::Conflict => "conflict",
                                IngestStatus::Failed => "failed",
                                _ => "rejected",
                            });

                    if reason_code == "segment_removed" {
                        UploadEvent::new(
                            &segment_key,
                            bytes,
                            duration_ms,
                            UploadOutcome::Failed,
                            Some(metadata.path),
                            Some("segment_removed".to_string()),
                        )
                        .emit();
                        self.finish_segment_removed(segment.index, &local_files, now);
                        continue 'segments;
                    }

                    self.register_segment_error(&day, &segment_key, reason_code, now);
                    let error = TransportError::Rejected {
                        status: match response.status {
                            IngestStatus::Conflict => 409,
                            IngestStatus::Failed => 500,
                            _ => 400,
                        },
                        body: format!("ingest response: {:?}", response.status),
                    };
                    UploadEvent::new(
                        &segment_key,
                        bytes,
                        duration_ms,
                        UploadOutcome::Failed,
                        Some(metadata.path),
                        Some(reason_code.to_string()),
                    )
                    .emit();
                    self.on_error(&error);
                    continue 'segments;
                }
                Err(e) => {
                    if let TransportError::Rejected { status, ref body } = e {
                        let reason = serde_json::from_str::<serde_json::Value>(body)
                            .ok()
                            .and_then(|v| v.get("reason_code")?.as_str().map(str::to_string))
                            .unwrap_or_else(|| format!("http_{status}"));
                        if reason == "segment_removed" {
                            let duration_ms = elapsed_ms(started);
                            UploadEvent::new(
                                &segment_key,
                                bytes,
                                duration_ms,
                                UploadOutcome::Failed,
                                None,
                                Some("segment_removed".to_string()),
                            )
                            .emit();
                            self.finish_segment_removed(segment.index, &local_files, now);
                            continue 'segments;
                        }
                    }

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

                    if is_device_scoped_refusal(&e) {
                        return Err(e);
                    }
                    if is_attributable_rejection(&e) {
                        self.register_reject(segment.index);
                        self.register_segment_error(
                            &day,
                            &segment_key,
                            &transport_error_code(&e),
                            now,
                        );
                        continue 'segments;
                    }
                    if matches!(
                        e,
                        TransportError::Io(_)
                            | TransportError::Tls(_)
                            | TransportError::NoEndpoint
                            | TransportError::Json(_)
                    ) {
                        return Err(e);
                    }
                    if let TransportError::Rejected { status, ref body } = e {
                        let reason = serde_json::from_str::<serde_json::Value>(body)
                            .ok()
                            .and_then(|v| v.get("reason_code")?.as_str().map(str::to_string))
                            .unwrap_or_else(|| format!("http_{status}"));
                        self.register_segment_error(&day, &segment_key, &reason, now);
                        continue 'segments;
                    }
                    return Err(e);
                }
            }
        }

        // Listing pass: after pending segment POSTs
        let mut candidate_days: Vec<String> = unknown_by_day.keys().cloned().collect();
        candidate_days.sort();

        let eligible_day = candidate_days.into_iter().find(|d| {
            let bounds = self.day_bounds.lock().unwrap();
            bounds.get(d).is_none_or(|b| b.until_epoch <= now)
        });

        if let Some(day) = eligible_day {
            match self.client.list_segments(&day).await {
                Ok((envelope, _metadata)) => {
                    self.day_bounds.lock().unwrap().insert(
                        day.clone(),
                        DayListingBound {
                            until_epoch: now.saturating_add(3600),
                        },
                    );

                    if let Some(unknown_list) = unknown_by_day.get(&day) {
                        for unk in unknown_list {
                            let matched_item = envelope.items.iter().find(|item| {
                                if item.key != unk.segment_key {
                                    return false;
                                }
                                if item.files.len() != unk.local_files.len() {
                                    return false;
                                }
                                for (name, sha, size) in &unk.local_files {
                                    let Some(sf) = item.files.iter().find(|f| &f.name == name)
                                    else {
                                        return false;
                                    };
                                    if sf.size != *size || &sf.sha256 != sha || !sf.status.is_held()
                                    {
                                        return false;
                                    }
                                }
                                true
                            });

                            if let Some(item) = matched_item {
                                let ack_files: Vec<AckFile> = unk
                                    .local_files
                                    .iter()
                                    .map(|(name, sha, size)| {
                                        let sf =
                                            item.files.iter().find(|f| &f.name == name).unwrap();
                                        AckFile {
                                            submitted: name.clone(),
                                            written: sf.name.clone(),
                                            size: *size,
                                            sha256: sha.clone(),
                                            disposition: None,
                                            listing_status: Some(sf.status),
                                        }
                                    })
                                    .collect();

                                let ack = UploadAck::new_listing(
                                    self.client.journal_identity(),
                                    &day,
                                    &unk.segment_key,
                                    &item.key,
                                    ack_files,
                                );
                                if let Err(e) = self.store.write_ack(unk.index, &ack) {
                                    tracing::warn!(
                                        target: "pl_upload",
                                        index = unk.index,
                                        error = %e,
                                        "listing write_ack failed"
                                    );
                                } else {
                                    match self.try_gate_delete(unk.index, &ack.files) {
                                        Ok(DeleteGate::Blocked) => {
                                            self.set_hold(unk.index, now.saturating_add(3600));
                                        }
                                        Ok(_) => {}
                                        Err(e) => {
                                            self.set_hold(unk.index, now.saturating_add(3600));
                                            tracing::warn!(
                                                target: "pl_upload",
                                                index = unk.index,
                                                error = %e,
                                                "listing try_gate_delete failed"
                                            );
                                        }
                                    }
                                }
                            } else {
                                self.on_unknown_kept();
                                self.register_segment_bound(
                                    &day,
                                    &unk.segment_key,
                                    "unknown_kept",
                                    now,
                                    86400,
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    self.on_error(&e);
                    if is_device_scoped_refusal(&e) {
                        return Err(e);
                    }
                    if matches!(
                        e,
                        TransportError::Io(_)
                            | TransportError::Tls(_)
                            | TransportError::NoEndpoint
                            | TransportError::Json(_)
                    ) {
                        return Err(e);
                    }
                    if let TransportError::Rejected { .. } = e {
                        self.on_listing_refusal();
                        self.day_bounds.lock().unwrap().insert(
                            day.clone(),
                            DayListingBound {
                                until_epoch: now.saturating_add(3600),
                            },
                        );
                    } else {
                        return Err(e);
                    }
                }
            }
        }

        self.set_pending(
            self.store
                .scan()
                .map(|s| s.iter().filter(|seg| !self.is_held(seg.index, now)).count() as u64)
                .unwrap_or(0),
        );
        Ok(witnesses)
    }

    fn on_confirmed(
        &self,
        segment_key: &str,
        server_key: &str,
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
            snapshot.upload.last_uploaded_server_segment = Some(server_key.to_string());
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

    fn note_tick_success(&self) {
        let is_recovery = if let Ok(mut snapshot) = self.sync.lock() {
            let was_failing = snapshot.upload.recent_error_count > 0;
            snapshot.upload.record_success(now_epoch_millis());
            if let Some(slot) = &self.client_slot {
                let client = slot.load();
                crate::unknown_journals::publish_unknown_journals(
                    &mut snapshot,
                    client.transport_client(),
                    &client.credential().instance_id,
                );
            }
            was_failing
        } else {
            false
        };
        let _ = is_recovery;
    }

    fn note_tick_failure(&self, err: &TransportError) {
        let stopped = self.client.refusal_stop().is_some();
        let was_healthy = if let Ok(mut snapshot) = self.sync.lock() {
            let healthy = snapshot.upload.recent_error_count == 0;
            snapshot.upload.record_failure(&transport_error_code(err));
            if stopped {
                snapshot.pairing.phase = PairingPhase::Failed;
                snapshot.pairing.detail = Some(PAIRING_REFUSED_DETAIL.to_string());
            }
            if let Some(slot) = &self.client_slot {
                let client = slot.load();
                crate::unknown_journals::publish_unknown_journals(
                    &mut snapshot,
                    client.transport_client(),
                    &client.credential().instance_id,
                );
            }
            healthy
        } else {
            false
        };
        if was_healthy {
            if let Some(jv) = &self.journal_version {
                jv.mark_session_disconnected(self.version_generation, &self.sync);
            }
            if let (Some(pc), Some(token)) = (&self.post_connect, self.post_connect_generation) {
                pc.mark_session_disconnected(token);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashSet, VecDeque};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use observer_model::RECENT_ERROR_COUNT_MAX;
    use observer_pl::ingest::{
        FileDescriptor, FileDescriptors, SegmentFile, SegmentFileStatus, SegmentItem,
    };
    use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};

    use crate::credential::{Credential, EndpointAddr};
    use crate::sealed::{DirEntryFact, SealedSegment};

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

        fn list_entries(&self, _index: u64) -> std::io::Result<Vec<DirEntryFact>> {
            Ok(Vec::new())
        }

        fn remove_entry(&self, _index: u64, _name: &str) -> std::io::Result<()> {
            Ok(())
        }

        fn remove_dir(&self, _index: u64) -> std::io::Result<()> {
            Ok(())
        }

        fn read_ack(&self, _index: u64) -> std::io::Result<Option<UploadAck>> {
            Ok(None)
        }

        fn write_ack(&self, _index: u64, _ack: &UploadAck) -> std::io::Result<()> {
            Ok(())
        }

        fn modified(&self, _index: u64, _name: &str) -> std::io::Result<SystemTime> {
            Ok(SystemTime::UNIX_EPOCH)
        }

        fn quarantined_media_dirs(&self) -> std::io::Result<u64> {
            Ok(0)
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

        fn list_entries(&self, _index: u64) -> std::io::Result<Vec<DirEntryFact>> {
            Err(std::io::Error::other("C:\\Users\\me\\seg.mp4"))
        }

        fn remove_entry(&self, _index: u64, _name: &str) -> std::io::Result<()> {
            Err(std::io::Error::other("C:\\Users\\me\\seg.mp4"))
        }

        fn remove_dir(&self, _index: u64) -> std::io::Result<()> {
            Err(std::io::Error::other("C:\\Users\\me\\seg.mp4"))
        }

        fn read_ack(&self, _index: u64) -> std::io::Result<Option<UploadAck>> {
            Err(std::io::Error::other("C:\\Users\\me\\seg.mp4"))
        }

        fn write_ack(&self, _index: u64, _ack: &UploadAck) -> std::io::Result<()> {
            Err(std::io::Error::other("C:\\Users\\me\\seg.mp4"))
        }

        fn modified(&self, _index: u64, _name: &str) -> std::io::Result<SystemTime> {
            Err(std::io::Error::other("C:\\Users\\me\\seg.mp4"))
        }

        fn quarantined_media_dirs(&self) -> std::io::Result<u64> {
            Err(std::io::Error::other("C:\\Users\\me\\seg.mp4"))
        }
    }

    struct OneSegmentStore {
        removed: Arc<Mutex<bool>>,
        segment: SealedSegment,
        #[allow(dead_code)]
        file_name: String,
        #[allow(dead_code)]
        bytes: Vec<u8>,
        ack: Arc<Mutex<Option<UploadAck>>>,
        files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    }

    impl OneSegmentStore {
        fn new(boundary_epoch_secs: u64, file_name: &str, bytes: Vec<u8>) -> Self {
            let mut files = HashMap::new();
            files.insert(file_name.to_string(), bytes.clone());
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
                ack: Arc::new(Mutex::new(None)),
                files: Arc::new(Mutex::new(files)),
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
            let files = self.files.lock().unwrap();
            files
                .get(name)
                .cloned()
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "file not found"))
        }

        fn remove(&self, _index: u64) -> std::io::Result<()> {
            *self.removed.lock().unwrap() = true;
            *self.ack.lock().unwrap() = None;
            Ok(())
        }

        fn quarantine(&self, _index: u64) -> std::io::Result<()> {
            Ok(())
        }

        fn mark_confirmed(&self, _index: u64) -> std::io::Result<()> {
            *self.removed.lock().unwrap() = true;
            *self.ack.lock().unwrap() = None;
            Ok(())
        }

        fn confirmed(&self) -> std::io::Result<Vec<SealedSegment>> {
            if *self.removed.lock().unwrap() {
                return Ok(Vec::new());
            }
            if self.ack.lock().unwrap().is_some() {
                Ok(vec![self.segment.clone()])
            } else {
                Ok(Vec::new())
            }
        }

        fn list_entries(&self, _index: u64) -> std::io::Result<Vec<DirEntryFact>> {
            if *self.removed.lock().unwrap() {
                return Ok(Vec::new());
            }
            let files = self.files.lock().unwrap();
            let mut facts = Vec::new();
            for (name, data) in files.iter() {
                facts.push(DirEntryFact {
                    name: name.clone(),
                    is_file: true,
                    size_bytes: data.len() as u64,
                });
            }
            if self.ack.lock().unwrap().is_some() {
                facts.push(DirEntryFact {
                    name: UPLOADED_MARKER.to_string(),
                    is_file: true,
                    size_bytes: 100,
                });
            }
            Ok(facts)
        }

        fn remove_entry(&self, _index: u64, name: &str) -> std::io::Result<()> {
            if name == UPLOADED_MARKER {
                *self.ack.lock().unwrap() = None;
            }
            let mut files = self.files.lock().unwrap();
            files.remove(name);
            Ok(())
        }

        fn remove_dir(&self, _index: u64) -> std::io::Result<()> {
            *self.removed.lock().unwrap() = true;
            *self.ack.lock().unwrap() = None;
            Ok(())
        }

        fn read_ack(&self, _index: u64) -> std::io::Result<Option<UploadAck>> {
            if *self.removed.lock().unwrap() {
                return Ok(None);
            }
            Ok(self.ack.lock().unwrap().clone())
        }

        fn write_ack(&self, _index: u64, ack: &UploadAck) -> std::io::Result<()> {
            *self.ack.lock().unwrap() = Some(ack.clone());
            Ok(())
        }

        fn modified(&self, _index: u64, _name: &str) -> std::io::Result<SystemTime> {
            Ok(SystemTime::UNIX_EPOCH)
        }

        fn quarantined_media_dirs(&self) -> std::io::Result<u64> {
            Ok(0)
        }
    }

    #[derive(Clone)]
    struct MultiSegmentStore {
        state: Arc<Mutex<MultiSegmentState>>,
    }

    struct MultiSegmentState {
        segments: Vec<SealedSegment>,
        bytes: HashMap<u64, HashMap<String, Vec<u8>>>,
        read_errors: HashMap<u64, String>,
        removed: HashSet<u64>,
        confirmed: HashSet<u64>,
        quarantined: HashSet<u64>,
        remove_fails_once: HashSet<u64>,
        mark_confirmed_fails_once: HashSet<u64>,
        acks: HashMap<u64, UploadAck>,
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
                let mut map = HashMap::new();
                map.insert(file_name.to_string(), data);
                bytes.insert(index, map);
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
                    acks: HashMap::new(),
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

        fn insert_segment(
            &self,
            index: u64,
            boundary_epoch_secs: u64,
            file_name: &str,
            data: Vec<u8>,
        ) {
            let mut state = self.state.lock().unwrap();
            state.segments.push(SealedSegment {
                index,
                boundary_epoch_secs,
                len_secs: None,
                files: vec![file_name.to_string()],
            });
            state.segments.sort_by_key(|s| s.index);
            state
                .bytes
                .entry(index)
                .or_default()
                .insert(file_name.to_string(), data);
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
                        && !state.acks.contains_key(&segment.index)
                        && !state.quarantined.contains(&segment.index)
                })
                .cloned()
                .collect())
        }

        fn read_file(&self, index: u64, name: &str) -> std::io::Result<Vec<u8>> {
            let state = self.state.lock().unwrap();
            if let Some(message) = state.read_errors.get(&index) {
                return Err(std::io::Error::other(message.clone()));
            }
            state
                .bytes
                .get(&index)
                .and_then(|map| map.get(name))
                .cloned()
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "missing bytes"))
        }

        fn remove(&self, index: u64) -> std::io::Result<()> {
            let mut state = self.state.lock().unwrap();
            if state.remove_fails_once.remove(&index) {
                return Err(std::io::Error::other("C:\\Users\\me\\seg.mp4"));
            }
            state.removed.insert(index);
            state.acks.remove(&index);
            state.confirmed.remove(&index);
            state.bytes.remove(&index);
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
                .filter(|segment| {
                    !state.removed.contains(&segment.index)
                        && (state.confirmed.contains(&segment.index)
                            || state.acks.contains_key(&segment.index))
                })
                .cloned()
                .collect())
        }

        fn list_entries(&self, index: u64) -> std::io::Result<Vec<DirEntryFact>> {
            let state = self.state.lock().unwrap();
            if state.removed.contains(&index) {
                return Ok(Vec::new());
            }
            let mut facts = Vec::new();
            let mut has_uploaded = false;
            if let Some(map) = state.bytes.get(&index) {
                for (name, data) in map {
                    if name == UPLOADED_MARKER {
                        has_uploaded = true;
                    }
                    facts.push(DirEntryFact {
                        name: name.clone(),
                        is_file: true,
                        size_bytes: data.len() as u64,
                    });
                }
            }
            if !has_uploaded && state.acks.contains_key(&index) {
                facts.push(DirEntryFact {
                    name: UPLOADED_MARKER.to_string(),
                    is_file: true,
                    size_bytes: 100,
                });
            }
            Ok(facts)
        }

        fn remove_entry(&self, index: u64, name: &str) -> std::io::Result<()> {
            let mut state = self.state.lock().unwrap();
            if state.remove_fails_once.remove(&index) {
                return Err(std::io::Error::other("C:\\Users\\me\\seg.mp4"));
            }
            if name == UPLOADED_MARKER {
                state.acks.remove(&index);
            }
            if let Some(map) = state.bytes.get_mut(&index) {
                map.remove(name);
            }
            Ok(())
        }

        fn remove_dir(&self, index: u64) -> std::io::Result<()> {
            let mut state = self.state.lock().unwrap();
            if state.remove_fails_once.remove(&index) {
                return Err(std::io::Error::other("C:\\Users\\me\\seg.mp4"));
            }
            state.removed.insert(index);
            state.acks.remove(&index);
            state.confirmed.remove(&index);
            state.bytes.remove(&index);
            Ok(())
        }

        fn read_ack(&self, index: u64) -> std::io::Result<Option<UploadAck>> {
            let state = self.state.lock().unwrap();
            if state.removed.contains(&index) {
                return Ok(None);
            }
            Ok(state.acks.get(&index).cloned())
        }

        fn write_ack(&self, index: u64, ack: &UploadAck) -> std::io::Result<()> {
            let mut state = self.state.lock().unwrap();
            if state.mark_confirmed_fails_once.remove(&index) {
                return Err(std::io::Error::other("C:\\Users\\me\\seg.mp4"));
            }
            let bytes = ack
                .to_bytes()
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            state.acks.insert(index, ack.clone());
            state
                .bytes
                .entry(index)
                .or_default()
                .insert(UPLOADED_MARKER.to_string(), bytes);
            Ok(())
        }

        fn modified(&self, index: u64, name: &str) -> std::io::Result<SystemTime> {
            let state = self.state.lock().unwrap();
            if let Some(message) = state.read_errors.get(&index) {
                return Err(std::io::Error::other(message.clone()));
            }
            if state
                .bytes
                .get(&index)
                .and_then(|map| map.get(name))
                .is_some()
                || (name == UPLOADED_MARKER && state.acks.contains_key(&index))
            {
                Ok(SystemTime::UNIX_EPOCH)
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "file not found",
                ))
            }
        }

        fn quarantined_media_dirs(&self) -> std::io::Result<u64> {
            Ok(self.state.lock().unwrap().quarantined.len() as u64)
        }
    }

    struct FakeClient {
        ingests: Mutex<VecDeque<Result<(IngestResponse, SendMetadata), TransportError>>>,
        lists: Mutex<VecDeque<Result<(SegmentsEnvelope, SendMetadata), TransportError>>>,
        submitted_day: Mutex<Option<String>>,
        stop: Mutex<Option<HandshakeStop>>,
        /// Every POST as (segment key, part file names).
        posts: Mutex<Vec<(String, Vec<String>)>>,
    }

    impl FakeClient {
        fn new(
            ingests: Vec<Result<(IngestResponse, SendMetadata), TransportError>>,
            lists: Vec<Result<(SegmentsEnvelope, SendMetadata), TransportError>>,
        ) -> Arc<Self> {
            Arc::new(Self {
                ingests: Mutex::new(VecDeque::from(ingests)),
                lists: Mutex::new(VecDeque::from(lists)),
                submitted_day: Mutex::new(None),
                stop: Mutex::new(None),
                posts: Mutex::new(Vec::new()),
            })
        }
    }

    impl UploadClient for FakeClient {
        fn journal_identity(&self) -> JournalIdentity {
            JournalIdentity {
                instance_id: "test".to_string(),
                ca_fp_prefix: "0000000000000000".to_string(),
                client_cert_sha256:
                    "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
            }
        }

        fn ingest<'a>(
            &'a self,
            segment: &'a str,
            day: &'a str,
            files: Vec<FilePart>,
        ) -> IngestFuture<'a> {
            *self.submitted_day.lock().unwrap() = Some(day.to_owned());
            self.posts.lock().unwrap().push((
                segment.to_owned(),
                files.iter().map(|part| part.filename.clone()).collect(),
            ));
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

        fn refusal_stop(&self) -> Option<HandshakeStop> {
            *self.stop.lock().unwrap()
        }
    }

    /// A journal that refuses every request and counts them, for proving a
    /// tick reached no journal at all.
    struct RefusingClient {
        calls: AtomicUsize,
    }

    impl RefusingClient {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
            })
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl UploadClient for RefusingClient {
        fn journal_identity(&self) -> JournalIdentity {
            JournalIdentity {
                instance_id: "test".to_string(),
                ca_fp_prefix: "0000000000000000".to_string(),
                client_cert_sha256:
                    "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
            }
        }

        fn ingest<'a>(
            &'a self,
            _segment: &'a str,
            _day: &'a str,
            _files: Vec<FilePart>,
        ) -> IngestFuture<'a> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Err(TransportError::Rejected {
                    status: 403,
                    body: "refused".to_string(),
                })
            })
        }

        fn list_segments<'a>(&'a self, _day: &'a str) -> ListSegmentsFuture<'a> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Err(TransportError::Rejected {
                    status: 403,
                    body: "refused".to_string(),
                })
            })
        }
    }

    /// A segments root under the system temp dir, removed on drop.
    struct TestRoot(std::path::PathBuf);

    impl TestRoot {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "sw-test-{tag}-{}-{}",
                std::process::id(),
                now_epoch_millis()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A real sealed store with chosen operations made to fail.
    struct FaultStore {
        inner: crate::sealed::LocalSealedStore,
        fail_list_entries: bool,
        fail_remove_dir: bool,
        fail_remove_marker: bool,
    }

    impl FaultStore {
        fn new(root: &std::path::Path) -> Self {
            Self {
                inner: crate::sealed::LocalSealedStore::new(root, 300),
                fail_list_entries: false,
                fail_remove_dir: false,
                fail_remove_marker: false,
            }
        }
    }

    impl SealedStore for FaultStore {
        fn scan(&self) -> std::io::Result<Vec<SealedSegment>> {
            self.inner.scan()
        }

        fn read_file(&self, index: u64, name: &str) -> std::io::Result<Vec<u8>> {
            self.inner.read_file(index, name)
        }

        fn remove(&self, index: u64) -> std::io::Result<()> {
            self.inner.remove(index)
        }

        fn quarantine(&self, index: u64) -> std::io::Result<()> {
            self.inner.quarantine(index)
        }

        fn mark_confirmed(&self, index: u64) -> std::io::Result<()> {
            self.inner.mark_confirmed(index)
        }

        fn confirmed(&self) -> std::io::Result<Vec<SealedSegment>> {
            self.inner.confirmed()
        }

        fn list_entries(&self, index: u64) -> std::io::Result<Vec<DirEntryFact>> {
            if self.fail_list_entries {
                return Err(std::io::Error::other("list failed"));
            }
            self.inner.list_entries(index)
        }

        fn remove_entry(&self, index: u64, name: &str) -> std::io::Result<()> {
            if self.fail_remove_marker && name == UPLOADED_MARKER {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "marker locked",
                ));
            }
            self.inner.remove_entry(index, name)
        }

        fn remove_dir(&self, index: u64) -> std::io::Result<()> {
            if self.fail_remove_dir {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "directory locked",
                ));
            }
            self.inner.remove_dir(index)
        }

        fn read_ack(&self, index: u64) -> std::io::Result<Option<UploadAck>> {
            self.inner.read_ack(index)
        }

        fn write_ack(&self, index: u64, ack: &UploadAck) -> std::io::Result<()> {
            self.inner.write_ack(index, ack)
        }

        fn modified(&self, index: u64, name: &str) -> std::io::Result<SystemTime> {
            self.inner.modified(index, name)
        }

        fn quarantined_media_dirs(&self) -> std::io::Result<u64> {
            self.inner.quarantined_media_dirs()
        }
    }

    /// Index whose boundary is 1_700_000_100 at the 300 s period.
    const TEST_INDEX: u64 = 5_666_667;

    fn test_identity(instance_id: &str, client_cert_sha256: &str) -> JournalIdentity {
        JournalIdentity {
            instance_id: instance_id.to_string(),
            ca_fp_prefix: "0000000000000000".to_string(),
            client_cert_sha256: client_cert_sha256.to_string(),
        }
    }

    fn written_descriptor(name: &str, bytes: &[u8]) -> FileDescriptor {
        FileDescriptor {
            submitted: name.to_string(),
            written: name.to_string(),
            size: bytes.len() as u64,
            sha256: ca::sha256_hex(bytes),
            disposition: "written".to_string(),
        }
    }

    fn write_marker(dir: &std::path::Path, identity: JournalIdentity, files: &[FileDescriptor]) {
        let ack = UploadAck::new_upload(
            identity,
            "20231114",
            "120000_300",
            "120000_300",
            "ok",
            files,
        );
        std::fs::write(dir.join(UPLOADED_MARKER), ack.to_bytes().unwrap()).unwrap();
    }

    fn set_mtime(path: &std::path::Path, secs: u64) {
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap();
    }

    struct CancelAfterFirstListClient {
        ingests: Mutex<VecDeque<Result<(IngestResponse, SendMetadata), TransportError>>>,
        lists: Mutex<VecDeque<Result<(SegmentsEnvelope, SendMetadata), TransportError>>>,
        cancel: watch::Sender<bool>,
        ingest_count: Arc<AtomicUsize>,
        list_count: AtomicUsize,
        submitted_day: Mutex<Option<String>>,
    }

    impl CancelAfterFirstListClient {
        fn new(
            ingests: Vec<Result<(IngestResponse, SendMetadata), TransportError>>,
            lists: Vec<Result<(SegmentsEnvelope, SendMetadata), TransportError>>,
            cancel: watch::Sender<bool>,
        ) -> Arc<Self> {
            Arc::new(Self {
                ingests: Mutex::new(VecDeque::from(ingests)),
                lists: Mutex::new(VecDeque::from(lists)),
                cancel,
                ingest_count: Arc::new(AtomicUsize::new(0)),
                list_count: AtomicUsize::new(0),
                submitted_day: Mutex::new(None),
            })
        }

        fn ingest_count(&self) -> usize {
            self.ingest_count.load(Ordering::SeqCst)
        }
    }

    impl UploadClient for CancelAfterFirstListClient {
        fn journal_identity(&self) -> JournalIdentity {
            JournalIdentity {
                instance_id: "test".to_string(),
                ca_fp_prefix: "0000000000000000".to_string(),
                client_cert_sha256:
                    "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
            }
        }

        fn ingest<'a>(
            &'a self,
            _segment: &'a str,
            day: &'a str,
            _files: Vec<FilePart>,
        ) -> IngestFuture<'a> {
            *self.submitted_day.lock().unwrap() = Some(day.to_owned());
            let result = self
                .ingests
                .lock()
                .unwrap()
                .pop_front()
                .expect("scripted ingest result");
            let ingest_count = self.ingest_count.clone();
            let cancel = self.cancel.clone();
            Box::pin(async move {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let count = ingest_count.fetch_add(1, Ordering::SeqCst);
                if count == 0 {
                    let _ = cancel.send(true);
                }
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

    fn test_metadata() -> SendMetadata {
        SendMetadata {
            path: TransportPath::Direct,
            attempts: 1,
        }
    }

    fn dummy_credential() -> Credential {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let params = CertificateParams::new(vec!["spl.local".to_string()]).unwrap();
        let cert = params.self_signed(&key).unwrap();
        Credential {
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
        }
    }

    fn dummy_client() -> Arc<ObserverClient> {
        Arc::new(ObserverClient::new(dummy_credential()).unwrap())
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
            300,
            local_offset,
            Arc::new(crate::journal_version::JournalVersionController::new(
                std::env::temp_dir().join(format!("test-jv-{}.json", std::process::id())),
            )),
        )
    }

    fn coordinator_with_client(
        client: Arc<dyn UploadClient>,
        store: Box<dyn SealedStore>,
        sync: Arc<Mutex<SyncSnapshot>>,
    ) -> UploadCoordinator {
        coordinator_with_client_and_offset(client, store, sync, Arc::new(FixedOffset(0)))
    }

    fn coordinator_with_client_and_offset(
        client: Arc<dyn UploadClient>,
        store: Box<dyn SealedStore>,
        sync: Arc<Mutex<SyncSnapshot>>,
        local_offset: Arc<dyn LocalOffset>,
    ) -> UploadCoordinator {
        UploadCoordinator::new_with_client(client, store, sync, 300, local_offset)
    }

    fn coordinator_with_client_and_jv(
        client: Arc<dyn UploadClient>,
        store: Box<dyn SealedStore>,
        sync: Arc<Mutex<SyncSnapshot>>,
        jv: Arc<JournalVersionController>,
    ) -> UploadCoordinator {
        UploadCoordinator {
            client,
            client_slot: None,
            post_connect: None,
            post_connect_generation: None,
            version_generation: JournalVersionSessionToken(jv.current_token().0),
            journal_version: Some(jv),
            store,
            sync,
            period_secs: 300,
            local_offset: Arc::new(FixedOffset(0)),
            quarantine_counts: Mutex::new(HashMap::new()),
            base_wall_epoch: now_epoch_secs(),
            base_instant: tokio::time::Instant::now(),
            segment_bounds: Mutex::new(HashMap::new()),
            delete_holds: Mutex::new(HashMap::new()),
            day_bounds: Mutex::new(HashMap::new()),
        }
    }

    fn accepted_ingest(
        segment_key: &str,
        file_name: &str,
        bytes: &[u8],
        attempts: u32,
    ) -> Result<(IngestResponse, SendMetadata), TransportError> {
        scripted_ingest(
            "ok",
            Some(segment_key),
            None,
            vec![FileDescriptor {
                submitted: file_name.to_string(),
                written: file_name.to_string(),
                size: bytes.len() as u64,
                sha256: ca::sha256_hex(bytes),
                disposition: "written".to_string(),
            }],
            attempts,
        )
    }

    fn accepted_unconfirmed_ingest(
        attempts: u32,
    ) -> Result<(IngestResponse, SendMetadata), TransportError> {
        Ok((
            IngestResponse {
                status: IngestStatus::Ok,
                segment: Some("120000_300".to_string()),
                existing_segment: None,
                reason_code: None,
                file_descriptors: FileDescriptors::Absent,
            },
            SendMetadata {
                path: TransportPath::Direct,
                attempts,
            },
        ))
    }

    fn scripted_ingest(
        status: &str,
        segment: Option<&str>,
        existing_segment: Option<&str>,
        descriptors: Vec<FileDescriptor>,
        attempts: u32,
    ) -> Result<(IngestResponse, SendMetadata), TransportError> {
        Ok((
            IngestResponse {
                status: match status {
                    "ok" => IngestStatus::Ok,
                    "duplicate" => IngestStatus::Duplicate,
                    "collision" => IngestStatus::Collision,
                    "conflict" => IngestStatus::Conflict,
                    "failed" => IngestStatus::Failed,
                    other => panic!("unsupported ingest status {other}"),
                },
                segment: segment.map(ToOwned::to_owned),
                existing_segment: existing_segment.map(ToOwned::to_owned),
                reason_code: None,
                file_descriptors: FileDescriptors::Decoded(descriptors),
            },
            SendMetadata {
                path: TransportPath::Direct,
                attempts,
            },
        ))
    }

    fn empty_segments() -> Result<(SegmentsEnvelope, SendMetadata), TransportError> {
        Ok((
            SegmentsEnvelope {
                items: Vec::new(),
                total: 0,
                protocol_version: 3,
            },
            test_metadata(),
        ))
    }

    fn confirmed_segments(
        segment_key: String,
        file_name: &str,
        sha: String,
        size: u64,
    ) -> Result<(SegmentsEnvelope, SendMetadata), TransportError> {
        listed_segments(segment_key, file_name, sha, size, Some("present"))
    }

    fn listed_segments(
        segment_key: String,
        file_name: &str,
        sha: String,
        size: u64,
        status: Option<&str>,
    ) -> Result<(SegmentsEnvelope, SendMetadata), TransportError> {
        Ok((
            SegmentsEnvelope {
                items: vec![SegmentItem {
                    key: segment_key,
                    observed: false,
                    files: vec![SegmentFile {
                        name: file_name.to_string(),
                        sha256: sha,
                        size,
                        status: match status {
                            Some("present") => SegmentFileStatus::Present,
                            Some("missing") => SegmentFileStatus::Missing,
                            Some("processed") => SegmentFileStatus::Processed,
                            other => panic!("unsupported custody status {other:?}"),
                        },
                        submitted_name: None,
                    }],
                    original_key: None,
                }],
                total: 1,
                protocol_version: 3,
            },
            test_metadata(),
        ))
    }

    fn adversarial_body() -> String {
        "SECRET https://10.0.0.5/y?token=abc C:\\Users\\me\\seg.mp4 sha256:abc".into()
    }

    fn attributable_request_body() -> String {
        r#"{"error":"Ingest request refused","reason_code":"envelope_invalid","detail":"test"}"#
            .into()
    }

    #[test]
    fn only_attributable_local_request_rejections_count() {
        let json_error = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        let cases = [
            (
                TransportError::Io(std::io::Error::other("C:\\Users\\me\\seg.mp4")),
                false,
            ),
            (TransportError::Tls("tls secret".into()), false),
            (TransportError::Crypto("crypto secret".into()), false),
            (
                TransportError::Mux(spl_core::mux::MuxError::Incomplete),
                false,
            ),
            (
                TransportError::Http(spl_core::http::HttpError::BadStatusLine(
                    "HTTP/1.1 SECRET".into(),
                )),
                false,
            ),
            (TransportError::Json(json_error), false),
            (TransportError::PairLink("token=abc".into()), false),
            (TransportError::Pairing("sha256:abc".into()), false),
            (TransportError::Ingest("duplicate filename".into()), false),
            (
                TransportError::Rejected {
                    status: 503,
                    body: adversarial_body(),
                },
                false,
            ),
            (
                TransportError::Rejected {
                    status: 400,
                    body: r#"{"reason_code":"envelope_invalid"}"#.into(),
                },
                true,
            ),
            (
                TransportError::Rejected {
                    status: 400,
                    body: r#"{"reason_code":"protocol_version_legacy"}"#.into(),
                },
                false,
            ),
            (
                TransportError::Rejected {
                    status: 413,
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
                false,
            ),
            (TransportError::NoEndpoint, false),
            (TransportError::NotPaired, false),
            (TransportError::LocalOffset, false),
        ];

        for (error, expected) in cases {
            assert_eq!(is_attributable_rejection(&error), expected, "{error:?}");
        }
    }

    // Previous-model disclaimer: Retained under the 12.2.0 receipt model; earlier tests relied on manifest verification.
    #[tokio::test]
    async fn reject_isolation_processes_later_segments() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let file_name = "display_1_screen.mp4";
        let boundary1 = 1_700_000_100;
        let boundary2 = boundary1 + 300;
        let bytes1 = b"poison segment".to_vec();
        let bytes2 = b"healthy segment".to_vec();
        let key2 = civil::segment_key_string_local(boundary2, 0, 300);
        let store = MultiSegmentStore::new(vec![
            (1, boundary1, file_name, bytes1),
            (2, boundary2, file_name, bytes2.clone()),
        ]);
        let handle = store.clone();
        let client = FakeClient::new(
            vec![
                Err(TransportError::Rejected {
                    status: 400,
                    body: attributable_request_body(),
                }),
                accepted_ingest(&key2, file_name, &bytes2, 1),
            ],
            vec![],
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

    // Previous-model disclaimer: Retained under the 12.2.0 receipt model; advances simulated time between ticks to exercise the quarantine threshold.
    #[tokio::test(start_paused = true)]
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
                        status: 400,
                        body: attributable_request_body(),
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
            tokio::time::advance(Duration::from_secs(86401)).await;
        }

        let snapshot = sync.lock().unwrap().clone();
        assert!(handle.quarantined(1));
        assert!(handle.pending_indices().is_empty());
        assert_eq!(snapshot.upload.quarantined_segments, 1);
        assert_eq!(snapshot.upload.pending_segments, 0);
        assert_eq!(snapshot.upload.recent_error_count, 0);
        assert_eq!(snapshot.upload.last_error.as_deref(), Some("http_400"));
        let last_error = snapshot.upload.last_error.unwrap();
        assert!(!last_error.contains("SECRET"));
        assert!(!last_error.contains("token"));
        assert!(!last_error.contains("Users"));
        assert!(!last_error.contains("https://"));
        assert!(!last_error.contains("sha256"));
        assert!(!last_error.contains("10.0.0.5"));
    }

    // Previous-model disclaimer: Retained under the 12.2.0 receipt model.
    #[tokio::test]
    async fn transport_error_aborts_tick_and_leaves_rest_untried() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let file_name = "display_1_screen.mp4";
        let boundary1 = 1_700_000_100;
        let boundary2 = boundary1 + 300;
        let bytes2 = b"second".to_vec();
        let key2 = civil::segment_key_string_local(boundary2, 0, 300);
        let store = MultiSegmentStore::new(vec![
            (1, boundary1, file_name, b"first".to_vec()),
            (2, boundary2, file_name, bytes2.clone()),
        ]);
        let client = FakeClient::new(
            vec![
                Err(TransportError::Io(std::io::Error::other(
                    "C:\\Users\\me\\seg.mp4",
                ))),
                accepted_ingest(&key2, file_name, &bytes2, 1),
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

    // Previous-model disclaimer: Retained under the 12.2.0 receipt model.
    #[tokio::test]
    async fn a_stopped_pairing_reads_as_refused_and_an_ordinary_failure_does_not() {
        for (stop, refused) in [
            (Some(HandshakeStop::TlsAccessDenied), true),
            (Some(HandshakeStop::RefusalsExhausted), true),
            (None, false),
        ] {
            let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
            sync.lock().unwrap().pairing.phase = PairingPhase::Paired;
            let store = MultiSegmentStore::new(vec![(
                1,
                1_700_000_100,
                "display_1_screen.mp4",
                b"first".to_vec(),
            )]);
            let client = FakeClient::new(
                vec![Err(TransportError::Tls("tls access denied".into()))],
                vec![],
            );
            *client.stop.lock().unwrap() = stop;
            let coordinator =
                coordinator_with_client(client.clone(), Box::new(store), sync.clone());

            assert!(coordinator.tick().await.is_err());
            let pairing = sync.lock().unwrap().pairing.clone();
            if refused {
                assert_eq!(pairing.phase, PairingPhase::Failed, "{stop:?}");
                assert_eq!(
                    pairing.detail.as_deref(),
                    Some(PAIRING_REFUSED_DETAIL),
                    "{stop:?}"
                );
            } else {
                assert_eq!(pairing.phase, PairingPhase::Paired);
                assert_eq!(pairing.detail, None);
            }
        }
    }

    // Previous-model disclaimer: Retained under the 12.2.0 receipt model.
    #[tokio::test]
    async fn read_error_skips_segment_without_quarantine() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let file_name = "display_1_screen.mp4";
        let boundary1 = 1_700_000_100;
        let boundary2 = boundary1 + 300;
        let bytes2 = b"healthy segment".to_vec();
        let key2 = civil::segment_key_string_local(boundary2, 0, 300);
        let store = MultiSegmentStore::new(vec![
            (1, boundary1, file_name, b"locked".to_vec()),
            (2, boundary2, file_name, bytes2.clone()),
        ])
        .with_read_error(1, "C:\\Users\\me\\seg.mp4");
        let handle = store.clone();
        let client = FakeClient::new(vec![accepted_ingest(&key2, file_name, &bytes2, 1)], vec![]);
        let coordinator = coordinator_with_client(client, Box::new(store), sync.clone());

        let confirmed = coordinator.tick().await.unwrap();
        let snapshot = sync.lock().unwrap().clone();

        assert_eq!(confirmed, 1);
        assert_eq!(handle.pending_indices(), vec![1]);
        assert!(handle.removed(2));
        assert_eq!(snapshot.upload.quarantined_segments, 0);
        assert!(snapshot.upload.failed_segments >= 1);
    }

    // Previous-model disclaimer: Retained under the 12.2.0 receipt model; uses simulated time advance across ticks.
    #[tokio::test(start_paused = true)]
    async fn list_reject_does_not_feed_quarantine_counter() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let file_name = "display_1_screen.mp4";
        let boundary = 1_700_000_100;
        let store = MultiSegmentStore::new(vec![(1, boundary, file_name, b"segment".to_vec())]);
        let handle = store.clone();
        let mut ingests = Vec::new();
        for _ in 0..(QUARANTINE_AFTER_REJECTS - 1) {
            ingests.push(Err(TransportError::Rejected {
                status: 400,
                body: attributable_request_body(),
            }));
        }
        ingests.push(accepted_unconfirmed_ingest(1));
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
            tokio::time::advance(Duration::from_secs(86401)).await;
        }
        assert!(matches!(
            coordinator.tick().await,
            Err(TransportError::Rejected { status: 403, .. })
        ));
        let snapshot = sync.lock().unwrap().clone();

        assert_eq!(handle.pending_indices(), vec![1]);
        assert!(!handle.quarantined(1));
        assert_eq!(snapshot.upload.quarantined_segments, 0);
        assert_eq!(snapshot.upload.recent_error_count, 1);
    }

    // Previous-model disclaimer: Retained under the 12.2.0 receipt model.
    #[tokio::test]
    async fn list_transport_error_aborts_after_accepted_ingest() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let file_name = "display_1_screen.mp4";
        let boundary = 1_700_000_100;
        let store = MultiSegmentStore::new(vec![(1, boundary, file_name, b"segment".to_vec())]);
        let client = FakeClient::new(
            vec![accepted_unconfirmed_ingest(1)],
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

    // Previous-model disclaimer: Retained under the 12.2.0 receipt model.
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
        let store = MultiSegmentStore::new(vec![
            (1, boundary1, file_name, bytes1.clone()),
            (2, boundary2, file_name, bytes2),
            (3, boundary3, file_name, bytes3),
        ]);
        let handle = store.clone();
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let client = CancelAfterFirstListClient::new(
            vec![accepted_ingest(&key1, file_name, &bytes1, 1)],
            vec![],
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

    // Remove-failure timing: at 3540s (59 min) the segment is still held; at 3601s local finish removes it with no listing and no second POST.
    #[tokio::test(start_paused = true)]
    async fn cleanup_remove_failure_is_nonfatal_and_reconfirms_next_tick() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let file_name = "display_1_screen.mp4";
        let boundary1 = 1_700_000_100;
        let boundary2 = boundary1 + 300;
        let bytes1 = b"first segment".to_vec();
        let bytes2 = b"second segment".to_vec();
        let key1 = civil::segment_key_string_local(boundary1, 0, 300);
        let key2 = civil::segment_key_string_local(boundary2, 0, 300);
        let store = MultiSegmentStore::new(vec![
            (1, boundary1, file_name, bytes1.clone()),
            (2, boundary2, file_name, bytes2.clone()),
        ])
        .with_remove_fails_once(1);
        let handle = store.clone();
        let client = FakeClient::new(
            vec![
                accepted_ingest(&key1, file_name, &bytes1, 1),
                accepted_ingest(&key2, file_name, &bytes2, 1),
            ],
            vec![],
        );
        let coordinator = coordinator_with_client(client.clone(), Box::new(store), sync.clone());

        let first = coordinator.tick().await.unwrap();
        assert_eq!(first, 2);
        assert!(!handle.removed(1));
        assert!(handle.removed(2));

        // At 3540s (59 minutes): still held, tick does not remove, POST, or list
        tokio::time::advance(Duration::from_secs(3540)).await;
        let mid = coordinator.tick().await.unwrap();
        assert_eq!(mid, 0);
        assert!(!handle.removed(1));
        assert!(client.lists.lock().unwrap().is_empty());
        assert!(client.ingests.lock().unwrap().is_empty());

        // At 3601s: hold expired, local_finish removes it with no listing and no second POST
        tokio::time::advance(Duration::from_secs(61)).await;
        let second = coordinator.tick().await.unwrap();
        assert_eq!(second, 0);
        assert!(handle.removed(1));
        assert!(handle.pending_indices().is_empty());
        assert!(client.lists.lock().unwrap().is_empty());
        assert!(client.ingests.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn cleanup_remove_failure_local_finish_survives_transport_error() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let file_name = "display_1_screen.mp4";
        let boundary1 = 1_700_000_100;
        let boundary2 = boundary1 + 300;
        let bytes1 = b"first segment".to_vec();
        let bytes2 = b"second segment".to_vec();
        let key1 = civil::segment_key_string_local(boundary1, 0, 300);
        let store = MultiSegmentStore::new(vec![(1, boundary1, file_name, bytes1.clone())])
            .with_remove_fails_once(1);
        let handle = store.clone();
        let client = FakeClient::new(
            vec![
                accepted_ingest(&key1, file_name, &bytes1, 1),
                Err(TransportError::Io(std::io::Error::other("network failure"))),
            ],
            vec![],
        );
        let coordinator = coordinator_with_client(client.clone(), Box::new(store), sync.clone());

        // First tick confirms segment 1 but delete fails and is held for 1 hour
        let first = coordinator.tick().await.unwrap();
        assert_eq!(first, 1);
        assert!(!handle.removed(1));

        // Insert second segment into store before the 1-hour tick
        handle.insert_segment(2, boundary2, file_name, bytes2);

        // Advance past 1 hour
        tokio::time::advance(Duration::from_secs(3601)).await;

        // Second tick runs local_finish (deleting segment 1), then attempts segment 2 which returns Io error -> tick returns Err
        let res = coordinator.tick().await;
        assert!(res.is_err());

        // Segment 1 is still deleted despite the tick's subsequent transport error
        assert!(handle.removed(1));
    }

    #[tokio::test(start_paused = true)]
    async fn cleanup_write_ack_failure_is_nonfatal_and_recovers_next_post_already_held() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let file_name = "display_1_screen.mp4";
        let boundary = now_epoch_secs();
        let bytes = b"retained segment".to_vec();
        let key = civil::segment_key_string_local(boundary, 0, 300);
        let store = MultiSegmentStore::new(vec![(1, boundary, file_name, bytes.clone())])
            .with_mark_confirmed_fails_once(1);
        let handle = store.clone();
        let client = FakeClient::new(
            vec![
                accepted_ingest(&key, file_name, &bytes, 1),
                scripted_ingest(
                    "ok",
                    Some(&key),
                    None,
                    vec![FileDescriptor {
                        submitted: file_name.to_string(),
                        written: file_name.to_string(),
                        size: bytes.len() as u64,
                        sha256: ca::sha256_hex(&bytes),
                        disposition: "already_held".to_string(),
                    }],
                    1,
                ),
            ],
            vec![],
        );
        let coordinator = coordinator_with_client(client.clone(), Box::new(store), sync);

        // Tick 1: write_ack fails, so nothing is deleted and no listing is performed
        let first = coordinator.tick().await.unwrap();
        assert_eq!(first, 1);
        assert!(!handle.removed(1));
        assert_eq!(handle.pending_indices(), vec![1]);
        assert!(client.lists.lock().unwrap().is_empty());

        // Tick 2: next POST answered with disposition already_held -> write_ack succeeds and deletes segment
        let second = coordinator.tick().await.unwrap();
        assert_eq!(second, 1);
        assert!(handle.removed(1));
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
    async fn terminal_transport_failures_keep_unconfirmed_sealed_segment_and_path_unset() {
        let errors = [
            (TransportError::ReplayUnsafe, "replay_unsafe"),
            (TransportError::RelayDisabled, "relay_disabled"),
            (TransportError::RelayRetired, "relay_retired"),
            (
                TransportError::RelayPublicationRejected,
                "relay_publication_rejected",
            ),
            (
                TransportError::RelayPublicationIndeterminate,
                "relay_publication_indeterminate",
            ),
        ];

        for (error, expected_code) in errors {
            let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
            let store = OneSegmentStore::new(
                1_700_000_100,
                "display_1_screen.mp4",
                b"sealed segment".to_vec(),
            );
            let removed = store.removed_handle();
            let client = FakeClient::new(vec![Err(error)], vec![]);
            let coordinator = coordinator_with_client(client, Box::new(store), sync.clone());

            let result = coordinator.tick().await;
            assert_eq!(
                result
                    .as_ref()
                    .err()
                    .map(|error| transport_error_code(error).to_string()),
                Some(expected_code.to_string())
            );
            assert!(
                !*removed.lock().unwrap(),
                "terminal transport failure discarded unconfirmed sealed custody"
            );
            let snapshot = sync.lock().unwrap().clone();
            assert_eq!(snapshot.upload.failed_segments, 1);
            assert_eq!(snapshot.upload.last_error.as_deref(), Some(expected_code));
            assert_eq!(snapshot.upload.last_upload_path, None);
            let reason = snapshot.upload.last_error.unwrap();
            assert!(reason.len() <= 240);
            assert!(!reason.contains("token"));
            assert!(!reason.contains("http"));
            assert!(!reason.contains('/'));
        }
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

    // Previous-model disclaimer: Retained under the 12.2.0 receipt model; tests fallback when file_descriptors is Absent.
    #[tokio::test(start_paused = true)]
    async fn accepted_unconfirmed_does_not_set_earned_upload_fields_until_confirmed_once() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let boundary = 1_700_000_100;
        let file_name = "display_1_screen.mp4";
        let bytes = b"segment bytes".to_vec();
        let segment_key = civil::segment_key_string_local(boundary, 0, 300);
        let client = FakeClient::new(
            vec![
                accepted_unconfirmed_ingest(2),
                accepted_ingest(&segment_key, file_name, &bytes, 3),
            ],
            vec![empty_segments()],
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

        tokio::time::advance(Duration::from_secs(86401)).await;
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

    // Previous-model disclaimer: Retained under the 12.2.0 receipt model.
    #[tokio::test]
    async fn local_offset_failure_aborts_without_submitting_key_and_retries() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let boundary = 1_700_000_100;
        let file_name = "display_1_screen.mp4";
        let bytes = b"segment bytes".to_vec();
        let segment_key = civil::segment_key_string_local(boundary, 0, 300);
        assert!(matches!(
            FailingOffset.local_offset_secs(boundary),
            Err(observer_model::LocalOffsetError::Lookup)
        ));
        let client = FakeClient::new(
            vec![accepted_ingest(&segment_key, file_name, &bytes, 1)],
            vec![],
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

    // Previous-model disclaimer: Retained under the 12.2.0 receipt model.
    #[tokio::test]
    async fn duplicate_reconciles_against_existing_segment_key() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let boundary = 1_700_000_100;
        let file_name = "display_1_screen.mp4";
        let bytes = b"segment bytes".to_vec();
        let local_key = civil::segment_key_string_local(boundary, 0, 300);
        let server_key = "111111_300";
        assert_ne!(local_key, server_key);
        let client = FakeClient::new(
            vec![scripted_ingest(
                "duplicate",
                None,
                Some(server_key),
                vec![FileDescriptor {
                    submitted: file_name.to_string(),
                    written: file_name.to_string(),
                    size: bytes.len() as u64,
                    sha256: ca::sha256_hex(&bytes),
                    disposition: "already_held".to_string(),
                }],
                1,
            )],
            vec![],
        );
        let store = OneSegmentStore::new(boundary, file_name, bytes);
        let removed = store.removed_handle();
        let coordinator = coordinator_with_client(client, Box::new(store), sync.clone());

        let confirmed = coordinator.tick().await.unwrap();

        assert_eq!(confirmed, 1);
        assert!(*removed.lock().unwrap());
        assert_eq!(sync.lock().unwrap().upload.uploaded_segments, 1);
    }

    // Previous-model disclaimer: Retained under the 12.2.0 receipt model.
    #[tokio::test]
    async fn collision_reconciles_against_remapped_segment_key() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let boundary = 1_700_000_100;
        let file_name = "display_1_screen.mp4";
        let bytes = b"segment bytes".to_vec();
        let remapped_key = "222222_300";
        let client = FakeClient::new(
            vec![scripted_ingest(
                "collision",
                Some(remapped_key),
                None,
                vec![FileDescriptor {
                    submitted: file_name.to_string(),
                    written: file_name.to_string(),
                    size: bytes.len() as u64,
                    sha256: ca::sha256_hex(&bytes),
                    disposition: "written".to_string(),
                }],
                1,
            )],
            vec![],
        );
        let store = OneSegmentStore::new(boundary, file_name, bytes);
        let removed = store.removed_handle();
        let coordinator = coordinator_with_client(client, Box::new(store), sync.clone());

        let confirmed = coordinator.tick().await.unwrap();

        assert_eq!(confirmed, 1);
        assert!(*removed.lock().unwrap());
        assert_eq!(sync.lock().unwrap().upload.uploaded_segments, 1);
    }

    // Previous-model disclaimer: Retained under the 12.2.0 receipt model; uses simulated time advance across ticks.
    #[tokio::test(start_paused = true)]
    async fn missing_status_does_not_confirm_or_delete_until_held() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let boundary = 1_700_000_100;
        let file_name = "display_1_screen.mp4";
        let bytes = b"segment bytes".to_vec();
        let sha = ca::sha256_hex(&bytes);
        let segment_key = civil::segment_key_string_local(boundary, 0, 300);
        let client = FakeClient::new(
            vec![
                accepted_unconfirmed_ingest(1),
                accepted_unconfirmed_ingest(2),
            ],
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

        tokio::time::advance(Duration::from_secs(86401)).await;
        let second = coordinator.tick().await.unwrap();
        assert_eq!(second, 0);
        assert!(*removed.lock().unwrap());
        assert_eq!(sync.lock().unwrap().upload.uploaded_segments, 0);
    }

    // Previous-model disclaimer: Retained under the 12.2.0 receipt model; uses simulated time advance across ticks.
    #[tokio::test(start_paused = true)]
    async fn unknown_custody_status_is_retry_eligible_and_the_original_segment_confirms_later() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let boundary = 1_700_000_100;
        let file_name = "audio.flac";
        let bytes = b"original bytes".to_vec();
        let segment_key = civil::segment_key_string_local(boundary, 0, 300);
        let unknown_status = || {
            TransportError::Json(
                serde_json::from_str::<SegmentsEnvelope>(
                    r#"{"items":[{"key":"080000_600","observed":false,"files":[{"name":"audio.flac","size":91,"sha256":"abc","status":"quarantined"}]}],"total":1,"protocol_version":3}"#,
                )
                .unwrap_err(),
            )
        };
        let client = FakeClient::new(
            vec![
                accepted_unconfirmed_ingest(1),
                accepted_unconfirmed_ingest(2),
                accepted_unconfirmed_ingest(3),
            ],
            vec![
                Err(unknown_status()),
                Err(unknown_status()),
                confirmed_segments(
                    segment_key,
                    file_name,
                    ca::sha256_hex(&bytes),
                    bytes.len() as u64,
                ),
            ],
        );
        let store = OneSegmentStore::new(boundary, file_name, bytes);
        let removed = store.removed_handle();
        let coordinator = coordinator_with_client(client, Box::new(store), sync);

        assert!(matches!(
            coordinator.tick().await,
            Err(TransportError::Json(_))
        ));
        tokio::time::advance(Duration::from_secs(3601)).await;
        assert!(matches!(
            coordinator.tick().await,
            Err(TransportError::Json(_))
        ));
        assert!(!*removed.lock().unwrap());
        assert!(coordinator.quarantine_counts.lock().unwrap().is_empty());

        tokio::time::advance(Duration::from_secs(3601)).await;
        assert_eq!(coordinator.tick().await.unwrap(), 0);
        assert!(*removed.lock().unwrap());
        assert!(coordinator.quarantine_counts.lock().unwrap().is_empty());
    }

    // Previous-model disclaimer: Retained under the 12.2.0 receipt model; uses simulated time advance across ticks.
    #[tokio::test(start_paused = true)]
    async fn conflict_and_failed_statuses_do_not_accumulate_quarantine() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let store = OneSegmentStore::new(1_700_000_100, "audio.flac", b"retained".to_vec());
        let removed = store.removed_handle();
        let client = FakeClient::new(
            vec![
                scripted_ingest("conflict", None, None, vec![], 1),
                scripted_ingest("failed", None, None, vec![], 2),
            ],
            vec![],
        );
        let coordinator = coordinator_with_client(client, Box::new(store), sync);

        assert_eq!(coordinator.tick().await.unwrap(), 0);
        tokio::time::advance(Duration::from_secs(3601)).await;
        assert_eq!(coordinator.tick().await.unwrap(), 0);
        assert!(!*removed.lock().unwrap());
        assert!(coordinator.quarantine_counts.lock().unwrap().is_empty());
    }

    // Previous-model disclaimer: Retained under the 12.2.0 receipt model.
    #[tokio::test(start_paused = true)]
    async fn tick_failure_marks_disconnected_and_recovery_triggers_refresh() {
        let jv_path = std::env::temp_dir().join(format!(
            "journal-version-coord-test-{}.json",
            std::process::id()
        ));
        let jv = Arc::new(crate::journal_version::JournalVersionController::new(
            jv_path,
        ));
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let cred = dummy_credential();
        jv.begin_session(&cred, &sync);

        // Simulate an already fresh version in sync snapshot.
        {
            let mut s = sync.lock().unwrap();
            s.journal_version = Some("0.4.0".into());
            s.journal_version_fresh = true;
        }

        let store = OneSegmentStore::new(1_700_000_100, "audio.flac", b"audio".to_vec());
        let client = FakeClient::new(
            vec![
                Err(TransportError::Io(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "reset",
                ))),
                scripted_ingest("conflict", None, None, vec![], 1),
                scripted_ingest("conflict", None, None, vec![], 1),
            ],
            vec![],
        );

        let coordinator =
            coordinator_with_client_and_jv(client, Box::new(store), sync.clone(), jv.clone());

        assert_eq!(jv.in_flight_token(), None);

        // First tick fails: note_tick_failure should mark journal_version_fresh = false.
        let res = coordinator.tick().await;
        assert!(res.is_err());
        {
            let s = sync.lock().unwrap();
            assert_eq!(s.journal_version.as_deref(), Some("0.4.0"));
            assert!(!s.journal_version_fresh);
            assert_eq!(s.upload.recent_error_count, 1);
        }
        assert_eq!(jv.in_flight_token(), None);

        // Recovery does not start an independent version-refresh burst.
        tokio::time::advance(Duration::from_secs(3601)).await;
        let res = coordinator.tick().await;
        assert_eq!(res.unwrap(), 0);
        {
            let s = sync.lock().unwrap();
            assert_eq!(s.upload.recent_error_count, 0);
        }
        assert_eq!(jv.in_flight_token(), None);

        // Third tick succeeds when already healthy (recent_error_count == 0) -> does NOT trigger a refresh.
        jv.apply_result((1, 1), Ok("0.4.1".into()), &sync);
        assert_eq!(jv.in_flight_token(), None);
        tokio::time::advance(Duration::from_secs(3601)).await;
        let res = coordinator.tick().await;
        assert_eq!(res.unwrap(), 0);
        assert_eq!(jv.in_flight_token(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn acceptance_2_received_not_written_daily_bound() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let boundary = 1_700_000_100;
        let file_name = "screen.mp4";
        let bytes = b"payload".to_vec();
        let key = civil::segment_key_string_local(boundary, 0, 300);
        let store = OneSegmentStore::new(boundary, file_name, bytes.clone());
        let handle = store.removed_handle();
        let client = FakeClient::new(
            vec![
                scripted_ingest(
                    "ok",
                    Some(&key),
                    None,
                    vec![FileDescriptor {
                        submitted: file_name.to_string(),
                        written: file_name.to_string(),
                        size: bytes.len() as u64,
                        sha256: ca::sha256_hex(&bytes),
                        disposition: "received_not_written".to_string(),
                    }],
                    1,
                ),
                accepted_ingest(&key, file_name, &bytes, 1),
            ],
            vec![],
        );
        let coordinator = coordinator_with_client(client, Box::new(store), sync.clone());

        // Tick 1: returns received_not_written -> 24h bound, no deletion, invalid_receipts == 0
        let confirmed = coordinator.tick().await.unwrap();
        assert_eq!(confirmed, 0);
        assert!(!*handle.lock().unwrap());
        let snapshot = sync.lock().unwrap().clone();
        assert_eq!(snapshot.upload.invalid_receipts, 0);

        // Tick 2 (1 hour later): still bound (24h), skipped
        tokio::time::advance(Duration::from_secs(3600)).await;
        let confirmed2 = coordinator.tick().await.unwrap();
        assert_eq!(confirmed2, 0);

        // Tick 3 (24h+1s later): bound expired, re-attempts and confirms
        tokio::time::advance(Duration::from_secs(82801)).await;
        let confirmed3 = coordinator.tick().await.unwrap();
        assert_eq!(confirmed3, 1);
        assert!(*handle.lock().unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn acceptance_3_segment_removed_terminal_state() {
        // 1. HTTP 500 rejection with reason_code "segment_removed" -> deletes segment via try_gate_delete
        {
            let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
            let boundary = 1_700_000_100;
            let file_name = "screen.mp4";
            let bytes = b"payload".to_vec();
            let store = OneSegmentStore::new(boundary, file_name, bytes);
            let handle = store.removed_handle();
            let client = FakeClient::new(
                vec![Err(TransportError::Rejected {
                    status: 500,
                    body: r#"{"error":"Removed","reason_code":"segment_removed"}"#.to_string(),
                })],
                vec![],
            );
            let coordinator = coordinator_with_client(client, Box::new(store), sync.clone());

            let confirmed = coordinator.tick().await.unwrap();
            assert_eq!(confirmed, 0);
            assert!(*handle.lock().unwrap());
            let snapshot = sync.lock().unwrap().clone();
            assert_eq!(snapshot.upload.segment_removed_segments, 1);
            assert_eq!(snapshot.upload.uploaded_segments, 1);
            assert_eq!(snapshot.upload.recent_error_count, 0);
            assert_eq!(snapshot.upload.last_error, None);
        }

        // 2. HTTP 200 with non-accepted status and reason_code "segment_removed" -> deletes segment
        {
            let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
            let boundary = 1_700_000_100;
            let file_name = "screen.mp4";
            let bytes = b"payload".to_vec();
            let store = OneSegmentStore::new(boundary, file_name, bytes);
            let handle = store.removed_handle();
            let client = FakeClient::new(
                vec![Ok((
                    IngestResponse {
                        status: IngestStatus::Failed,
                        segment: None,
                        existing_segment: None,
                        reason_code: Some("segment_removed".to_string()),
                        file_descriptors: FileDescriptors::Absent,
                    },
                    SendMetadata {
                        path: TransportPath::Direct,
                        attempts: 1,
                    },
                ))],
                vec![],
            );
            let coordinator = coordinator_with_client(client, Box::new(store), sync.clone());

            let confirmed = coordinator.tick().await.unwrap();
            assert_eq!(confirmed, 0);
            assert!(*handle.lock().unwrap());
            let snapshot = sync.lock().unwrap().clone();
            assert_eq!(snapshot.upload.segment_removed_segments, 1);
            assert_eq!(snapshot.upload.uploaded_segments, 1);
            assert_eq!(snapshot.upload.recent_error_count, 0);
            assert_eq!(snapshot.upload.last_error, None);
        }

        // 3. HTTP 500 with unparsable / generic body -> keeps segment
        {
            let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
            let boundary = 1_700_000_100;
            let file_name = "screen.mp4";
            let bytes = b"payload".to_vec();
            let store = OneSegmentStore::new(boundary, file_name, bytes);
            let handle = store.removed_handle();
            let client = FakeClient::new(
                vec![Err(TransportError::Rejected {
                    status: 500,
                    body: "500 Internal Server Error".to_string(),
                })],
                vec![],
            );
            let coordinator = coordinator_with_client(client, Box::new(store), sync.clone());

            let confirmed = coordinator.tick().await.unwrap();
            assert_eq!(confirmed, 0);
            assert!(!*handle.lock().unwrap());
            let snapshot = sync.lock().unwrap().clone();
            assert_eq!(snapshot.upload.segment_removed_segments, 0);
            assert_eq!(snapshot.upload.uploaded_segments, 0);
            assert!(snapshot.upload.last_error.is_some());
        }

        // 4. remove_entry failure on segment_removed is not delivered yet: the segment
        // is held for an hour, not posted again, then removed and counted once
        {
            let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
            let boundary = 1_700_000_100;
            let file_name = "screen.mp4";
            let bytes = b"payload".to_vec();
            let store = MultiSegmentStore::new(vec![(1, boundary, file_name, bytes.clone())])
                .with_remove_fails_once(1);
            store
                .state
                .lock()
                .unwrap()
                .bytes
                .entry(1)
                .or_default()
                .insert(observer_model::LEN_FILE_NAME.to_string(), b"300".to_vec());
            let client = FakeClient::new(
                vec![
                    Err(TransportError::Rejected {
                        status: 500,
                        body: r#"{"error":"Removed","reason_code":"segment_removed"}"#.to_string(),
                    }),
                    Err(TransportError::Rejected {
                        status: 500,
                        body: r#"{"error":"Removed","reason_code":"segment_removed"}"#.to_string(),
                    }),
                ],
                vec![],
            );
            let coordinator =
                coordinator_with_client(client.clone(), Box::new(store), sync.clone());

            let confirmed1 = coordinator.tick().await.unwrap();
            assert_eq!(confirmed1, 0);
            let snap1 = sync.lock().unwrap().clone();
            assert_eq!(snap1.upload.uploaded_segments, 0);
            assert_eq!(snap1.upload.segment_removed_segments, 0);
            let entries1 = coordinator.store.list_entries(1).unwrap();
            assert!(entries1
                .iter()
                .any(|e| e.name == observer_model::LEN_FILE_NAME));
            assert!(coordinator.is_held(1, coordinator.monotonic_now_epoch_secs()));

            // The next tick inside the hold makes no request
            let confirmed2 = coordinator.tick().await.unwrap();
            assert_eq!(confirmed2, 0);
            assert_eq!(client.posts.lock().unwrap().len(), 1);
            assert_eq!(sync.lock().unwrap().upload.uploaded_segments, 0);

            // After the hold the retry removes the segment and counts it once
            tokio::time::advance(Duration::from_secs(3601)).await;
            let confirmed3 = coordinator.tick().await.unwrap();
            assert_eq!(confirmed3, 0);
            assert_eq!(client.posts.lock().unwrap().len(), 2);
            let snap3 = sync.lock().unwrap().clone();
            assert_eq!(snap3.upload.uploaded_segments, 1);
            assert_eq!(snap3.upload.segment_removed_segments, 1);
            assert_eq!(coordinator.store.scan().unwrap().len(), 0);
            assert_eq!(coordinator.store.list_entries(1).unwrap().len(), 0);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn acceptance_4_error_ladder() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let boundary = 1_700_000_100;
        let file_name = "screen.mp4";
        let bytes = b"payload".to_vec();
        let key = civil::segment_key_string_local(boundary, 0, 300);
        let store = OneSegmentStore::new(boundary, file_name, bytes.clone());
        let handle = store.removed_handle();
        let client = FakeClient::new(
            vec![
                Err(TransportError::Rejected {
                    status: 500,
                    body: r#"{"status":"failed"}"#.to_string(),
                }),
                Err(TransportError::Rejected {
                    status: 500,
                    body: r#"{"status":"failed"}"#.to_string(),
                }),
                Err(TransportError::Rejected {
                    status: 500,
                    body: r#"{"status":"failed"}"#.to_string(),
                }),
                accepted_ingest(&key, file_name, &bytes, 1),
            ],
            vec![],
        );
        let coordinator = coordinator_with_client(client, Box::new(store), sync.clone());

        // Streak 1 (1st rejection): 1-hour bound
        assert_eq!(coordinator.tick().await.unwrap(), 0);

        // At 30m: bound active, skipped
        tokio::time::advance(Duration::from_secs(1800)).await;
        assert_eq!(coordinator.tick().await.unwrap(), 0);

        // At 1h+1s: Streak 2 (2nd rejection): 1-hour bound
        tokio::time::advance(Duration::from_secs(1801)).await;
        assert_eq!(coordinator.tick().await.unwrap(), 0);

        // At 1h+1s: Streak 3 (3rd rejection): 24-hour bound
        tokio::time::advance(Duration::from_secs(3601)).await;
        assert_eq!(coordinator.tick().await.unwrap(), 0);

        // At 1h+1s after 3rd error: still bound by 24h ladder!
        tokio::time::advance(Duration::from_secs(3601)).await;
        assert_eq!(coordinator.tick().await.unwrap(), 0);

        // At 24h+1s after 3rd error: bound expired, succeeds!
        tokio::time::advance(Duration::from_secs(86400)).await;
        assert_eq!(coordinator.tick().await.unwrap(), 1);
        assert!(*handle.lock().unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn acceptance_4a_day_listing_bounds() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let boundary = 1_700_000_100;
        let file_name = "screen.mp4";
        let bytes = b"payload".to_vec();
        let store = OneSegmentStore::new(boundary, file_name, bytes);
        let client = FakeClient::new(
            vec![
                accepted_unconfirmed_ingest(1),
                accepted_unconfirmed_ingest(2),
                accepted_unconfirmed_ingest(3),
            ],
            vec![
                Err(TransportError::Rejected {
                    status: 409,
                    body: r#"{"error":"Conflict","reason_code":"content_conflict"}"#.to_string(),
                }),
                empty_segments(),
            ],
        );
        let coordinator = coordinator_with_client(client, Box::new(store), sync.clone());

        // Tick 1: Ingest succeeds unconfirmed, listing fails with 409 content_conflict -> listing_refusals increments, 1h day bound
        let res1 = coordinator.tick().await;
        assert_eq!(res1.unwrap(), 0);
        let snapshot = sync.lock().unwrap().clone();
        assert_eq!(snapshot.upload.listing_refusals, 1);

        // Tick 2 (10m later): day is bound, listing is not retried
        tokio::time::advance(Duration::from_secs(600)).await;
        let res2 = coordinator.tick().await;
        assert_eq!(res2.unwrap(), 0);

        // Tick 3 (1h+1s later): day bound expired, listing is retried
        tokio::time::advance(Duration::from_secs(3001)).await;
        let res3 = coordinator.tick().await;
        assert_eq!(res3.unwrap(), 0);
    }

    #[tokio::test]
    async fn listing_device_scoped_403_aborts_tick() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let boundary = 1_700_000_100;
        let file_name = "screen.mp4";
        let bytes = b"payload".to_vec();
        let store = OneSegmentStore::new(boundary, file_name, bytes);
        let client = FakeClient::new(
            vec![accepted_unconfirmed_ingest(1)],
            vec![Err(TransportError::Rejected {
                status: 403,
                body: "forbidden".to_string(),
            })],
        );
        let coordinator = coordinator_with_client(client, Box::new(store), sync.clone());

        let res = coordinator.tick().await;
        assert!(matches!(
            res,
            Err(TransportError::Rejected { status: 403, .. })
        ));
        let snapshot = sync.lock().unwrap().clone();
        assert_eq!(snapshot.upload.listing_refusals, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn acceptance_5_single_day_listing_per_tick() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let file_name = "screen.mp4";
        // Day 1: epoch 1700000000 (2023-11-14)
        let boundary1 = 1_700_000_000;
        // Day 2: epoch 1700000000 + 86400 (2023-11-15)
        let boundary2 = boundary1 + 86400;
        let store = MultiSegmentStore::new(vec![
            (1, boundary1, file_name, b"day1".to_vec()),
            (2, boundary2, file_name, b"day2".to_vec()),
        ]);
        let client = FakeClient::new(
            vec![
                accepted_unconfirmed_ingest(1),
                accepted_unconfirmed_ingest(1),
            ],
            vec![empty_segments()],
        );
        let coordinator = coordinator_with_client(client.clone(), Box::new(store), sync.clone());

        // Both segments POSTed, exactly 1 listing performed for the first sorted day
        let confirmed = coordinator.tick().await.unwrap();
        assert_eq!(confirmed, 0);
        assert_eq!(client.lists.lock().unwrap().len(), 0);
    }

    #[test]
    fn acceptance_6_gate_deletion_verification() {
        let boundary = 1_700_000_100;
        let file_name = "screen.mp4";
        let bytes = b"correct payload".to_vec();
        let store = OneSegmentStore::new(boundary, file_name, bytes.clone());
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let coordinator = coordinator(Box::new(store), sync);

        let ack_file = AckFile {
            submitted: file_name.to_string(),
            written: file_name.to_string(),
            size: bytes.len() as u64,
            sha256: ca::sha256_hex(&bytes),
            disposition: Some("written".to_string()),
            listing_status: None,
        };

        // 1. Mismatched sha256 -> DeleteGate::Partial
        let mut bad_sha_ack = ack_file.clone();
        bad_sha_ack.sha256 =
            "0000000000000000000000000000000000000000000000000000000000000000".to_string();
        assert_eq!(
            coordinator.try_gate_delete(1, &[bad_sha_ack]).unwrap(),
            DeleteGate::Partial
        );

        // 2. Mismatched size -> DeleteGate::Partial
        let mut bad_size_ack = ack_file.clone();
        bad_size_ack.size = 999999;
        assert_eq!(
            coordinator.try_gate_delete(1, &[bad_size_ack]).unwrap(),
            DeleteGate::Partial
        );

        // 3. Invalid disposition -> DeleteGate::Partial
        let mut bad_disp_ack = ack_file.clone();
        bad_disp_ack.disposition = Some("received_not_written".to_string());
        assert_eq!(
            coordinator.try_gate_delete(1, &[bad_disp_ack]).unwrap(),
            DeleteGate::Partial
        );

        // 4. Exact match -> DeleteGate::Deleted and deletes
        assert_eq!(
            coordinator.try_gate_delete(1, &[ack_file]).unwrap(),
            DeleteGate::Deleted
        );
    }

    #[test]
    fn local_sealed_store_gate_tests() {
        let root = TestRoot::new("gate");
        let temp_path = root.path().to_path_buf();

        let store = crate::sealed::LocalSealedStore::new(&temp_path, 300);
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let coordinator = coordinator(Box::new(store), sync.clone());
        let day = "20231114";
        let segment_key = "1700000100_300";

        // 1. Matching receipt deletes
        let seg1 = temp_path.join("1");
        std::fs::create_dir_all(&seg1).unwrap();
        std::fs::write(seg1.join("screen.mp4"), b"matching bytes").unwrap();
        std::fs::write(seg1.join(observer_model::LEN_FILE_NAME), "300").unwrap();
        let desc = FileDescriptor {
            submitted: "screen.mp4".to_string(),
            written: "screen.mp4".to_string(),
            size: 14,
            sha256: ca::sha256_hex(b"matching bytes"),
            disposition: "written".to_string(),
        };
        let ack = UploadAck::new_upload(
            coordinator.client.journal_identity(),
            day,
            segment_key,
            segment_key,
            "ok",
            &[desc],
        );
        let _ = coordinator.store.write_ack(1, &ack);
        assert_eq!(
            coordinator.try_gate_delete(1, &ack.files).unwrap(),
            DeleteGate::Deleted
        );
        assert!(!seg1.exists());

        // 2. No ack retains
        let seg2 = temp_path.join("2");
        std::fs::create_dir_all(&seg2).unwrap();
        std::fs::write(seg2.join("screen.mp4"), b"bytes2").unwrap();
        assert_eq!(coordinator.store.read_ack(2).unwrap(), None);

        // 3. Unparsable ack retains
        let seg3 = temp_path.join("3");
        std::fs::create_dir_all(&seg3).unwrap();
        std::fs::write(seg3.join("screen.mp4"), b"bytes3").unwrap();
        std::fs::write(seg3.join(crate::sealed::UPLOADED_MARKER), b"not valid json").unwrap();
        assert_eq!(coordinator.store.read_ack(3).unwrap(), None);

        // 4. Empty .uploaded retains
        let seg4 = temp_path.join("4");
        std::fs::create_dir_all(&seg4).unwrap();
        std::fs::write(seg4.join("screen.mp4"), b"bytes4").unwrap();
        std::fs::write(seg4.join(crate::sealed::UPLOADED_MARKER), b"").unwrap();
        assert_eq!(coordinator.store.read_ack(4).unwrap(), None);

        // 5. Changed bytes retains media (Partial)
        let seg5 = temp_path.join("5");
        std::fs::create_dir_all(&seg5).unwrap();
        std::fs::write(seg5.join("screen.mp4"), b"changed bytes").unwrap();
        assert_eq!(
            coordinator.try_gate_delete(5, &ack.files).unwrap(),
            DeleteGate::Partial
        );
        assert!(seg5.exists());

        // 6. Extra file: covered media and the marker go, the extra file and .len stay (Partial)
        let seg6 = temp_path.join("6");
        std::fs::create_dir_all(&seg6).unwrap();
        std::fs::write(seg6.join("screen.mp4"), b"matching bytes").unwrap();
        std::fs::write(seg6.join("extra.mp4"), b"extra").unwrap();
        std::fs::write(seg6.join(observer_model::LEN_FILE_NAME), "297").unwrap();
        let _ = coordinator.store.write_ack(6, &ack);
        assert_eq!(
            coordinator.try_gate_delete(6, &ack.files).unwrap(),
            DeleteGate::Partial
        );
        assert!(!seg6.join("screen.mp4").exists());
        assert!(seg6.join("extra.mp4").exists());
        assert!(!seg6.join(crate::sealed::UPLOADED_MARKER).exists());
        assert_eq!(
            std::fs::read_to_string(seg6.join(observer_model::LEN_FILE_NAME)).unwrap(),
            "297"
        );

        // 7. Non-regular entry stops (Stopped)
        let seg7 = temp_path.join("7");
        std::fs::create_dir_all(&seg7).unwrap();
        std::fs::write(seg7.join("screen.mp4"), b"matching bytes").unwrap();
        std::fs::create_dir_all(seg7.join("subdir")).unwrap();
        assert_eq!(
            coordinator.try_gate_delete(7, &ack.files).unwrap(),
            DeleteGate::Stopped
        );
        assert!(seg7.exists());

        // 8. Foreign journal_identity retains
        let seg8 = temp_path.join("8");
        std::fs::create_dir_all(&seg8).unwrap();
        std::fs::write(seg8.join("screen.mp4"), b"matching bytes").unwrap();
        let foreign_desc = FileDescriptor {
            submitted: "screen.mp4".to_string(),
            written: "screen.mp4".to_string(),
            size: 14,
            sha256: ca::sha256_hex(b"matching bytes"),
            disposition: "written".to_string(),
        };
        let foreign_ack = UploadAck::new_upload(
            JournalIdentity {
                instance_id: "foreign_instance".to_string(),
                ca_fp_prefix: "1111111111111111".to_string(),
                client_cert_sha256:
                    "1111111111111111111111111111111111111111111111111111111111111111".to_string(),
            },
            day,
            segment_key,
            segment_key,
            "ok",
            &[foreign_desc],
        );
        let _ = coordinator.store.write_ack(8, &foreign_ack);
        let read_foreign = coordinator.store.read_ack(8).unwrap().unwrap();
        assert_ne!(
            read_foreign.journal_identity,
            coordinator.client.journal_identity()
        );

        // 9. Stale .uploaded.tmp with no .uploaded is not a POST part
        let seg9 = temp_path.join("9");
        std::fs::create_dir_all(&seg9).unwrap();
        std::fs::write(seg9.join("screen.mp4"), b"payload9").unwrap();
        std::fs::write(
            seg9.join(crate::sealed::UPLOADED_TMP_MARKER),
            b"tmp ack bytes",
        )
        .unwrap();
        let pending = coordinator.store.scan().unwrap();
        let s9 = pending.iter().find(|s| s.index == 9).unwrap();
        assert_eq!(s9.files, vec!["screen.mp4".to_string()]);

        // 10. Failed media delete returns Blocked
        let failing_store = MultiSegmentStore::new(vec![(
            10,
            1_700_000_100,
            "screen.mp4",
            b"matching bytes".to_vec(),
        )])
        .with_remove_fails_once(10);
        let failing_coord = coordinator_with_client(
            coordinator.client.clone(),
            Box::new(failing_store),
            sync.clone(),
        );
        let del_res = failing_coord.try_gate_delete(10, &ack.files).unwrap();
        assert_eq!(del_res, DeleteGate::Blocked);
    }

    #[test]
    fn local_finish_handles_all_marker_classes() {
        let root = TestRoot::new("lf");
        let temp_path = root.path().to_path_buf();

        let store = crate::sealed::LocalSealedStore::new(&temp_path, 300);
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let coordinator = coordinator(Box::new(store), sync.clone());
        let day = "20231114";
        let segment_key = "1700000100_300";

        // 1. Valid matching ack -> deleted by local_finish
        let seg1 = temp_path.join("1");
        std::fs::create_dir_all(&seg1).unwrap();
        std::fs::write(seg1.join("screen.mp4"), b"payload1").unwrap();
        std::fs::write(seg1.join(observer_model::LEN_FILE_NAME), "300").unwrap();
        let desc1 = FileDescriptor {
            submitted: "screen.mp4".to_string(),
            written: "screen.mp4".to_string(),
            size: 8,
            sha256: ca::sha256_hex(b"payload1"),
            disposition: "written".to_string(),
        };
        let ack1 = UploadAck::new_upload(
            coordinator.client.journal_identity(),
            day,
            segment_key,
            segment_key,
            "ok",
            &[desc1],
        );
        let _ = coordinator.store.write_ack(1, &ack1);

        // 2. Valid foreign ack -> .uploaded stripped, media and .len retained
        let seg2 = temp_path.join("2");
        std::fs::create_dir_all(&seg2).unwrap();
        std::fs::write(seg2.join("screen.mp4"), b"payload2").unwrap();
        std::fs::write(seg2.join(observer_model::LEN_FILE_NAME), "300").unwrap();
        let foreign_ack = UploadAck::new_upload(
            JournalIdentity {
                instance_id: "foreign_id".to_string(),
                ca_fp_prefix: "ffffffffffffffff".to_string(),
                client_cert_sha256:
                    "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff".to_string(),
            },
            day,
            segment_key,
            segment_key,
            "ok",
            &[FileDescriptor {
                submitted: "screen.mp4".to_string(),
                written: "screen.mp4".to_string(),
                size: 8,
                sha256: ca::sha256_hex(b"payload2"),
                disposition: "written".to_string(),
            }],
        );
        let _ = coordinator.store.write_ack(2, &foreign_ack);

        // 3. Corrupted .uploaded marker with older media (media mtime <= marker mtime) -> removed completely
        let seg3 = temp_path.join("3");
        std::fs::create_dir_all(&seg3).unwrap();
        let media3 = seg3.join("screen.mp4");
        std::fs::write(&media3, b"payload3").unwrap();
        let f3_media = std::fs::OpenOptions::new()
            .write(true)
            .open(&media3)
            .unwrap();
        f3_media
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1000))
            .unwrap();
        let marker3 = seg3.join(crate::sealed::UPLOADED_MARKER);
        std::fs::write(&marker3, b"corrupted json 3").unwrap();
        let f3_marker = std::fs::OpenOptions::new()
            .write(true)
            .open(&marker3)
            .unwrap();
        f3_marker
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(2000))
            .unwrap();

        // 4. Corrupted .uploaded marker with newer media (media mtime > marker mtime) -> newer media retained, sidecars stripped, dir retained
        let seg4 = temp_path.join("4");
        std::fs::create_dir_all(&seg4).unwrap();
        let media4 = seg4.join("screen.mp4");
        std::fs::write(&media4, b"payload4").unwrap();
        let f4_media = std::fs::OpenOptions::new()
            .write(true)
            .open(&media4)
            .unwrap();
        f4_media
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(3000))
            .unwrap();
        let marker4 = seg4.join(crate::sealed::UPLOADED_MARKER);
        std::fs::write(&marker4, b"corrupted json 4").unwrap();
        let f4_marker = std::fs::OpenOptions::new()
            .write(true)
            .open(&marker4)
            .unwrap();
        f4_marker
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(2000))
            .unwrap();

        // 5. Empty .uploaded marker with older media -> media removed, sidecars removed, dir removed
        let seg5 = temp_path.join("5");
        std::fs::create_dir_all(&seg5).unwrap();
        let media5 = seg5.join("screen.mp4");
        std::fs::write(&media5, b"payload5").unwrap();
        let f5_media = std::fs::OpenOptions::new()
            .write(true)
            .open(&media5)
            .unwrap();
        f5_media
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1000))
            .unwrap();
        let marker5 = seg5.join(crate::sealed::UPLOADED_MARKER);
        std::fs::write(&marker5, b"").unwrap();
        let f5_marker = std::fs::OpenOptions::new()
            .write(true)
            .open(&marker5)
            .unwrap();
        f5_marker
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(2000))
            .unwrap();

        // 6. Empty .uploaded marker with newer media -> newer media retained, sidecars stripped, dir retained
        let seg6 = temp_path.join("6");
        std::fs::create_dir_all(&seg6).unwrap();
        let media6 = seg6.join("screen.mp4");
        std::fs::write(&media6, b"payload6").unwrap();
        let f6_media = std::fs::OpenOptions::new()
            .write(true)
            .open(&media6)
            .unwrap();
        f6_media
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(3000))
            .unwrap();
        let marker6 = seg6.join(crate::sealed::UPLOADED_MARKER);
        std::fs::write(&marker6, b"").unwrap();
        let f6_marker = std::fs::OpenOptions::new()
            .write(true)
            .open(&marker6)
            .unwrap();
        f6_marker
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(2000))
            .unwrap();

        // 7. 0-media directory with valid ack -> sidecars and dir removed
        let seg7 = temp_path.join("7");
        std::fs::create_dir_all(&seg7).unwrap();
        std::fs::write(seg7.join(observer_model::LEN_FILE_NAME), "300").unwrap();
        let ack7 = UploadAck::new_upload(
            coordinator.client.journal_identity(),
            day,
            segment_key,
            segment_key,
            "ok",
            &[],
        );
        let _ = coordinator.store.write_ack(7, &ack7);

        // 8. 0-media directory with no ack or unparsable marker -> sidecars and dir removed
        let seg8 = temp_path.join("8");
        std::fs::create_dir_all(&seg8).unwrap();
        std::fs::write(seg8.join(observer_model::LEN_FILE_NAME), "300").unwrap();
        std::fs::write(seg8.join(crate::sealed::UPLOADED_MARKER), b"bad json").unwrap();

        // 9. No .uploaded marker with media -> unaffected
        let seg9 = temp_path.join("9");
        std::fs::create_dir_all(&seg9).unwrap();
        std::fs::write(seg9.join("screen.mp4"), b"payload9").unwrap();
        std::fs::write(seg9.join(observer_model::LEN_FILE_NAME), "300").unwrap();

        coordinator.local_finish(now_epoch_secs());

        // 1. Valid matching deleted
        assert!(!seg1.exists());

        // 2. Foreign ack stripped, media & .len retained
        assert!(seg2.exists());
        assert!(!seg2.join(crate::sealed::UPLOADED_MARKER).exists());
        assert!(seg2.join("screen.mp4").exists());
        assert!(seg2.join(observer_model::LEN_FILE_NAME).exists());

        // 3. Corrupted marker older media deleted
        assert!(!seg3.exists());

        // 4. Corrupted marker newer media retained, sidecars stripped, dir retained
        assert!(seg4.exists());
        assert!(seg4.join("screen.mp4").exists());
        assert!(!seg4.join(crate::sealed::UPLOADED_MARKER).exists());

        // 5. Empty marker older media deleted
        assert!(!seg5.exists());

        // 6. Empty marker newer media retained, sidecars stripped, dir retained
        assert!(seg6.exists());
        assert!(seg6.join("screen.mp4").exists());
        assert!(!seg6.join(crate::sealed::UPLOADED_MARKER).exists());

        // 7. 0-media with valid ack removed
        assert!(!seg7.exists());

        // 8. 0-media with unparsable marker removed
        assert!(!seg8.exists());

        // 9. No .uploaded marker with media unaffected
        assert!(seg9.exists());
        assert!(seg9.join("screen.mp4").exists());
        assert!(seg9.join(observer_model::LEN_FILE_NAME).exists());
    }

    #[tokio::test]
    async fn start_of_tick_quarantined_recount() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let store =
            MultiSegmentStore::new(vec![(1, 1_700_000_100, "screen.mp4", b"data".to_vec())]);
        store.quarantine(1).unwrap();
        let coordinator = coordinator(Box::new(store), sync.clone());

        let res = coordinator.tick().await.unwrap();
        assert_eq!(res, 0);
        let snapshot = sync.lock().unwrap().clone();
        assert_eq!(snapshot.upload.quarantined_segments, 1);
        assert_eq!(snapshot.upload.pending_segments, 0);
    }

    #[tokio::test]
    async fn held_indices_excluded_from_pending_count() {
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let boundary = 1_700_000_100;
        let file_name = "screen.mp4";
        let bytes = b"second".to_vec();
        let key2 = civil::segment_key_string_local(boundary + 300, 0, 300);
        let store = MultiSegmentStore::new(vec![
            (1, boundary, "screen.mp4", b"first".to_vec()),
            (2, boundary + 300, file_name, bytes.clone()),
        ]);
        let client = FakeClient::new(vec![accepted_ingest(&key2, file_name, &bytes, 1)], vec![]);
        let coordinator = coordinator_with_client(client, Box::new(store), sync.clone());
        coordinator.set_hold(1, now_epoch_secs() + 3600);

        let res = coordinator.tick().await.unwrap();
        assert_eq!(res, 1);
        let snapshot = sync.lock().unwrap().clone();
        // Index 1 was held and skipped, index 2 was processed, so pending_segments is now 0
        assert_eq!(snapshot.upload.pending_segments, 0);
        assert_eq!(snapshot.upload.uploaded_segments, 1);
    }

    #[tokio::test]
    async fn partial_gate_keeps_len_and_the_next_tick_posts_the_extra_file_under_the_original_key()
    {
        let root = TestRoot::new("partial");
        let boundary = TEST_INDEX * 300;
        let dir = root.path().join(TEST_INDEX.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("screen.mp4"), b"covered").unwrap();
        std::fs::write(dir.join("extra.mp4"), b"extra").unwrap();
        std::fs::write(dir.join(observer_model::LEN_FILE_NAME), "297").unwrap();
        write_marker(
            &dir,
            test_identity(
                "test",
                "0000000000000000000000000000000000000000000000000000000000000000",
            ),
            &[written_descriptor("screen.mp4", b"covered")],
        );

        let key = civil::segment_key_string_local(boundary, 0, 297);
        assert_ne!(key, civil::segment_key_string_local(boundary, 0, 300));
        let client = FakeClient::new(
            vec![accepted_ingest(&key, "extra.mp4", b"extra", 1)],
            vec![],
        );
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let coordinator = coordinator_with_client(
            client.clone(),
            Box::new(crate::sealed::LocalSealedStore::new(root.path(), 300)),
            sync.clone(),
        );

        coordinator.local_finish(coordinator.monotonic_now_epoch_secs());
        assert!(!dir.join("screen.mp4").exists());
        assert!(dir.join("extra.mp4").exists());
        assert!(!dir.join(UPLOADED_MARKER).exists());
        assert!(!dir.join(UPLOADED_TMP_MARKER).exists());
        assert_eq!(
            std::fs::read_to_string(dir.join(observer_model::LEN_FILE_NAME)).unwrap(),
            "297"
        );
        assert!(client.posts.lock().unwrap().is_empty());

        assert_eq!(coordinator.tick().await.unwrap(), 1);
        assert_eq!(
            *client.posts.lock().unwrap(),
            vec![(key, vec!["extra.mp4".to_string()])]
        );
        assert!(!dir.exists());
    }

    #[tokio::test]
    async fn markerless_dirs_without_media_are_removed_and_never_pending() {
        let root = TestRoot::new("nomedia");
        let len_only = root.path().join("1");
        std::fs::create_dir_all(&len_only).unwrap();
        std::fs::write(len_only.join(observer_model::LEN_FILE_NAME), "300").unwrap();
        let empty = root.path().join("2");
        std::fs::create_dir_all(&empty).unwrap();

        let client = RefusingClient::new();
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let coordinator = coordinator_with_client(
            client.clone(),
            Box::new(crate::sealed::LocalSealedStore::new(root.path(), 300)),
            sync.clone(),
        );

        assert_eq!(coordinator.tick().await.unwrap(), 0);
        assert!(!len_only.exists());
        assert!(!empty.exists());
        assert_eq!(sync.lock().unwrap().upload.pending_segments, 0);
        assert_eq!(client.calls(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn markerless_dirs_without_media_that_cannot_be_removed_stay_out_of_pending() {
        let root = TestRoot::new("nomedia-held");
        let len_only = root.path().join("1");
        std::fs::create_dir_all(&len_only).unwrap();
        std::fs::write(len_only.join(observer_model::LEN_FILE_NAME), "300").unwrap();
        let empty = root.path().join("2");
        std::fs::create_dir_all(&empty).unwrap();

        let mut store = FaultStore::new(root.path());
        store.fail_remove_dir = true;
        let client = RefusingClient::new();
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let coordinator = coordinator_with_client(client.clone(), Box::new(store), sync.clone());

        for _ in 0..2 {
            assert_eq!(coordinator.tick().await.unwrap(), 0);
            assert!(len_only.exists());
            assert!(empty.exists());
            // Both directories are still scannable, yet neither counts as pending.
            assert_eq!(coordinator.store.scan().unwrap().len(), 2);
            let now = coordinator.monotonic_now_epoch_secs();
            assert!(coordinator.is_held(1, now));
            assert!(coordinator.is_held(2, now));
            assert_eq!(sync.lock().unwrap().upload.pending_segments, 0);
            tokio::time::advance(Duration::from_secs(1800)).await;
        }
        assert_eq!(client.calls(), 0);
    }

    #[tokio::test]
    async fn same_instance_ack_with_a_new_client_certificate_finishes_locally_without_the_journal()
    {
        let root = TestRoot::new("newcert");
        let dir = root.path().join(TEST_INDEX.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("screen.mp4"), b"payload").unwrap();
        std::fs::write(dir.join(observer_model::LEN_FILE_NAME), "300").unwrap();
        write_marker(
            &dir,
            test_identity(
                "test",
                "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            ),
            &[written_descriptor("screen.mp4", b"payload")],
        );

        let client = RefusingClient::new();
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let coordinator = coordinator_with_client(
            client.clone(),
            Box::new(crate::sealed::LocalSealedStore::new(root.path(), 300)),
            sync.clone(),
        );

        assert_eq!(coordinator.tick().await.unwrap(), 0);
        assert!(!dir.exists());
        assert_eq!(client.calls(), 0);
        assert_eq!(sync.lock().unwrap().upload.pending_segments, 0);
    }

    #[tokio::test]
    async fn foreign_instance_ack_is_resent_to_the_current_journal_and_removed_on_its_receipt() {
        let root = TestRoot::new("foreign");
        let boundary = TEST_INDEX * 300;
        let dir = root.path().join(TEST_INDEX.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("screen.mp4"), b"payload").unwrap();
        std::fs::write(dir.join(observer_model::LEN_FILE_NAME), "300").unwrap();
        write_marker(
            &dir,
            test_identity(
                "foreign_instance",
                "1111111111111111111111111111111111111111111111111111111111111111",
            ),
            &[written_descriptor("screen.mp4", b"payload")],
        );

        let key = civil::segment_key_string_local(boundary, 0, 300);
        let client = FakeClient::new(
            vec![accepted_ingest(&key, "screen.mp4", b"payload", 1)],
            vec![],
        );
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let coordinator = coordinator_with_client(
            client.clone(),
            Box::new(crate::sealed::LocalSealedStore::new(root.path(), 300)),
            sync.clone(),
        );

        coordinator.local_finish(coordinator.monotonic_now_epoch_secs());
        assert!(!dir.join(UPLOADED_MARKER).exists());
        assert!(dir.join("screen.mp4").exists());
        assert!(dir.join(observer_model::LEN_FILE_NAME).exists());
        assert!(client.posts.lock().unwrap().is_empty());

        assert_eq!(coordinator.tick().await.unwrap(), 1);
        assert_eq!(
            *client.posts.lock().unwrap(),
            vec![(key, vec!["screen.mp4".to_string()])]
        );
        assert!(!dir.exists());
    }

    #[test]
    fn empty_marker_with_newer_media_leaves_that_file_pending() {
        let root = TestRoot::new("emptymarker");
        let dir = root.path().join(TEST_INDEX.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let media = dir.join("screen.mp4");
        std::fs::write(&media, b"payload").unwrap();
        set_mtime(&media, 3000);
        let marker = dir.join(UPLOADED_MARKER);
        std::fs::write(&marker, b"").unwrap();
        set_mtime(&marker, 2000);

        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let coordinator = coordinator_with_client(
            RefusingClient::new(),
            Box::new(crate::sealed::LocalSealedStore::new(root.path(), 300)),
            sync,
        );
        assert!(coordinator.store.scan().unwrap().is_empty());

        coordinator.local_finish(coordinator.monotonic_now_epoch_secs());
        let pending = coordinator.store.scan().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].index, TEST_INDEX);
        assert_eq!(pending[0].files, vec!["screen.mp4".to_string()]);
    }

    #[tokio::test]
    async fn gate_io_errors_hold_the_segment_for_an_hour() {
        // Local finish: the gate cannot list the directory.
        {
            let root = TestRoot::new("gate-err-lf");
            let dir = root.path().join("1");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("screen.mp4"), b"payload").unwrap();
            write_marker(
                &dir,
                test_identity(
                    "test",
                    "0000000000000000000000000000000000000000000000000000000000000000",
                ),
                &[written_descriptor("screen.mp4", b"payload")],
            );
            let mut store = FaultStore::new(root.path());
            store.fail_list_entries = true;
            let coordinator = coordinator_with_client(
                RefusingClient::new(),
                Box::new(store),
                Arc::new(Mutex::new(SyncSnapshot::default())),
            );
            let now = coordinator.monotonic_now_epoch_secs();
            coordinator.local_finish(now);
            assert!(coordinator.is_held(1, now));
            assert!(dir.join("screen.mp4").exists());
        }

        // Local finish: a foreign marker cannot be removed.
        {
            let root = TestRoot::new("gate-err-foreign");
            let dir = root.path().join("1");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("screen.mp4"), b"payload").unwrap();
            write_marker(
                &dir,
                test_identity(
                    "foreign_instance",
                    "1111111111111111111111111111111111111111111111111111111111111111",
                ),
                &[written_descriptor("screen.mp4", b"payload")],
            );
            let mut store = FaultStore::new(root.path());
            store.fail_remove_marker = true;
            let coordinator = coordinator_with_client(
                RefusingClient::new(),
                Box::new(store),
                Arc::new(Mutex::new(SyncSnapshot::default())),
            );
            let now = coordinator.monotonic_now_epoch_secs();
            coordinator.local_finish(now);
            assert!(coordinator.is_held(1, now));
            assert!(dir.join(UPLOADED_MARKER).exists());
        }

        // Tick: the receipt is recorded but the gate cannot list the directory.
        {
            let root = TestRoot::new("gate-err-tick");
            let boundary = TEST_INDEX * 300;
            let dir = root.path().join(TEST_INDEX.to_string());
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("screen.mp4"), b"payload").unwrap();
            let key = civil::segment_key_string_local(boundary, 0, 300);
            let client = FakeClient::new(
                vec![accepted_ingest(&key, "screen.mp4", b"payload", 1)],
                vec![],
            );
            let mut store = FaultStore::new(root.path());
            store.fail_list_entries = true;
            let coordinator = coordinator_with_client(
                client.clone(),
                Box::new(store),
                Arc::new(Mutex::new(SyncSnapshot::default())),
            );
            assert_eq!(coordinator.tick().await.unwrap(), 1);
            assert!(coordinator.is_held(TEST_INDEX, coordinator.monotonic_now_epoch_secs()));
            assert!(dir.join(UPLOADED_MARKER).exists());
            assert!(dir.join("screen.mp4").exists());
        }
    }
}
