// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Streaming native journal export download manager for Windows.
//!
//! Downloads ready journal export archives through the shared SPL journal
//! bridge into a sibling `.solstone-export-<id>.part` file, supporting 64-bit
//! `Range` + `If-Range` resume, LAN-to-relay pause, system sleep pause,
//! power-hold management, full-file SHA-256 verification, and atomic rename
//! so preexisting destination files are never clobbered on cancel or failure.

use std::fs::{self, File, OpenOptions};
use std::future::Future;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use spl_core::bridge::BridgeNames;
use spl_core::http;
use spl_transport::journal_bridge::{self, BridgePolicy, CarrierOpener, JournalBridgeConfig};
use spl_transport::observe::OperationObserver;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;

use crate::access::CredentialAccess;
use crate::journal_bridge::is_export_download_path;
use crate::ObserverClient;

const PLATFORM: &str = "windows";
const HEAD_CAP: usize = 32 * 1024;
const MAX_ERROR_BODY_BYTES: usize = 8 * 1024;
const CHUNK_READ_SIZE: usize = 64 * 1024;
const VERIFY_CHUNK_BYTES: usize = 256 * 1024;
const OPEN_TIMEOUT: Duration = Duration::from_secs(60);
const READ_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExportOpenError {
    InvalidPath,
    Forbidden,
    TransportUnavailable,
}

pub struct OpenedExportStream {
    pub status: u16,
    pub content_range: Option<String>,
    pub etag: Option<String>,
    pub transport: &'static str,
    pub body: Box<dyn ExportByteStream>,
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub type ClientLoader =
    Arc<dyn Fn() -> BoxFuture<'static, Option<Arc<ObserverClient>>> + Send + Sync>;

pub trait ExportByteStream: Send {
    fn next_chunk<'a>(&'a mut self) -> BoxFuture<'a, Result<Option<Vec<u8>>, ()>>;

    fn read_bounded<'a>(&'a mut self, max_bytes: usize) -> BoxFuture<'a, Vec<u8>>;
}

pub trait ExportStreamOpener: Send + Sync {
    fn current_binding<'a>(&'a self) -> BoxFuture<'a, Option<String>>;

    fn open<'a>(
        &'a self,
        expected_binding: &'a str,
        download_path: &'a str,
        range_header: Option<String>,
        if_range_header: Option<String>,
    ) -> BoxFuture<'a, Result<OpenedExportStream, ExportOpenError>>;
}

pub type SpaceProbe = Arc<dyn Fn(&Path) -> Option<u64> + Send + Sync>;
pub type ReplaceFileFn = Arc<dyn Fn(&Path, &Path) -> io::Result<()> + Send + Sync>;
pub type PowerHoldGuard = Box<dyn Send + Sync>;
pub type PowerHoldFactory = Arc<dyn Fn() -> Option<PowerHoldGuard> + Send + Sync>;
#[cfg(test)]
type VerifyChunkHook = Arc<dyn Fn(u64) + Send + Sync>;

/// Parameters for starting a native journal export save.
#[derive(Clone, Debug)]
pub struct StartExportParams {
    pub request_id: String,
    pub export_id: String,
    pub download_path: String,
    pub filename: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub etag: String,
    pub destination_path: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExportStatusSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub export_id: Option<String>,
    pub phase: String,
    pub bytes_written: u64,
    pub total_bytes: u64,
    pub destination_display: Option<String>,
    pub pause_reason: Option<String>,
    pub error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_bytes: Option<u64>,
    pub can_reveal: bool,
    pub verified: bool,
    pub platform: &'static str,
}

impl ExportStatusSnapshot {
    pub fn idle(id: Option<String>) -> Self {
        Self {
            id,
            export_id: None,
            phase: "idle".to_owned(),
            bytes_written: 0,
            total_bytes: 0,
            destination_display: None,
            pause_reason: None,
            error_code: None,
            required_bytes: None,
            available_bytes: None,
            can_reveal: false,
            verified: false,
            platform: PLATFORM,
        }
    }

    pub fn with_id(&self, id: Option<String>) -> Self {
        let mut copy = self.clone();
        copy.id = id;
        copy
    }

    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| {
            json!({
                "phase": "error",
                "error_code": "serialization_error",
                "platform": PLATFORM,
            })
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ControlSignal {
    Run,
    PauseOwner,
    PauseSleep,
    Cancel,
}

struct ActiveTransfer {
    generation: u64,
    export_id: String,
    download_path: String,
    size_bytes: u64,
    sha256: String,
    etag: String,
    destination_path: PathBuf,
    part_path: PathBuf,
    destination_display: String,
    binding: String,
    initial_transport: Option<&'static str>,
    allow_relay: bool,
    control_tx: watch::Sender<ControlSignal>,
    cancel_flag: Arc<AtomicBool>,
}

struct ExportState {
    next_generation: u64,
    snapshot: ExportStatusSnapshot,
    active: Option<ActiveTransfer>,
    saved_export_id: Option<String>,
    saved_destination: Option<PathBuf>,
    saved_display: Option<String>,
}

impl Default for ExportState {
    fn default() -> Self {
        Self {
            next_generation: 0,
            snapshot: ExportStatusSnapshot::idle(None),
            active: None,
            saved_export_id: None,
            saved_destination: None,
            saved_display: None,
        }
    }
}

struct ExportInner {
    opener: Arc<dyn ExportStreamOpener>,
    space_probe: SpaceProbe,
    replace_file: ReplaceFileFn,
    power_hold: PowerHoldFactory,
    /// Serializes destination publication (`replace_file` + `saved` commit)
    /// against `cancel()` / `cancel_for_unpair()` so a cancelled verification
    /// can never publish a stale file over a newer attempt or preexisting target.
    /// Lock order is strictly `publish_lock` -> `state`.
    publish_lock: Mutex<()>,
    state: Mutex<ExportState>,
    #[cfg(test)]
    verify_chunk_hook: Mutex<Option<VerifyChunkHook>>,
}

#[derive(Clone)]
pub struct ExportTransferManager {
    inner: Arc<ExportInner>,
}

impl ExportTransferManager {
    pub fn new(
        opener: Arc<dyn ExportStreamOpener>,
        space_probe: SpaceProbe,
        replace_file: ReplaceFileFn,
        power_hold: PowerHoldFactory,
    ) -> Self {
        Self {
            inner: Arc::new(ExportInner {
                opener,
                space_probe,
                replace_file,
                power_hold,
                publish_lock: Mutex::new(()),
                state: Mutex::new(ExportState::default()),
                #[cfg(test)]
                verify_chunk_hook: Mutex::new(None),
            }),
        }
    }

    #[cfg(test)]
    fn set_verify_chunk_hook(&self, hook: VerifyChunkHook) {
        *self
            .inner
            .verify_chunk_hook
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(hook);
    }

    pub async fn start(&self, req: StartExportParams) -> ExportStatusSnapshot {
        if !valid_export_id(&req.export_id)
            || req.download_path != format!("/app/export/api/ready/{}/download", req.export_id)
            || !is_export_download_path(&req.download_path)
            || !valid_sha256_hex(&req.sha256)
            || req.size_bytes == 0
            || req.etag.trim().is_empty()
        {
            return failed_snapshot(Some(req.request_id), Some(req.export_id), "invalid_request");
        }
        let Some(parent) = req.destination_path.parent() else {
            return failed_snapshot(
                Some(req.request_id),
                Some(req.export_id),
                "invalid_destination",
            );
        };
        if req.destination_path.file_name().is_none()
            || req.destination_path.is_dir()
            || !parent.is_dir()
        {
            return failed_snapshot(
                Some(req.request_id),
                Some(req.export_id),
                "invalid_destination",
            );
        }

        let destination_display = req
            .destination_path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_owned)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| req.filename.clone());

        let discovered_part = {
            let state = self.lock_state();
            if state
                .active
                .as_ref()
                .is_some_and(|existing| existing.export_id == req.export_id)
            {
                state.active.as_ref().and_then(|existing| {
                    fs::symlink_metadata(&existing.part_path)
                        .ok()
                        .filter(|meta| meta.file_type().is_file() && meta.len() <= req.size_bytes)
                        .map(|meta| (existing.part_path.clone(), meta.len()))
                })
            } else {
                discover_existing_part_file(parent, &req.export_id, req.size_bytes)
            }
        };
        let existing_part_len = discovered_part.as_ref().map(|(_, len)| *len).unwrap_or(0);
        let remaining_needed = req.size_bytes.saturating_sub(existing_part_len);
        let required = required_device_bytes(remaining_needed);
        if let Some(available) = (self.inner.space_probe)(parent) {
            if available < required {
                let mut fail = failed_snapshot(
                    Some(req.request_id),
                    Some(req.export_id),
                    "insufficient_device_space",
                );
                fail.total_bytes = req.size_bytes;
                fail.required_bytes = Some(required);
                fail.available_bytes = Some(available);
                fail.destination_display = Some(destination_display);
                return fail;
            }
        }

        let Some(binding) = self.inner.opener.current_binding().await else {
            return failed_snapshot(
                Some(req.request_id),
                Some(req.export_id),
                "export_requester_revoked",
            );
        };

        let (generation, reply) = {
            let mut state = self.lock_state();
            if let Some(existing) = &state.active {
                if matches!(state.snapshot.phase.as_str(), "transferring" | "verifying") {
                    if existing.export_id == req.export_id {
                        return state.snapshot.with_id(Some(req.request_id));
                    }
                    return failed_snapshot(
                        Some(req.request_id),
                        Some(req.export_id),
                        "export_already_running",
                    );
                }
            }
            let reused_source = if let Some(prev) = state.active.take() {
                if prev.export_id != req.export_id {
                    prev.cancel_flag.store(true, Ordering::Release);
                    let _ = prev.control_tx.send(ControlSignal::Cancel);
                    let _ = fs::remove_file(&prev.part_path);
                    discovered_part.map(|(p, _)| p)
                } else {
                    Some(prev.part_path)
                }
            } else {
                discovered_part.map(|(p, _)| p)
            };
            state.next_generation = state.next_generation.wrapping_add(1);
            let generation = state.next_generation;
            let part_path = sibling_part_path_for_generation(parent, &req.export_id, generation);
            let mut adopted_len = 0u64;
            if let Some(src_part) = reused_source {
                if src_part == part_path {
                    adopted_len = fs::symlink_metadata(&part_path)
                        .ok()
                        .filter(|m| m.file_type().is_file() && m.len() <= req.size_bytes)
                        .map(|m| m.len())
                        .unwrap_or(0);
                } else if fs::symlink_metadata(&src_part)
                    .ok()
                    .is_some_and(|m| m.file_type().is_file() && m.len() <= req.size_bytes)
                    && fs::rename(&src_part, &part_path).is_ok()
                {
                    adopted_len = fs::symlink_metadata(&part_path)
                        .ok()
                        .filter(|m| m.file_type().is_file() && m.len() <= req.size_bytes)
                        .map(|m| m.len())
                        .unwrap_or(0);
                }
            }
            let (control_tx, _) = watch::channel(ControlSignal::Run);
            let cancel_flag = Arc::new(AtomicBool::new(false));
            state.active = Some(ActiveTransfer {
                generation,
                export_id: req.export_id.clone(),
                download_path: req.download_path,
                size_bytes: req.size_bytes,
                sha256: req.sha256.to_ascii_lowercase(),
                etag: req.etag,
                destination_path: req.destination_path,
                part_path,
                destination_display: destination_display.clone(),
                binding,
                initial_transport: None,
                allow_relay: false,
                control_tx,
                cancel_flag,
            });
            state.snapshot = ExportStatusSnapshot {
                id: None,
                export_id: Some(req.export_id),
                phase: "transferring".to_owned(),
                bytes_written: adopted_len.min(req.size_bytes),
                total_bytes: req.size_bytes,
                destination_display: Some(destination_display),
                pause_reason: None,
                error_code: None,
                required_bytes: None,
                available_bytes: None,
                can_reveal: false,
                verified: false,
                platform: PLATFORM,
            };
            (generation, state.snapshot.with_id(Some(req.request_id)))
        };

        self.spawn_transfer_attempt(generation);
        reply
    }

    pub fn status(&self, request_id: &str, export_id: Option<&str>) -> ExportStatusSnapshot {
        let state = self.lock_state();
        if let Some(wanted) = export_id {
            if state.snapshot.export_id.as_deref() != Some(wanted) {
                return ExportStatusSnapshot::idle(Some(request_id.to_owned()));
            }
        }
        state.snapshot.with_id(Some(request_id.to_owned()))
    }

    pub fn pause(&self, request_id: &str, export_id: Option<&str>) -> ExportStatusSnapshot {
        let mut state = self.lock_state();
        if let Some(active) = &state.active {
            if export_id.is_none_or(|id| id == active.export_id)
                && matches!(state.snapshot.phase.as_str(), "transferring" | "paused")
            {
                let _ = active.control_tx.send(ControlSignal::PauseOwner);
                state.snapshot.phase = "paused".to_owned();
                state.snapshot.pause_reason = Some("owner".to_owned());
                state.snapshot.error_code = None;
            }
        }
        state.snapshot.with_id(Some(request_id.to_owned()))
    }

    pub fn pause_for_sleep(&self) {
        let mut state = self.lock_state();
        if let Some(active) = &state.active {
            if state.snapshot.phase == "transferring" {
                let _ = active.control_tx.send(ControlSignal::PauseSleep);
                state.snapshot.phase = "paused".to_owned();
                state.snapshot.pause_reason = Some("sleep".to_owned());
                state.snapshot.error_code = None;
            }
        }
    }

    pub fn resume(&self, request_id: &str, export_id: Option<&str>) -> ExportStatusSnapshot {
        let (spawn_gen, reply) = {
            let mut state = self.lock_state();
            let can_resume = state.active.as_ref().is_some_and(|active| {
                export_id.is_none_or(|id| id == active.export_id)
                    && state.snapshot.phase == "paused"
            });
            if can_resume {
                let was_relay_pause =
                    state.snapshot.pause_reason.as_deref() == Some("relay_switched");
                let part_len = state
                    .active
                    .as_ref()
                    .and_then(|a| fs::symlink_metadata(&a.part_path).ok())
                    .filter(|m| m.file_type().is_file())
                    .map(|m| m.len())
                    .unwrap_or(state.snapshot.bytes_written);
                let next_gen = state.next_generation.wrapping_add(1);
                state.next_generation = next_gen;
                let active_size = state.active.as_mut().map(|active| {
                    active.generation = next_gen;
                    if was_relay_pause {
                        active.allow_relay = true;
                        active.initial_transport = Some("relay");
                    }
                    let (control_tx, _) = watch::channel(ControlSignal::Run);
                    active.control_tx = control_tx;
                    active.cancel_flag = Arc::new(AtomicBool::new(false));
                    active.size_bytes
                });
                if let Some(size_bytes) = active_size {
                    state.snapshot.phase = "transferring".to_owned();
                    state.snapshot.bytes_written = part_len.min(size_bytes);
                    state.snapshot.pause_reason = None;
                    state.snapshot.error_code = None;
                }
                (
                    Some(next_gen),
                    state.snapshot.with_id(Some(request_id.to_owned())),
                )
            } else {
                (None, state.snapshot.with_id(Some(request_id.to_owned())))
            }
        };
        if let Some(generation) = spawn_gen {
            self.spawn_transfer_attempt(generation);
        }
        reply
    }

    pub fn cancel(&self, request_id: &str, export_id: Option<&str>) -> ExportStatusSnapshot {
        let _publish_guard = self.lock_publish();
        let (part_to_remove, reply) = {
            let mut state = self.lock_state();
            let matches_active = state
                .active
                .as_ref()
                .is_some_and(|active| export_id.is_none_or(|id| id == active.export_id));
            if matches_active {
                let active = state.active.take().expect("active checked");
                active.cancel_flag.store(true, Ordering::Release);
                let _ = active.control_tx.send(ControlSignal::Cancel);
                state.snapshot = ExportStatusSnapshot {
                    id: None,
                    export_id: Some(active.export_id),
                    phase: "cancelled".to_owned(),
                    bytes_written: 0,
                    total_bytes: active.size_bytes,
                    destination_display: Some(active.destination_display),
                    pause_reason: None,
                    error_code: None,
                    required_bytes: None,
                    available_bytes: None,
                    can_reveal: false,
                    verified: false,
                    platform: PLATFORM,
                };
                (
                    Some(active.part_path),
                    state.snapshot.with_id(Some(request_id.to_owned())),
                )
            } else {
                (None, state.snapshot.with_id(Some(request_id.to_owned())))
            }
        };
        if let Some(part_path) = part_to_remove {
            let _ = fs::remove_file(part_path);
        }
        reply
    }

    pub fn cancel_for_unpair(&self) {
        let _publish_guard = self.lock_publish();
        let part_to_remove = {
            let mut state = self.lock_state();
            state.saved_export_id = None;
            state.saved_destination = None;
            state.saved_display = None;
            if let Some(active) = state.active.take() {
                active.cancel_flag.store(true, Ordering::Release);
                let _ = active.control_tx.send(ControlSignal::Cancel);
                state.snapshot = ExportStatusSnapshot {
                    id: None,
                    export_id: Some(active.export_id),
                    phase: "cancelled".to_owned(),
                    bytes_written: 0,
                    total_bytes: active.size_bytes,
                    destination_display: Some(active.destination_display),
                    pause_reason: None,
                    error_code: None,
                    required_bytes: None,
                    available_bytes: None,
                    can_reveal: false,
                    verified: false,
                    platform: PLATFORM,
                };
                Some(active.part_path)
            } else {
                state.snapshot = ExportStatusSnapshot::idle(None);
                None
            }
        };
        if let Some(part) = part_to_remove {
            let _ = fs::remove_file(part);
        }
    }

    pub fn saved_destination(&self, export_id: Option<&str>) -> Option<(PathBuf, Option<String>)> {
        let state = self.lock_state();
        let matches = export_id.is_none_or(|id| state.saved_export_id.as_deref() == Some(id));
        if matches {
            state
                .saved_destination
                .clone()
                .map(|p| (p, state.saved_display.clone()))
        } else {
            None
        }
    }

    pub fn available_space(&self, probe_dir: &Path) -> Option<u64> {
        (self.inner.space_probe)(probe_dir)
    }

    fn lock_publish(&self) -> std::sync::MutexGuard<'_, ()> {
        self.inner
            .publish_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, ExportState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn spawn_transfer_attempt(&self, generation: u64) {
        let handle = self.clone();
        tokio::spawn(async move {
            handle.run_transfer_attempt(generation).await;
        });
    }

    async fn run_transfer_attempt(&self, generation: u64) {
        let Some(params) = self.snapshot_active_params(generation) else {
            return;
        };

        let _power_hold = (self.inner.power_hold)();
        let mut control_rx = params.control_rx.clone();
        let mut reset_and_retry_once = false;

        loop {
            if self.check_pairing_lost(generation, &params.binding).await {
                return;
            }
            let control_now = *control_rx.borrow();
            match control_now {
                ControlSignal::Cancel => {
                    let _ = fs::remove_file(&params.part_path);
                    return;
                }
                ControlSignal::PauseOwner => {
                    self.mark_paused_if_current(generation, "owner", None);
                    return;
                }
                ControlSignal::PauseSleep => {
                    self.mark_paused_if_current(generation, "sleep", None);
                    return;
                }
                ControlSignal::Run => {}
            }

            let part_len = fs::symlink_metadata(&params.part_path)
                .ok()
                .filter(|m| m.file_type().is_file())
                .map(|m| m.len())
                .unwrap_or(0);

            if part_len == params.size_bytes {
                self.verify_and_finalize(generation, &params).await;
                return;
            }

            let (range_header, if_range_header, requested_offset) =
                if part_len > 0 && part_len < params.size_bytes {
                    (
                        Some(format!("bytes={part_len}-")),
                        Some(params.etag.clone()),
                        part_len,
                    )
                } else {
                    (None, None, 0)
                };

            let opened = tokio::select! {
                res = self.inner.opener.open(
                    &params.binding,
                    &params.download_path,
                    range_header,
                    if_range_header,
                ) => res,
                _ = wait_control_stop(&mut control_rx) => {
                    let signal = *control_rx.borrow();
                    match signal {
                        ControlSignal::Cancel => {
                            let _ = fs::remove_file(&params.part_path);
                        }
                        ControlSignal::PauseOwner => {
                            self.mark_paused_if_current(generation, "owner", None);
                        }
                        ControlSignal::PauseSleep => {
                            self.mark_paused_if_current(generation, "sleep", None);
                        }
                        ControlSignal::Run => {}
                    }
                    return;
                }
            };

            let mut stream = match opened {
                Ok(stream) => stream,
                Err(ExportOpenError::Forbidden) => {
                    self.fail_and_cleanup(generation, "export_requester_revoked");
                    return;
                }
                Err(ExportOpenError::InvalidPath) => {
                    self.fail_and_cleanup(generation, "invalid_request");
                    return;
                }
                Err(ExportOpenError::TransportUnavailable) => {
                    self.mark_paused_if_current(generation, "disconnected", None);
                    return;
                }
            };

            if self.record_transport_and_check_relay_pause(generation, stream.transport) {
                self.mark_paused_if_current(generation, "relay_switched", None);
                return;
            }

            match stream.status {
                200 => {}
                206 => {
                    if !matches_content_range(
                        stream.content_range.as_deref(),
                        requested_offset,
                        params.size_bytes,
                    ) {
                        self.mark_paused_if_current(generation, "disconnected", None);
                        return;
                    }
                    if !matches_strong_etag(stream.etag.as_deref(), &params.etag) {
                        if !reset_and_retry_once {
                            reset_and_retry_once = true;
                            let _ = fs::remove_file(&params.part_path);
                            self.update_progress(generation, 0);
                            continue;
                        }
                        self.fail_and_cleanup(generation, "checksum_mismatch");
                        return;
                    }
                }
                416 => {
                    if !reset_and_retry_once {
                        reset_and_retry_once = true;
                        let _ = fs::remove_file(&params.part_path);
                        self.update_progress(generation, 0);
                        continue;
                    }
                    self.fail_and_cleanup(generation, "export_range_error");
                    return;
                }
                410 => {
                    let body = stream.body.read_bounded(MAX_ERROR_BODY_BYTES).await;
                    let code = parse_error_code(&body)
                        .unwrap_or_else(|| "export_invalidated_by_erasure".to_owned());
                    self.fail_and_cleanup(generation, &code);
                    return;
                }
                404 => {
                    let body = stream.body.read_bounded(MAX_ERROR_BODY_BYTES).await;
                    let code =
                        parse_error_code(&body).unwrap_or_else(|| "export_not_found".to_owned());
                    self.fail_and_cleanup(generation, &code);
                    return;
                }
                401 | 403 => {
                    self.fail_and_cleanup(generation, "export_requester_revoked");
                    return;
                }
                _ => {
                    self.mark_paused_if_current(generation, "disconnected", None);
                    return;
                }
            }

            let start_offset = if stream.status == 206 {
                requested_offset
            } else {
                0
            };

            let mut file = match open_part_file(&params.part_path, start_offset) {
                Ok(file) => file,
                Err(error) => {
                    let code = if is_disk_space_io_error(&error) {
                        "insufficient_device_space"
                    } else {
                        "dest_write_failed"
                    };
                    self.fail_and_cleanup(generation, code);
                    return;
                }
            };

            let mut written = start_offset;
            self.update_progress(generation, written);

            loop {
                let next = tokio::select! {
                    biased;
                    _ = wait_control_stop(&mut control_rx) => {
                        let _ = file.flush();
                        drop(file);
                        let signal = *control_rx.borrow();
                        match signal {
                            ControlSignal::Cancel => {
                                let _ = fs::remove_file(&params.part_path);
                            }
                            ControlSignal::PauseOwner => {
                                self.mark_paused_if_current(generation, "owner", Some(written));
                            }
                            ControlSignal::PauseSleep => {
                                self.mark_paused_if_current(generation, "sleep", Some(written));
                            }
                            ControlSignal::Run => {}
                        }
                        return;
                    }
                    chunk = stream.body.next_chunk() => chunk,
                };

                match next {
                    Ok(Some(bytes)) => {
                        if bytes.is_empty() {
                            continue;
                        }
                        if written.saturating_add(bytes.len() as u64) > params.size_bytes {
                            drop(file);
                            self.fail_and_cleanup(generation, "export_size_mismatch");
                            return;
                        }
                        if let Err(error) = file.write_all(&bytes) {
                            drop(file);
                            let code = if is_disk_space_io_error(&error) {
                                "insufficient_device_space"
                            } else {
                                "dest_write_failed"
                            };
                            self.fail_and_cleanup(generation, code);
                            return;
                        }
                        written += bytes.len() as u64;
                        self.update_progress(generation, written);
                    }
                    Ok(None) => {
                        let _ = file.flush();
                        break;
                    }
                    Err(()) => {
                        let _ = file.flush();
                        drop(file);
                        self.mark_paused_if_current(generation, "disconnected", Some(written));
                        return;
                    }
                }
            }

            if written < params.size_bytes {
                drop(file);
                self.mark_paused_if_current(generation, "disconnected", Some(written));
                return;
            }
            if let Err(error) = file.sync_all() {
                drop(file);
                let code = if is_disk_space_io_error(&error) {
                    "insufficient_device_space"
                } else {
                    "dest_write_failed"
                };
                self.fail_and_cleanup(generation, code);
                return;
            }
            drop(file);
            self.verify_and_finalize(generation, &params).await;
            return;
        }
    }

    async fn verify_and_finalize(&self, generation: u64, params: &RunParams) {
        {
            let mut state = self.lock_state();
            if !state
                .active
                .as_ref()
                .is_some_and(|a| a.generation == generation)
            {
                return;
            }
            state.snapshot.phase = "verifying".to_owned();
            state.snapshot.bytes_written = params.size_bytes;
            state.snapshot.pause_reason = None;
            state.snapshot.error_code = None;
        }

        let part_path = params.part_path.clone();
        let expected_size = params.size_bytes;
        let expected_sha = params.sha256.clone();
        let cancel_flag = Arc::clone(&params.cancel_flag);
        #[cfg(test)]
        let verify_hook = self
            .inner
            .verify_chunk_hook
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let verify_result = tokio::task::spawn_blocking(move || -> io::Result<VerifyOutcome> {
            let file = File::open(&part_path)?;
            let reader = CancelableReader {
                inner: file,
                cancel_flag: Arc::clone(&cancel_flag),
                bytes_read: 0,
                #[cfg(test)]
                on_chunk: verify_hook,
            };
            let hashed = observer_log::hash_reader_sha256(reader);
            // `reader` (and its underlying `File`) is dropped here before any unlink attempt
            // so Windows releases the file handle promptly on cancellation.
            match hashed {
                Ok((digest, total)) => {
                    if cancel_flag.load(Ordering::Acquire) {
                        let _ = fs::remove_file(&part_path);
                        Ok(VerifyOutcome::Cancelled)
                    } else if total == expected_size && digest.eq_ignore_ascii_case(&expected_sha) {
                        Ok(VerifyOutcome::Matched)
                    } else {
                        Ok(VerifyOutcome::Mismatch)
                    }
                }
                Err(error)
                    if error.kind() == io::ErrorKind::Interrupted
                        || cancel_flag.load(Ordering::Acquire) =>
                {
                    let _ = fs::remove_file(&part_path);
                    Ok(VerifyOutcome::Cancelled)
                }
                Err(error) => Err(error),
            }
        })
        .await;

        match verify_result {
            Ok(Ok(VerifyOutcome::Matched)) => {}
            Ok(Ok(VerifyOutcome::Cancelled)) => {
                let _ = fs::remove_file(&params.part_path);
                return;
            }
            Ok(Ok(VerifyOutcome::Mismatch)) => {
                self.fail_and_cleanup(generation, "checksum_mismatch");
                return;
            }
            Ok(Err(_)) | Err(_) => {
                if params.cancel_flag.load(Ordering::Acquire) {
                    let _ = fs::remove_file(&params.part_path);
                    return;
                }
                self.fail_and_cleanup(generation, "dest_write_failed");
                return;
            }
        }

        if self.check_pairing_lost(generation, &params.binding).await {
            return;
        }
        if params.cancel_flag.load(Ordering::Acquire)
            || *params.control_rx.borrow() == ControlSignal::Cancel
        {
            let _ = fs::remove_file(&params.part_path);
            return;
        }

        // Publish under `publish_lock -> state` so a concurrent `cancel()` or
        // `cancel_for_unpair()` can never race with `replace_file` and let a
        // cancelled generation clobber the destination or a newer retry.
        let inner = Arc::clone(&self.inner);
        let part_path = params.part_path.clone();
        let dest_path = params.destination_path.clone();
        let cancel_flag = Arc::clone(&params.cancel_flag);
        let publish_result = tokio::task::spawn_blocking(move || -> PublishOutcome {
            let _publish_guard = inner
                .publish_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            {
                let state = inner
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if cancel_flag.load(Ordering::Acquire)
                    || !state
                        .active
                        .as_ref()
                        .is_some_and(|a| a.generation == generation)
                {
                    let _ = fs::remove_file(&part_path);
                    return PublishOutcome::Cancelled;
                }
            }
            match (inner.replace_file)(&part_path, &dest_path) {
                Ok(()) => {
                    let mut state = inner
                        .state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if let Some(active) = state.active.take_if(|a| a.generation == generation) {
                        state.saved_export_id = Some(active.export_id.clone());
                        state.saved_destination = Some(active.destination_path);
                        state.saved_display = Some(active.destination_display.clone());
                        state.snapshot = ExportStatusSnapshot {
                            id: None,
                            export_id: Some(active.export_id),
                            phase: "saved".to_owned(),
                            bytes_written: active.size_bytes,
                            total_bytes: active.size_bytes,
                            destination_display: Some(active.destination_display),
                            pause_reason: None,
                            error_code: None,
                            required_bytes: None,
                            available_bytes: None,
                            can_reveal: true,
                            verified: true,
                            platform: PLATFORM,
                        };
                    }
                    PublishOutcome::Published
                }
                Err(error) => PublishOutcome::IoError(error),
            }
        })
        .await;

        match publish_result {
            Ok(PublishOutcome::Published | PublishOutcome::Cancelled) => {}
            Ok(PublishOutcome::IoError(error)) => {
                let code = if is_disk_space_io_error(&error) {
                    "insufficient_device_space"
                } else {
                    "dest_write_failed"
                };
                self.fail_and_cleanup(generation, code);
            }
            Err(_) => {
                self.fail_and_cleanup(generation, "dest_write_failed");
            }
        }
    }

    fn snapshot_active_params(&self, generation: u64) -> Option<RunParams> {
        let state = self.lock_state();
        let active = state.active.as_ref()?;
        if active.generation != generation {
            return None;
        }
        Some(RunParams {
            download_path: active.download_path.clone(),
            size_bytes: active.size_bytes,
            sha256: active.sha256.clone(),
            etag: active.etag.clone(),
            destination_path: active.destination_path.clone(),
            part_path: active.part_path.clone(),
            binding: active.binding.clone(),
            control_rx: active.control_tx.subscribe(),
            cancel_flag: Arc::clone(&active.cancel_flag),
        })
    }

    fn record_transport_and_check_relay_pause(
        &self,
        generation: u64,
        transport: &'static str,
    ) -> bool {
        let mut state = self.lock_state();
        let Some(active) = state.active.as_mut() else {
            return false;
        };
        if active.generation != generation {
            return false;
        }
        if active.initial_transport.is_none() {
            active.initial_transport = Some(transport);
            return false;
        }
        active.initial_transport == Some("local") && transport == "relay" && !active.allow_relay
    }

    async fn check_pairing_lost(&self, generation: u64, expected_binding: &str) -> bool {
        let still_valid = self
            .inner
            .opener
            .current_binding()
            .await
            .is_some_and(|current| current == expected_binding);
        if still_valid {
            return false;
        }
        let part_to_remove = {
            let mut state = self.lock_state();
            if !state
                .active
                .as_ref()
                .is_some_and(|a| a.generation == generation)
            {
                return true;
            }
            let active = state.active.take().expect("active checked");
            active.cancel_flag.store(true, Ordering::Release);
            let _ = active.control_tx.send(ControlSignal::Cancel);
            state.snapshot = ExportStatusSnapshot {
                id: None,
                export_id: Some(active.export_id),
                phase: "cancelled".to_owned(),
                bytes_written: 0,
                total_bytes: active.size_bytes,
                destination_display: Some(active.destination_display),
                pause_reason: None,
                error_code: None,
                required_bytes: None,
                available_bytes: None,
                can_reveal: false,
                verified: false,
                platform: PLATFORM,
            };
            Some(active.part_path)
        };
        if let Some(part) = part_to_remove {
            let _ = fs::remove_file(part);
        }
        true
    }

    fn update_progress(&self, generation: u64, bytes_written: u64) {
        let mut state = self.lock_state();
        if state
            .active
            .as_ref()
            .is_some_and(|a| a.generation == generation)
            && state.snapshot.phase == "transferring"
        {
            state.snapshot.bytes_written = bytes_written;
        }
    }

    fn mark_paused_if_current(&self, generation: u64, reason: &str, bytes_written: Option<u64>) {
        let mut state = self.lock_state();
        if !state
            .active
            .as_ref()
            .is_some_and(|a| a.generation == generation)
        {
            return;
        }
        if matches!(
            state.snapshot.phase.as_str(),
            "saved" | "error" | "cancelled"
        ) {
            return;
        }
        state.snapshot.phase = "paused".to_owned();
        if let Some(written) = bytes_written {
            state.snapshot.bytes_written = written;
        }
        state.snapshot.pause_reason = Some(reason.to_owned());
        state.snapshot.error_code = None;
    }

    fn fail_and_cleanup(&self, generation: u64, error_code: &str) {
        let part_to_remove = {
            let mut state = self.lock_state();
            if !state
                .active
                .as_ref()
                .is_some_and(|a| a.generation == generation)
            {
                return;
            }
            let active = state.active.take().expect("active checked");
            state.snapshot = ExportStatusSnapshot {
                id: None,
                export_id: Some(active.export_id),
                phase: "error".to_owned(),
                bytes_written: 0,
                total_bytes: active.size_bytes,
                destination_display: Some(active.destination_display),
                pause_reason: None,
                error_code: Some(error_code.to_owned()),
                required_bytes: None,
                available_bytes: None,
                can_reveal: false,
                verified: false,
                platform: PLATFORM,
            };
            Some(active.part_path)
        };
        if let Some(part) = part_to_remove {
            let _ = fs::remove_file(part);
        }
    }
}

struct RunParams {
    download_path: String,
    size_bytes: u64,
    sha256: String,
    etag: String,
    destination_path: PathBuf,
    part_path: PathBuf,
    binding: String,
    control_rx: watch::Receiver<ControlSignal>,
    cancel_flag: Arc<AtomicBool>,
}

enum VerifyOutcome {
    Matched,
    Mismatch,
    Cancelled,
}

enum PublishOutcome {
    Published,
    Cancelled,
    IoError(io::Error),
}

struct CancelableReader<R> {
    inner: R,
    cancel_flag: Arc<AtomicBool>,
    bytes_read: u64,
    #[cfg(test)]
    on_chunk: Option<VerifyChunkHook>,
}

impl<R: Read> Read for CancelableReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel_flag.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "verification cancelled",
            ));
        }
        let limit = buf.len().min(VERIFY_CHUNK_BYTES);
        let n = self.inner.read(&mut buf[..limit])?;
        self.bytes_read = self.bytes_read.saturating_add(n as u64);
        #[cfg(test)]
        if n > 0 {
            if let Some(hook) = &self.on_chunk {
                hook(self.bytes_read);
            }
        }
        if self.cancel_flag.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "verification cancelled",
            ));
        }
        Ok(n)
    }
}

/// Production opener driving streaming export downloads over the shared SPL
/// loopback journal bridge without buffering the response body in memory.
pub struct BridgeExportOpener {
    load_client: ClientLoader,
}

impl BridgeExportOpener {
    pub fn from_credential_access(
        access: Arc<tokio::sync::Mutex<Option<CredentialAccess>>>,
    ) -> Self {
        Self {
            load_client: Arc::new(move || {
                let access = Arc::clone(&access);
                Box::pin(async move { crate::access::load_current_client(&access).await })
            }),
        }
    }

    pub fn from_loader(loader: ClientLoader) -> Self {
        Self {
            load_client: loader,
        }
    }
}

struct ObservedOpener {
    client: Arc<ObserverClient>,
    selected_transport: Arc<AtomicU8>,
}

impl CarrierOpener for ObservedOpener {
    fn proxy_headers(
        &self,
        headers: &[(String, String)],
    ) -> Result<Vec<(String, String)>, spl_transport::TransportError> {
        Ok(self.client.proxy_headers(headers))
    }

    fn dial_carrier(
        &self,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<spl_transport::DialedCarrier, spl_transport::TransportError>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            if !self.client.gate_open() || self.client.refusal_stop().is_some() {
                return Err(spl_transport::TransportError::NoEndpoint);
            }
            let obs = OperationObserver::new_unshared();
            let result = self
                .client
                .transport_client()
                .open_carrier(Some(&obs))
                .await;
            if result.is_ok() {
                let code = if obs.relay_successes() > 0 { 2 } else { 1 };
                self.selected_transport.store(code, Ordering::Release);
            }
            result.map_err(|error| match error {
                spl_transport::CarrierOpenError::Transport(error) => error,
                _ => spl_transport::TransportError::NoEndpoint,
            })
        })
    }
}

impl ExportStreamOpener for BridgeExportOpener {
    fn current_binding<'a>(&'a self) -> Pin<Box<dyn Future<Output = Option<String>> + Send + 'a>> {
        Box::pin(async move {
            let client = (self.load_client)().await?;
            if !client.gate_open() || client.refusal_stop().is_some() {
                return None;
            }
            Some(client.journal_identity().client_cert_sha256)
        })
    }

    fn open<'a>(
        &'a self,
        expected_binding: &'a str,
        download_path: &'a str,
        range_header: Option<String>,
        if_range_header: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<OpenedExportStream, ExportOpenError>> + Send + 'a>>
    {
        let expected_binding = expected_binding.to_owned();
        let download_path = download_path.to_owned();
        Box::pin(async move {
            if !is_export_download_path(&download_path) {
                return Err(ExportOpenError::InvalidPath);
            }
            let Some(client) = (self.load_client)().await else {
                return Err(ExportOpenError::Forbidden);
            };
            if !client.gate_open()
                || client.refusal_stop().is_some()
                || client.journal_identity().client_cert_sha256 != expected_binding
            {
                return Err(ExportOpenError::Forbidden);
            }

            let selected_transport = Arc::new(AtomicU8::new(0));
            let allowed_path = download_path.clone();
            let bridge = journal_bridge::start(JournalBridgeConfig {
                opener: Arc::new(ObservedOpener {
                    client: Arc::clone(&client),
                    selected_transport: Arc::clone(&selected_transport),
                }),
                bridge_names: BridgeNames {
                    capability_cookie_name: observer_pl::CAP_COOKIE_NAME.into(),
                    upstream_cookie_prefix: observer_pl::UPSTREAM_COOKIE_PREFIX.into(),
                    observer_header_name: observer_pl::OBSERVER_HANDLE_HEADER.to_ascii_lowercase(),
                    protocol_version_header_name: observer_pl::PROTOCOL_VERSION_HEADER
                        .to_ascii_lowercase(),
                },
                endpoint_hosts: Vec::new(),
                policy: BridgePolicy {
                    stream_response: Arc::new(|_| true),
                    local_response: Arc::new(move |head, _| {
                        if head.method == "GET" && head.target == allowed_path {
                            None
                        } else {
                            Some(journal_bridge::LocalResponse {
                                status: 403,
                                content_type: "text/plain".into(),
                                body: Vec::new(),
                            })
                        }
                    }),
                    ..Default::default()
                },
            })
            .await
            .map_err(|_| ExportOpenError::TransportUnavailable)?;

            let port = bridge.port();
            let cookie = bridge
                .capability_cookie()
                .ok_or(ExportOpenError::TransportUnavailable)?;
            let cookie = cookie.split(';').next().unwrap_or(&cookie);

            let mut headers = vec![
                ("Cookie".to_owned(), cookie.to_owned()),
                ("Cache-Control".to_owned(), "no-cache".to_owned()),
                ("Connection".to_owned(), "close".to_owned()),
            ];
            if let Some(range) = range_header {
                headers.push(("Range".to_owned(), range));
            }
            if let Some(if_range) = if_range_header {
                headers.push(("If-Range".to_owned(), if_range));
            }
            let head = http::build_request_head_with_host(
                "GET",
                &download_path,
                &format!("127.0.0.1:{port}"),
                &headers,
                0,
            );

            let mut tcp = match tokio::time::timeout(
                OPEN_TIMEOUT,
                tokio::net::TcpStream::connect(("127.0.0.1", port)),
            )
            .await
            {
                Ok(Ok(stream)) => stream,
                _ => {
                    bridge.begin_shutdown();
                    return Err(ExportOpenError::TransportUnavailable);
                }
            };
            if tokio::time::timeout(OPEN_TIMEOUT, tcp.write_all(&head))
                .await
                .map_err(|_| ())
                .and_then(|r| r.map_err(|_| ()))
                .is_err()
            {
                bridge.begin_shutdown();
                return Err(ExportOpenError::TransportUnavailable);
            }

            let parsed =
                match tokio::time::timeout(OPEN_TIMEOUT, read_http_response_head(tcp)).await {
                    Ok(Ok(parsed)) => parsed,
                    _ => {
                        bridge.begin_shutdown();
                        return Err(ExportOpenError::TransportUnavailable);
                    }
                };

            let status = parsed.status;
            if status == 403 || (status == 502 && bridge.status().terminal_reason.is_some()) {
                bridge.begin_shutdown();
                return Err(ExportOpenError::Forbidden);
            }

            let transport = match selected_transport.load(Ordering::Acquire) {
                2 => "relay",
                _ => "local",
            };
            let content_range = parsed.content_range.clone();
            let etag = parsed.etag.clone();
            Ok(OpenedExportStream {
                status,
                content_range,
                etag,
                transport,
                body: Box::new(BridgeHttpBodyStream {
                    _bridge: BridgeShutdownGuard(Some(bridge)),
                    reader: parsed.body_reader,
                }),
            })
        })
    }
}

struct BridgeShutdownGuard(Option<journal_bridge::JournalBridgeHandle>);

impl Drop for BridgeShutdownGuard {
    fn drop(&mut self) {
        if let Some(bridge) = self.0.take() {
            bridge.begin_shutdown();
        }
    }
}

struct BridgeHttpBodyStream {
    _bridge: BridgeShutdownGuard,
    reader: HttpBodyReader<tokio::net::TcpStream>,
}

impl ExportByteStream for BridgeHttpBodyStream {
    fn next_chunk<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Vec<u8>>, ()>> + Send + 'a>> {
        Box::pin(async move {
            tokio::time::timeout(READ_IDLE_TIMEOUT, self.reader.next_chunk())
                .await
                .map_err(|_| ())?
        })
    }

    fn read_bounded<'a>(
        &'a mut self,
        max_bytes: usize,
    ) -> Pin<Box<dyn Future<Output = Vec<u8>> + Send + 'a>> {
        Box::pin(async move {
            let mut out = Vec::new();
            while out.len() < max_bytes {
                match self.reader.next_chunk().await {
                    Ok(Some(chunk)) => {
                        let take = chunk.len().min(max_bytes - out.len());
                        out.extend_from_slice(&chunk[..take]);
                        if take < chunk.len() {
                            break;
                        }
                    }
                    _ => break,
                }
            }
            out
        })
    }
}

struct ParsedHttpResponse<R> {
    status: u16,
    content_range: Option<String>,
    etag: Option<String>,
    body_reader: HttpBodyReader<R>,
}

enum BodyMode {
    ContentLength(u64),
    Chunked {
        in_chunk_remaining: usize,
        expect_crlf_after_chunk: bool,
        eof: bool,
    },
    UntilEof,
}

struct HttpBodyReader<R> {
    inner: R,
    buffered: Vec<u8>,
    mode: BodyMode,
}

async fn read_http_response_head<R: AsyncRead + Unpin>(
    mut inner: R,
) -> Result<ParsedHttpResponse<R>, ()> {
    let mut raw = Vec::new();
    let mut scratch = [0u8; 4096];
    let split = loop {
        if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        if raw.len() > HEAD_CAP {
            return Err(());
        }
        let n = inner.read(&mut scratch).await.map_err(|_| ())?;
        if n == 0 {
            return Err(());
        }
        raw.extend_from_slice(&scratch[..n]);
    };
    let head_bytes = &raw[..split];
    let leftover = raw[split + 4..].to_vec();
    let (status, headers) = http::parse_head(head_bytes).map_err(|_| ())?;
    let mut content_length: Option<u64> = None;
    let mut content_range: Option<String> = None;
    let mut etag: Option<String> = None;
    let mut chunked = false;
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.trim().parse::<u64>().ok();
        } else if name.eq_ignore_ascii_case("content-range") {
            content_range = Some(value.trim().to_owned());
        } else if name.eq_ignore_ascii_case("etag") {
            etag = Some(value.trim().to_owned());
        } else if name.eq_ignore_ascii_case("transfer-encoding")
            && value.to_ascii_lowercase().contains("chunked")
        {
            chunked = true;
        }
    }
    let mode = if chunked {
        BodyMode::Chunked {
            in_chunk_remaining: 0,
            expect_crlf_after_chunk: false,
            eof: false,
        }
    } else if let Some(len) = content_length {
        BodyMode::ContentLength(len)
    } else {
        BodyMode::UntilEof
    };
    Ok(ParsedHttpResponse {
        status,
        content_range,
        etag,
        body_reader: HttpBodyReader {
            inner,
            buffered: leftover,
            mode,
        },
    })
}

impl<R: AsyncRead + Unpin> HttpBodyReader<R> {
    async fn fill(&mut self) -> Result<usize, ()> {
        let mut scratch = vec![0u8; CHUNK_READ_SIZE];
        let n = self.inner.read(&mut scratch).await.map_err(|_| ())?;
        if n > 0 {
            self.buffered.extend_from_slice(&scratch[..n]);
        }
        Ok(n)
    }

    async fn read_crlf_line(&mut self) -> Result<String, ()> {
        loop {
            if let Some(pos) = self.buffered.windows(2).position(|w| w == b"\r\n") {
                let line = String::from_utf8(self.buffered[..pos].to_vec()).map_err(|_| ())?;
                self.buffered.drain(..pos + 2);
                return Ok(line);
            }
            if self.buffered.len() > 8192 {
                return Err(());
            }
            if self.fill().await? == 0 {
                return Err(());
            }
        }
    }

    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, ()> {
        match self.mode {
            BodyMode::ContentLength(remaining) => {
                if remaining == 0 {
                    return Ok(None);
                }
                if self.buffered.is_empty() && self.fill().await? == 0 {
                    return Err(());
                }
                let take = usize::try_from(remaining.min(self.buffered.len() as u64))
                    .unwrap_or(self.buffered.len());
                if let BodyMode::ContentLength(rem) = &mut self.mode {
                    *rem -= take as u64;
                }
                let out = self.buffered.drain(..take).collect();
                Ok(Some(out))
            }
            BodyMode::UntilEof => {
                if !self.buffered.is_empty() {
                    return Ok(Some(std::mem::take(&mut self.buffered)));
                }
                if self.fill().await? == 0 {
                    return Ok(None);
                }
                Ok(Some(std::mem::take(&mut self.buffered)))
            }
            BodyMode::Chunked { .. } => loop {
                let (in_rem, need_crlf, eof) = match self.mode {
                    BodyMode::Chunked {
                        in_chunk_remaining,
                        expect_crlf_after_chunk,
                        eof,
                    } => (in_chunk_remaining, expect_crlf_after_chunk, eof),
                    _ => unreachable!(),
                };
                if eof {
                    return Ok(None);
                }
                if need_crlf {
                    while self.buffered.len() < 2 {
                        if self.fill().await? == 0 {
                            return Err(());
                        }
                    }
                    if &self.buffered[..2] != b"\r\n" {
                        return Err(());
                    }
                    self.buffered.drain(..2);
                    if let BodyMode::Chunked {
                        expect_crlf_after_chunk,
                        ..
                    } = &mut self.mode
                    {
                        *expect_crlf_after_chunk = false;
                    }
                    continue;
                }
                if in_rem > 0 {
                    if self.buffered.is_empty() && self.fill().await? == 0 {
                        return Err(());
                    }
                    let take = in_rem.min(self.buffered.len());
                    let out: Vec<u8> = self.buffered.drain(..take).collect();
                    if let BodyMode::Chunked {
                        in_chunk_remaining,
                        expect_crlf_after_chunk,
                        ..
                    } = &mut self.mode
                    {
                        *in_chunk_remaining -= take;
                        if *in_chunk_remaining == 0 {
                            *expect_crlf_after_chunk = true;
                        }
                    }
                    return Ok(Some(out));
                }
                let size_line = self.read_crlf_line().await?;
                let size_hex = size_line.split(';').next().unwrap_or("").trim();
                let chunk_size = usize::from_str_radix(size_hex, 16).map_err(|_| ())?;
                if chunk_size == 0 {
                    // Drain optional trailers until blank line.
                    loop {
                        let trailer = self.read_crlf_line().await?;
                        if trailer.is_empty() {
                            break;
                        }
                    }
                    if let BodyMode::Chunked { eof, .. } = &mut self.mode {
                        *eof = true;
                    }
                    return Ok(None);
                }
                if let BodyMode::Chunked {
                    in_chunk_remaining, ..
                } = &mut self.mode
                {
                    *in_chunk_remaining = chunk_size;
                }
            },
        }
    }
}

async fn wait_control_stop(rx: &mut watch::Receiver<ControlSignal>) {
    loop {
        if *rx.borrow() != ControlSignal::Run {
            return;
        }
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

fn open_part_file(part_path: &Path, start_offset: u64) -> io::Result<File> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(part_path)?;
    if start_offset == 0 {
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
    } else {
        file.seek(SeekFrom::Start(start_offset))?;
    }
    Ok(file)
}

fn sibling_part_path_for_generation(parent: &Path, export_id: &str, generation: u64) -> PathBuf {
    parent.join(format!(".solstone-export-{export_id}-{generation}.part"))
}

fn is_part_file_for_export(file_name: &str, export_id: &str) -> bool {
    let prefix = format!(".solstone-export-{export_id}");
    if file_name == format!("{prefix}.part") {
        return true;
    }
    let Some(rest) = file_name.strip_prefix(&format!("{prefix}-")) else {
        return false;
    };
    let Some(gen_str) = rest.strip_suffix(".part") else {
        return false;
    };
    !gen_str.is_empty() && gen_str.bytes().all(|b| b.is_ascii_digit())
}

/// Discover the largest regular non-symlink on-disk partial for `export_id`
/// inside `parent` (supporting both legacy unsuffixed `.part` and per-generation
/// `.solstone-export-<id>-<gen>.part` files) so daemon/app restarts credit the
/// already-written bytes in preflight and resume from the exact byte offset.
fn discover_existing_part_file(
    parent: &Path,
    export_id: &str,
    expected_size: u64,
) -> Option<(PathBuf, u64)> {
    let entries = fs::read_dir(parent).ok()?;
    let mut best: Option<(PathBuf, u64)> = None;
    let mut extras: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !is_part_file_for_export(&name, export_id) {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.file_type().is_file() {
            continue;
        }
        let len = meta.len();
        if len > expected_size {
            extras.push(path);
            continue;
        }
        match &best {
            Some((_, best_len)) if len <= *best_len => extras.push(path),
            Some((prev_path, _)) => {
                extras.push(prev_path.clone());
                best = Some((path, len));
            }
            None => best = Some((path, len)),
        }
    }
    for stale in extras {
        let _ = fs::remove_file(stale);
    }
    best
}

pub fn required_device_bytes(remaining_bytes: u64) -> u64 {
    let headroom = (remaining_bytes / 100).clamp(1024 * 1024, 64 * 1024 * 1024);
    remaining_bytes.saturating_add(headroom)
}

fn valid_export_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_sha256_hex(hex: &str) -> bool {
    hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn matches_content_range(header: Option<&str>, expected_start: u64, expected_total: u64) -> bool {
    let Some(header) = header else {
        return false;
    };
    let Some(rest) = header.strip_prefix("bytes ") else {
        return false;
    };
    let Some((range_part, total_part)) = rest.split_once('/') else {
        return false;
    };
    let Ok(total) = total_part.trim().parse::<u64>() else {
        return false;
    };
    if total != expected_total {
        return false;
    }
    let Some((start_part, end_part)) = range_part.trim().split_once('-') else {
        return false;
    };
    let (Ok(start), Ok(end)) = (start_part.parse::<u64>(), end_part.parse::<u64>()) else {
        return false;
    };
    start == expected_start && end >= start && end < expected_total
}

fn matches_strong_etag(observed: Option<&str>, expected: &str) -> bool {
    let expected = expected.trim();
    if expected.is_empty() || expected.starts_with("W/") {
        return false;
    }
    observed
        .map(str::trim)
        .is_some_and(|val| !val.starts_with("W/") && val == expected)
}

fn parse_error_code(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("code")?.as_str().map(str::to_owned))
        .filter(|s| !s.is_empty())
}

fn is_disk_space_io_error(error: &io::Error) -> bool {
    // POSIX ENOSPC (28) / EDQUOT (122) and Windows ERROR_DISK_FULL (112) / ERROR_HANDLE_DISK_FULL (39).
    matches!(error.raw_os_error(), Some(28 | 39 | 112 | 122))
}

fn failed_snapshot(
    id: Option<String>,
    export_id: Option<String>,
    error_code: &str,
) -> ExportStatusSnapshot {
    ExportStatusSnapshot {
        id,
        export_id,
        phase: "error".to_owned(),
        bytes_written: 0,
        total_bytes: 0,
        destination_display: None,
        pause_reason: None,
        error_code: Some(error_code.to_owned()),
        required_bytes: None,
        available_bytes: None,
        can_reveal: false,
        verified: false,
        platform: PLATFORM,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicUsize;

    struct ScriptedStream {
        chunks: VecDeque<Result<Vec<u8>, ()>>,
    }

    impl ExportByteStream for ScriptedStream {
        fn next_chunk<'a>(
            &'a mut self,
        ) -> Pin<Box<dyn Future<Output = Result<Option<Vec<u8>>, ()>> + Send + 'a>> {
            let item = self.chunks.pop_front();
            Box::pin(async move {
                match item {
                    Some(Ok(bytes)) => Ok(Some(bytes)),
                    Some(Err(())) => Err(()),
                    None => Ok(None),
                }
            })
        }

        fn read_bounded<'a>(
            &'a mut self,
            max_bytes: usize,
        ) -> Pin<Box<dyn Future<Output = Vec<u8>> + Send + 'a>> {
            let mut out = Vec::new();
            while out.len() < max_bytes {
                match self.chunks.pop_front() {
                    Some(Ok(bytes)) => {
                        let take = bytes.len().min(max_bytes - out.len());
                        out.extend_from_slice(&bytes[..take]);
                    }
                    _ => break,
                }
            }
            Box::pin(async move { out })
        }
    }

    struct ScriptedOpener {
        binding: Mutex<Option<String>>,
        seen_ranges: Mutex<Vec<(Option<String>, Option<String>)>>,
        responses: Mutex<VecDeque<Result<OpenedExportStream, ExportOpenError>>>,
    }

    impl ScriptedOpener {
        fn new(responses: Vec<Result<OpenedExportStream, ExportOpenError>>) -> Self {
            Self {
                binding: Mutex::new(Some("binding-1".to_owned())),
                seen_ranges: Mutex::new(Vec::new()),
                responses: Mutex::new(VecDeque::from(responses)),
            }
        }
    }

    impl ExportStreamOpener for ScriptedOpener {
        fn current_binding<'a>(
            &'a self,
        ) -> Pin<Box<dyn Future<Output = Option<String>> + Send + 'a>> {
            let current = self.binding.lock().unwrap().clone();
            Box::pin(async move { current })
        }

        fn open<'a>(
            &'a self,
            expected_binding: &'a str,
            _download_path: &'a str,
            range_header: Option<String>,
            if_range_header: Option<String>,
        ) -> Pin<Box<dyn Future<Output = Result<OpenedExportStream, ExportOpenError>> + Send + 'a>>
        {
            self.seen_ranges
                .lock()
                .unwrap()
                .push((range_header, if_range_header));
            let current = self.binding.lock().unwrap().clone();
            let resp = if current.as_deref() != Some(expected_binding) {
                Err(ExportOpenError::Forbidden)
            } else {
                self.responses
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(Err(ExportOpenError::TransportUnavailable))
            };
            Box::pin(async move { resp })
        }
    }

    struct CountingHold(Arc<AtomicUsize>);
    impl Drop for CountingHold {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::AcqRel);
        }
    }

    async fn wait_phase(
        manager: &ExportTransferManager,
        export_id: &str,
        wanted: &str,
    ) -> ExportStatusSnapshot {
        for _ in 0..200 {
            let snap = manager.status("st", Some(export_id));
            if snap.phase == wanted {
                return snap;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "timed out waiting for phase {wanted}; last={:?}",
            manager.status("st", Some(export_id))
        );
    }

    #[tokio::test]
    async fn full_download_holds_power_verifies_sha256_and_atomically_replaces() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("solstone-journal-2026-11-26.zip");
        fs::write(&dest, b"preexisting-file").unwrap();

        let payload: Vec<u8> = (0..(128 * 1024 + 29)).map(|i| (i % 251) as u8).collect();
        let sha = observer_log::sha256_hex(&payload);
        let etag = format!("\"sha256-{sha}\"");
        let hold_active = Arc::new(AtomicUsize::new(0));
        let hold_acquired = Arc::new(AtomicUsize::new(0));
        let ha = Arc::clone(&hold_active);
        let hq = Arc::clone(&hold_acquired);

        let opener = Arc::new(ScriptedOpener::new(vec![Ok(OpenedExportStream {
            status: 200,
            content_range: None,
            etag: Some(etag.clone()),
            transport: "local",
            body: Box::new(ScriptedStream {
                chunks: VecDeque::from(vec![
                    Ok(payload[..65536].to_vec()),
                    Ok(payload[65536..].to_vec()),
                ]),
            }),
        })]));
        let manager = ExportTransferManager::new(
            opener,
            Arc::new(|_| Some(500 * 1024 * 1024)),
            Arc::new(|from, to| fs::rename(from, to)),
            Arc::new(move || {
                ha.fetch_add(1, Ordering::AcqRel);
                hq.fetch_add(1, Ordering::AcqRel);
                Some(Box::new(CountingHold(Arc::clone(&ha))))
            }),
        );

        let reply = manager
            .start(StartExportParams {
                request_id: "s1".into(),
                export_id: "exp_win_full".into(),
                download_path: "/app/export/api/ready/exp_win_full/download".into(),
                filename: "solstone-journal-2026-11-26.zip".into(),
                size_bytes: payload.len() as u64,
                sha256: sha,
                etag,
                destination_path: dest.clone(),
            })
            .await;
        assert_eq!(reply.phase, "transferring");

        let saved = wait_phase(&manager, "exp_win_full", "saved").await;
        assert!(saved.verified);
        assert!(saved.can_reveal);
        assert_eq!(fs::read(&dest).unwrap(), payload);
        assert!(!has_any_part_file(tmp.path(), "exp_win_full"));
        assert_eq!(hold_acquired.load(Ordering::Acquire), 1);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(hold_active.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn resume_and_lan_to_relay_pause_preserve_preexisting_destination() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("existing.zip");
        fs::write(&dest, b"keep-safe").unwrap();

        let payload: Vec<u8> = (0..(128 * 1024 + 11))
            .map(|i| ((i * 5) % 256) as u8)
            .collect();
        let sha = observer_log::sha256_hex(&payload);
        let etag = format!("\"sha256-{sha}\"");
        let split = 49152usize;

        let opener = Arc::new(ScriptedOpener::new(vec![
            Ok(OpenedExportStream {
                status: 200,
                content_range: None,
                etag: Some(etag.clone()),
                transport: "local",
                body: Box::new(ScriptedStream {
                    chunks: VecDeque::from(vec![Ok(payload[..split].to_vec()), Err(())]),
                }),
            }),
            Ok(OpenedExportStream {
                status: 206,
                content_range: Some(format!(
                    "bytes {split}-{}/{}",
                    payload.len() - 1,
                    payload.len()
                )),
                etag: Some(etag.clone()),
                transport: "relay",
                body: Box::new(ScriptedStream {
                    chunks: VecDeque::from(vec![Ok(payload[split..].to_vec())]),
                }),
            }),
            Ok(OpenedExportStream {
                status: 206,
                content_range: Some(format!(
                    "bytes {split}-{}/{}",
                    payload.len() - 1,
                    payload.len()
                )),
                etag: Some(etag.clone()),
                transport: "relay",
                body: Box::new(ScriptedStream {
                    chunks: VecDeque::from(vec![Ok(payload[split..].to_vec())]),
                }),
            }),
        ]));
        let manager = ExportTransferManager::new(
            opener.clone(),
            Arc::new(|_| Some(500 * 1024 * 1024)),
            Arc::new(|from, to| fs::rename(from, to)),
            Arc::new(|| None),
        );

        let _ = manager
            .start(StartExportParams {
                request_id: "s2".into(),
                export_id: "exp_win_relay".into(),
                download_path: "/app/export/api/ready/exp_win_relay/download".into(),
                filename: "existing.zip".into(),
                size_bytes: payload.len() as u64,
                sha256: sha,
                etag: etag.clone(),
                destination_path: dest.clone(),
            })
            .await;

        let paused = wait_phase(&manager, "exp_win_relay", "paused").await;
        assert_eq!(paused.pause_reason.as_deref(), Some("disconnected"));
        assert_eq!(fs::read(&dest).unwrap(), b"keep-safe");

        let _ = manager.resume("r1", Some("exp_win_relay"));
        let relay_paused = wait_phase(&manager, "exp_win_relay", "paused").await;
        assert_eq!(relay_paused.pause_reason.as_deref(), Some("relay_switched"));
        assert_eq!(fs::read(&dest).unwrap(), b"keep-safe");

        let _ = manager.resume("r2", Some("exp_win_relay"));
        let saved = wait_phase(&manager, "exp_win_relay", "saved").await;
        assert!(saved.verified);
        assert_eq!(fs::read(&dest).unwrap(), payload);

        let ranges = opener.seen_ranges.lock().unwrap().clone();
        assert_eq!(ranges[1], (Some(format!("bytes={split}-")), Some(etag)));
    }

    #[tokio::test]
    async fn erasure_410_and_insufficient_space_never_touch_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("keep.zip");
        fs::write(&dest, b"intact-bytes").unwrap();

        let opener = Arc::new(ScriptedOpener::new(vec![Ok(OpenedExportStream {
            status: 410,
            content_range: None,
            etag: None,
            transport: "local",
            body: Box::new(ScriptedStream {
                chunks: VecDeque::from(vec![Ok(
                    br#"{"code":"export_invalidated_by_erasure"}"#.to_vec()
                )]),
            }),
        })]));

        let low = ExportTransferManager::new(
            opener.clone(),
            Arc::new(|_| Some(512)),
            Arc::new(|from, to| fs::rename(from, to)),
            Arc::new(|| None),
        );
        let params = StartExportParams {
            request_id: "s3".into(),
            export_id: "exp_win_410".into(),
            download_path: "/app/export/api/ready/exp_win_410/download".into(),
            filename: "keep.zip".into(),
            size_bytes: 10 * 1024 * 1024,
            sha256: "a".repeat(64),
            etag: "\"e1\"".into(),
            destination_path: dest.clone(),
        };
        let low_reply = low.start(params.clone()).await;
        assert_eq!(low_reply.phase, "error");
        assert_eq!(
            low_reply.error_code.as_deref(),
            Some("insufficient_device_space")
        );
        assert_eq!(low_reply.available_bytes, Some(512));
        assert!(low_reply.required_bytes.unwrap_or(0) > params.size_bytes);
        assert_eq!(fs::read(&dest).unwrap(), b"intact-bytes");

        let ok_space = ExportTransferManager::new(
            opener,
            Arc::new(|_| Some(500 * 1024 * 1024)),
            Arc::new(|from, to| fs::rename(from, to)),
            Arc::new(|| None),
        );
        let _ = ok_space.start(params).await;
        let failed = wait_phase(&ok_space, "exp_win_410", "error").await;
        assert_eq!(
            failed.error_code.as_deref(),
            Some("export_invalidated_by_erasure")
        );
        assert_eq!(fs::read(&dest).unwrap(), b"intact-bytes");
        assert!(!has_any_part_file(tmp.path(), "exp_win_410"));
    }

    #[tokio::test]
    async fn resume_206_with_mismatched_etag_resets_part_instead_of_appending() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("etag.zip");
        let payload: Vec<u8> = (0..(64 * 1024 + 17)).map(|i| (i % 239) as u8).collect();
        let sha = observer_log::sha256_hex(&payload);
        let etag = format!("\"sha256-{sha}\"");
        let split = 16384usize;

        let opener = Arc::new(ScriptedOpener::new(vec![
            Ok(OpenedExportStream {
                status: 200,
                content_range: None,
                etag: Some(etag.clone()),
                transport: "local",
                body: Box::new(ScriptedStream {
                    chunks: VecDeque::from(vec![Ok(payload[..split].to_vec()), Err(())]),
                }),
            }),
            // Bogus 206 carrying a mismatched ETag must NOT be appended.
            Ok(OpenedExportStream {
                status: 206,
                content_range: Some(format!(
                    "bytes {split}-{}/{}",
                    payload.len() - 1,
                    payload.len()
                )),
                etag: Some("\"wrong-etag\"".to_owned()),
                transport: "local",
                body: Box::new(ScriptedStream {
                    chunks: VecDeque::from(vec![Ok(vec![0xff; payload.len() - split])]),
                }),
            }),
            // Fresh 200 retry succeeds cleanly.
            Ok(OpenedExportStream {
                status: 200,
                content_range: None,
                etag: Some(etag.clone()),
                transport: "local",
                body: Box::new(ScriptedStream {
                    chunks: VecDeque::from(vec![Ok(payload.clone())]),
                }),
            }),
        ]));
        let manager = ExportTransferManager::new(
            opener,
            Arc::new(|_| Some(500 * 1024 * 1024)),
            Arc::new(|from, to| fs::rename(from, to)),
            Arc::new(|| None),
        );
        let _ = manager
            .start(StartExportParams {
                request_id: "se".into(),
                export_id: "exp_win_etag".into(),
                download_path: "/app/export/api/ready/exp_win_etag/download".into(),
                filename: "etag.zip".into(),
                size_bytes: payload.len() as u64,
                sha256: sha,
                etag,
                destination_path: dest.clone(),
            })
            .await;
        let _ = wait_phase(&manager, "exp_win_etag", "paused").await;
        let _ = manager.resume("re", Some("exp_win_etag"));
        let saved = wait_phase(&manager, "exp_win_etag", "saved").await;
        assert!(saved.verified);
        assert_eq!(fs::read(&dest).unwrap(), payload);
    }

    #[tokio::test]
    async fn http_chunked_and_content_length_readers_stream_exact_bytes() {
        let raw_chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        let mut parsed = read_http_response_head(&raw_chunked[..]).await.unwrap();
        assert_eq!(parsed.status, 200);
        let mut collected = Vec::new();
        while let Some(chunk) = parsed.body_reader.next_chunk().await.unwrap() {
            collected.extend_from_slice(&chunk);
        }
        assert_eq!(collected, b"hello world");

        let raw_cl = b"HTTP/1.1 206 Partial Content\r\nContent-Length: 4\r\nContent-Range: bytes 2-5/10\r\nETag: \"abc\"\r\n\r\nabcd";
        let mut parsed_cl = read_http_response_head(&raw_cl[..]).await.unwrap();
        assert_eq!(parsed_cl.status, 206);
        assert_eq!(parsed_cl.content_range.as_deref(), Some("bytes 2-5/10"));
        assert_eq!(parsed_cl.etag.as_deref(), Some("\"abc\""));
        let mut out = Vec::new();
        while let Some(chunk) = parsed_cl.body_reader.next_chunk().await.unwrap() {
            out.extend_from_slice(&chunk);
        }
        assert_eq!(out, b"abcd");
    }

    fn has_any_part_file(parent: &Path, export_id: &str) -> bool {
        fs::read_dir(parent)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .any(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| is_part_file_for_export(name, export_id))
            })
    }

    fn build_valid_zip_bytes(min_size: usize) -> Vec<u8> {
        let mut cursor = io::Cursor::new(Vec::new());
        {
            let mut zw = zip::ZipWriter::new(&mut cursor);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zw.start_file("journal.txt", options).unwrap();
            let mut payload = vec![0u8; min_size];
            for (idx, b) in payload.iter_mut().enumerate() {
                *b = ((idx.wrapping_mul(31).wrapping_add(17)) & 0xff) as u8;
            }
            zw.write_all(&payload).unwrap();
            zw.finish().unwrap();
        }
        cursor.into_inner()
    }

    #[tokio::test]
    async fn cancel_during_verification_stops_promptly_preserves_destination_and_allows_fresh_save()
    {
        use std::sync::atomic::AtomicU64;

        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("solstone-journal-cancel-verify.zip");
        fs::write(&dest, b"preexisting-archive-bytes").unwrap();

        let payload_a = vec![0x5Au8; 16 * 1024 * 1024];
        let sha_a = observer_log::sha256_hex(&payload_a);
        let etag_a = format!("\"sha256-{sha_a}\"");

        let payload_b = build_valid_zip_bytes(512 * 1024);
        let sha_b = observer_log::sha256_hex(&payload_b);
        let etag_b = format!("\"sha256-{sha_b}\"");

        let opener = Arc::new(ScriptedOpener::new(vec![
            Ok(OpenedExportStream {
                status: 200,
                content_range: None,
                etag: Some(etag_a.clone()),
                transport: "local",
                body: Box::new(ScriptedStream {
                    chunks: VecDeque::from(vec![Ok(payload_a.clone())]),
                }),
            }),
            Ok(OpenedExportStream {
                status: 200,
                content_range: None,
                etag: Some(etag_b.clone()),
                transport: "local",
                body: Box::new(ScriptedStream {
                    chunks: VecDeque::from(vec![Ok(payload_b.clone())]),
                }),
            }),
        ]));
        let manager = ExportTransferManager::new(
            opener,
            Arc::new(|_| Some(512 * 1024 * 1024)),
            Arc::new(|from, to| fs::rename(from, to)),
            Arc::new(|| None),
        );

        let max_bytes_verified = Arc::new(AtomicU64::new(0));
        let cancel_fired = Arc::new(AtomicBool::new(false));
        let mgr_for_hook = manager.clone();
        let max_clone = Arc::clone(&max_bytes_verified);
        let fired_clone = Arc::clone(&cancel_fired);
        manager.set_verify_chunk_hook(Arc::new(move |bytes_read| {
            max_clone.fetch_max(bytes_read, Ordering::AcqRel);
            if !fired_clone.swap(true, Ordering::AcqRel) {
                let _ = mgr_for_hook.cancel("cv1", Some("exp_win_verify_cancel"));
            }
        }));

        let _ = manager
            .start(StartExportParams {
                request_id: "s_vc1".into(),
                export_id: "exp_win_verify_cancel".into(),
                download_path: "/app/export/api/ready/exp_win_verify_cancel/download".into(),
                filename: "solstone-journal-cancel-verify.zip".into(),
                size_bytes: payload_a.len() as u64,
                sha256: sha_a,
                etag: etag_a,
                destination_path: dest.clone(),
            })
            .await;

        let cancelled = wait_phase(&manager, "exp_win_verify_cancel", "cancelled").await;
        assert_eq!(cancelled.phase, "cancelled");

        // Wait briefly for the blocking worker to drop its File handle and unlink the .part file.
        for _ in 0..100 {
            if !has_any_part_file(tmp.path(), "exp_win_verify_cancel") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            !has_any_part_file(tmp.path(), "exp_win_verify_cancel"),
            "cancelled worker must release handle and remove .part file"
        );
        let verified_before_stop = max_bytes_verified.load(Ordering::Acquire);
        assert!(
            verified_before_stop > 0 && verified_before_stop <= 1024 * 1024,
            "verification should stop promptly after cancel, read {verified_before_stop} bytes"
        );
        assert_eq!(fs::read(&dest).unwrap(), b"preexisting-archive-bytes");

        // Now start a fresh save to the same destination and verify the resulting ZIP reopens.
        *manager.inner.verify_chunk_hook.lock().unwrap() = None;
        let _ = manager
            .start(StartExportParams {
                request_id: "s_vc2".into(),
                export_id: "exp_win_verify_cancel".into(),
                download_path: "/app/export/api/ready/exp_win_verify_cancel/download".into(),
                filename: "solstone-journal-cancel-verify.zip".into(),
                size_bytes: payload_b.len() as u64,
                sha256: sha_b,
                etag: etag_b,
                destination_path: dest.clone(),
            })
            .await;
        let saved = wait_phase(&manager, "exp_win_verify_cancel", "saved").await;
        assert!(saved.verified);
        assert_eq!(fs::read(&dest).unwrap(), payload_b);
        assert!(!has_any_part_file(tmp.path(), "exp_win_verify_cancel"));
        let mut archive = zip::ZipArchive::new(File::open(&dest).unwrap()).unwrap();
        assert_eq!(archive.len(), 1);
        assert_eq!(archive.by_index(0).unwrap().name().unwrap(), "journal.txt");
    }

    #[tokio::test]
    async fn restart_recovers_on_disk_partial_from_first_and_later_generations_under_tight_space() {
        let payload: Vec<u8> = (0usize..(16 * 1024 * 1024))
            .map(|i| ((i.wrapping_mul(17).wrapping_add(3)) & 0xff) as u8)
            .collect();
        let sha = observer_log::sha256_hex(&payload);
        let etag = format!("\"sha256-{sha}\"");
        let split = 12 * 1024 * 1024usize;
        let tight_free: u64 = 5 * 1024 * 1024; // 4 MiB remaining + 1 MiB reserve

        for (case_label, prior_cancel) in [("gen1", false), ("later_gen", true)] {
            let tmp = tempfile::tempdir().unwrap();
            let dest = tmp.path().join("restart.zip");
            let export_id = format!("exp_win_restart_{case_label}");
            let params = StartExportParams {
                request_id: format!("r_{case_label}"),
                export_id: export_id.clone(),
                download_path: format!("/app/export/api/ready/{export_id}/download"),
                filename: "restart.zip".into(),
                size_bytes: payload.len() as u64,
                sha256: sha.clone(),
                etag: etag.clone(),
                destination_path: dest.clone(),
            };

            let mut responses = Vec::new();
            if prior_cancel {
                responses.push(Ok(OpenedExportStream {
                    status: 200,
                    content_range: None,
                    etag: Some(etag.clone()),
                    transport: "local",
                    body: Box::new(ScriptedStream {
                        chunks: VecDeque::from(vec![Ok(payload[..65536].to_vec()), Err(())]),
                    }),
                }));
            }
            responses.push(Ok(OpenedExportStream {
                status: 200,
                content_range: None,
                etag: Some(etag.clone()),
                transport: "local",
                body: Box::new(ScriptedStream {
                    chunks: VecDeque::from(vec![Ok(payload[..split].to_vec()), Err(())]),
                }),
            }));

            let daemon1 = ExportTransferManager::new(
                Arc::new(ScriptedOpener::new(responses)),
                Arc::new(|_| Some(512 * 1024 * 1024)),
                Arc::new(|from, to| fs::rename(from, to)),
                Arc::new(|| None),
            );
            if prior_cancel {
                let _ = daemon1.start(params.clone()).await;
                let _ = wait_phase(&daemon1, &export_id, "paused").await;
                let _ = daemon1.cancel("c0", Some(&export_id));
            }

            let _ = daemon1.start(params.clone()).await;
            let paused = wait_phase(&daemon1, &export_id, "paused").await;
            assert_eq!(paused.bytes_written, split as u64, "case={case_label}");
            drop(daemon1); // Simulate app restart: in-memory active state is gone.

            let daemon2_opener = Arc::new(ScriptedOpener::new(vec![Ok(OpenedExportStream {
                status: 206,
                content_range: Some(format!(
                    "bytes {split}-{}/{}",
                    payload.len() - 1,
                    payload.len()
                )),
                etag: Some(etag.clone()),
                transport: "local",
                body: Box::new(ScriptedStream {
                    chunks: VecDeque::from(vec![Ok(payload[split..].to_vec())]),
                }),
            })]));
            let daemon2 = ExportTransferManager::new(
                daemon2_opener.clone(),
                Arc::new(move |_| Some(tight_free)),
                Arc::new(|from, to| fs::rename(from, to)),
                Arc::new(|| None),
            );
            let resp = daemon2.start(params).await;
            assert_eq!(resp.phase, "transferring", "case={case_label}: {resp:?}");
            assert_eq!(resp.bytes_written, split as u64, "case={case_label}");

            let saved = wait_phase(&daemon2, &export_id, "saved").await;
            assert!(saved.verified, "case={case_label}");
            assert_eq!(fs::read(&dest).unwrap(), payload, "case={case_label}");
            assert!(
                !has_any_part_file(tmp.path(), &export_id),
                "case={case_label}"
            );

            let seen = daemon2_opener.seen_ranges.lock().unwrap().clone();
            assert_eq!(seen.len(), 1, "case={case_label}");
            assert_eq!(
                seen[0],
                (Some(format!("bytes={split}-")), Some(etag.clone())),
                "case={case_label}"
            );
        }
    }
}
