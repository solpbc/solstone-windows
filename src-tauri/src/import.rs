// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! File import is a sibling custody loop; closing its page never stops it.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use observer_model::i18n::message;
use observer_model::{PairingPhase, SyncSnapshot};
use pl_transport_win::import_client::{
    hash_file_sha256, read_file_chunk, Handoff, ImportFile, ImportReply, ReconcileReply,
    ResumableConfig, ResumableProbe, ResumableUploadSession, RetryLadder, UploadChunkReply,
    UploadCommitReply, UploadCreateReply, UploadStatusReply,
};
use pl_transport_win::import_store::{
    self, Intake, IntakeRefusal, ReconcileOutcome, Refusal, ResumableUploadState, ReviewState,
    SavedProgress, Store, UploadPhase, VersionGate,
};
use pl_transport_win::ObserverClient;
use serde::Serialize;
use tauri::{Emitter, Manager};
use tokio::sync::{watch, Notify};

pub use pl_transport_win::import_store::{Document, Row};

static SERVICE: OnceLock<Arc<Service>> = OnceLock::new();
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(30);
const MAX_CHUNK_CHECKSUM_RETRIES: usize = 3;
/// Conservative size threshold used only when the capability probe itself fails transiently
/// before the journal answers `/app/import/api/sources`, so large files pause instead of
/// falling back to single-POST `/save` while offline.
const PROBE_UNAVAILABLE_PAUSE_THRESHOLD_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PowerState {
    suspended: bool,
    suspend_epoch: u64,
}

#[cfg_attr(not(test), allow(dead_code))]
enum DeliveryStep<T> {
    Completed(T),
    PausedForSleep,
}

enum ActiveStep<T> {
    Completed(T),
    PausedForSleep,
    Cancelled,
}

struct Service {
    store: Mutex<Option<Store>>,
    intake_lock: tokio::sync::Mutex<()>,
    wake: Notify,
    power: watch::Sender<PowerState>,
    cancel: watch::Sender<BTreeSet<String>>,
    changes: watch::Sender<()>,
    identity: Mutex<Option<(String, String)>>,
    auth_blocked: Mutex<Option<String>>,
    /// Cached capability probe keyed by `(pairing_binding, journal_version)`.
    resumable_probe: Mutex<Option<(String, String, Option<ResumableConfig>)>>,
    /// Per-item retry backoff ladders for resumable uploads.
    retry_ladders: Mutex<BTreeMap<String, RetryLadder>>,
    /// Optional shorter next-pass wait scheduled by a paused resumable upload's retry ladder.
    next_backoff: Mutex<Option<Duration>>,
    /// Items delivered this run and the journal row each became, so a journal
    /// page that handed one over can link to it.
    receipts: Mutex<VecDeque<(String, Option<String>)>>,
    counters: Option<source_counters::SharedCounters>,
    reconcile_requested: AtomicBool,
}

const RECEIPTS_KEPT: usize = 256;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn current_sync(app: &tauri::AppHandle) -> SyncSnapshot {
    app.state::<crate::app::AppState>()
        .sync
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

fn document_for(
    store: Option<&Store>,
    sync: &SyncSnapshot,
    journal: &str,
    auth_blocked: bool,
) -> Document {
    import_store::build_document(store, sync, journal, auth_blocked, now_ms())
}

fn current_identity(service: &Service, sync: &SyncSnapshot) -> (String, bool) {
    let journal = service
        .identity
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .filter(|(binding, _)| binding == &sync.pairing.binding)
        .map(|(_, id)| id.clone())
        .unwrap_or_default();
    let blocked = service
        .auth_blocked
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .is_some_and(|b| b == &sync.pairing.binding);
    (journal, blocked)
}

fn record_handoff_receipt(service: &Service, id: String, row: Option<String>) {
    let mut receipts = service.receipts.lock().unwrap_or_else(|p| p.into_inner());
    if receipts.len() == RECEIPTS_KEPT {
        receipts.pop_front();
    }
    receipts.push_back((id, row));
}

fn publish(app: &tauri::AppHandle, service: &Service) {
    let sync = current_sync(app);
    let (journal, blocked) = current_identity(service, &sync);
    let document = document_for(
        service
            .store
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref(),
        &sync,
        &journal,
        blocked,
    );
    {
        let state = app.state::<crate::app::AppState>();
        let mut sync = state.sync.lock().unwrap_or_else(|p| p.into_inner());
        sync.imports_waiting = document.waiting;
        sync.imports_ready_to_review = document.ready_to_review;
        sync.imports_need_owner = document.needs_owner;
    }
    service.changes.send_replace(());
    if let Err(error) = app.emit("import://changed", &document) {
        tracing::warn!(target: "import", %error, "import state could not reach the window");
    }
}

pub fn status(app: &tauri::AppHandle) -> Option<Document> {
    let service = SERVICE.get()?;
    let sync = current_sync(app);
    let (journal, blocked) = current_identity(service, &sync);
    Some(document_for(
        service
            .store
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref(),
        &sync,
        &journal,
        blocked,
    ))
}

pub fn on_changes() -> Option<watch::Receiver<()>> {
    SERVICE.get().map(|s| s.changes.subscribe())
}

/// Request reconciliation of `ready to review` items even when `waiting == 0`.
pub fn trigger_reconcile() {
    if let Some(service) = SERVICE.get() {
        service.reconcile_requested.store(true, Ordering::Release);
        service.wake.notify_one();
    }
}

/// Pause active import delivery and release any awake hold when Windows suspends.
pub fn on_system_suspending() {
    if let Some(service) = SERVICE.get() {
        mark_suspending(&service.power);
    }
}

/// Resume paused import delivery promptly after Windows wakes from sleep.
pub fn on_system_resumed() {
    if let Some(service) = SERVICE.get() {
        mark_resumed(&service.power);
        reset_all_retry_ladders(service);
        service.reconcile_requested.store(true, Ordering::Release);
        service.wake.notify_one();
    }
}

fn next_retry_delay(service: &Service, id: &str) -> Duration {
    let mut ladders = service
        .retry_ladders
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    ladders.entry(id.to_owned()).or_default().next_delay()
}

fn reset_retry_ladder(service: &Service, id: &str) {
    let mut ladders = service
        .retry_ladders
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    ladders.remove(id);
}

fn reset_all_retry_ladders(service: &Service) {
    service
        .retry_ladders
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clear();
    *service
        .next_backoff
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = None;
}

fn schedule_backoff(service: &Service, delay: Duration) {
    let mut slot = service
        .next_backoff
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    *slot = Some(match *slot {
        Some(existing) => existing.min(delay),
        None => delay,
    });
}

fn take_next_wait(service: &Service) -> Duration {
    service
        .next_backoff
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take()
        .unwrap_or(DEFAULT_POLL_INTERVAL)
}

fn mark_item_cancelled(cancel: &watch::Sender<BTreeSet<String>>, id: &str) {
    cancel.send_modify(|set| {
        set.insert(id.to_owned());
    });
}

fn clear_item_cancelled(cancel: &watch::Sender<BTreeSet<String>>, id: &str) {
    cancel.send_modify(|set| {
        set.remove(id);
    });
}

fn mark_suspending(power: &watch::Sender<PowerState>) {
    power.send_modify(|state| {
        state.suspended = true;
        state.suspend_epoch = state.suspend_epoch.wrapping_add(1);
    });
}

fn mark_resumed(power: &watch::Sender<PowerState>) {
    power.send_modify(|state| {
        state.suspended = false;
    });
}

/// Run one item's upload while holding an OS awake guard, cancelling the work
/// and dropping the guard on any exit path or system-suspend transition.
#[cfg_attr(not(test), allow(dead_code))]
async fn run_while_awake<H, F, Fut, T>(
    power_rx: &mut watch::Receiver<PowerState>,
    acquire_hold: F,
    work: Fut,
) -> DeliveryStep<T>
where
    F: FnOnce() -> H,
    Fut: std::future::Future<Output = T>,
{
    let start = *power_rx.borrow_and_update();
    if start.suspended {
        return DeliveryStep::PausedForSleep;
    }
    let _hold = acquire_hold();
    let after_acquire = *power_rx.borrow();
    if after_acquire.suspended || after_acquire.suspend_epoch != start.suspend_epoch {
        return DeliveryStep::PausedForSleep;
    }
    let start_epoch = start.suspend_epoch;
    tokio::select! {
        biased;
        _ = power_rx.wait_for(|state| state.suspended || state.suspend_epoch != start_epoch) => {
            DeliveryStep::PausedForSleep
        }
        outcome = work => DeliveryStep::Completed(outcome),
    }
}

/// Run one item's upload while holding an OS awake guard, aborting immediately
/// and dropping the guard on system suspend or owner cancellation of `item_id`.
async fn run_while_awake_or_cancel<H, F, Fut, T>(
    power_rx: &mut watch::Receiver<PowerState>,
    cancel_rx: &mut watch::Receiver<BTreeSet<String>>,
    item_id: &str,
    acquire_hold: F,
    work: Fut,
) -> ActiveStep<T>
where
    F: FnOnce() -> H,
    Fut: std::future::Future<Output = T>,
{
    if cancel_rx.borrow_and_update().contains(item_id) {
        return ActiveStep::Cancelled;
    }
    let start = *power_rx.borrow_and_update();
    if start.suspended {
        return ActiveStep::PausedForSleep;
    }
    let _hold = acquire_hold();
    if cancel_rx.borrow().contains(item_id) {
        return ActiveStep::Cancelled;
    }
    let after_acquire = *power_rx.borrow();
    if after_acquire.suspended || after_acquire.suspend_epoch != start.suspend_epoch {
        return ActiveStep::PausedForSleep;
    }
    let start_epoch = start.suspend_epoch;
    tokio::select! {
        biased;
        _ = cancel_rx.wait_for(|set| set.contains(item_id)) => ActiveStep::Cancelled,
        _ = power_rx.wait_for(|state| state.suspended || state.suspend_epoch != start_epoch) => {
            ActiveStep::PausedForSleep
        }
        outcome = work => ActiveStep::Completed(outcome),
    }
}

async fn wait_for_next_pass(
    power_rx: &mut watch::Receiver<PowerState>,
    wake: &Notify,
    poll_interval: Duration,
) {
    if power_rx.borrow().suspended {
        let _ = power_rx.wait_for(|state| !state.suspended).await;
        return;
    }
    let suspended = tokio::select! {
        _ = wake.notified() => false,
        _ = tokio::time::sleep(poll_interval) => false,
        res = power_rx.wait_for(|state| state.suspended) => res.is_ok(),
    };
    if suspended {
        let _ = power_rx.wait_for(|state| !state.suspended).await;
    }
}

pub fn start(app: tauri::AppHandle, counters: Option<source_counters::SharedCounters>) {
    let root = platform_win::local_data_root().join("import");
    let store = match Store::open(root) {
        Ok(store) => Some(store),
        Err(error) => {
            tracing::warn!(target: "import", %error, "import custody could not open");
            None
        }
    };
    let (power, _) = watch::channel(PowerState::default());
    let (cancel, _) = watch::channel(BTreeSet::new());
    let (changes, _) = watch::channel(());
    let service = Arc::new(Service {
        store: Mutex::new(store),
        intake_lock: tokio::sync::Mutex::new(()),
        wake: Notify::new(),
        power,
        cancel,
        changes,
        identity: Mutex::new(None),
        auth_blocked: Mutex::new(None),
        resumable_probe: Mutex::new(None),
        retry_ladders: Mutex::new(BTreeMap::new()),
        next_backoff: Mutex::new(None),
        receipts: Mutex::new(VecDeque::new()),
        counters,
        reconcile_requested: AtomicBool::new(true),
    });
    let _ = SERVICE.set(service.clone());
    let wake_on_network = Arc::downgrade(&service);
    platform_win::on_network_available(Arc::new(move || {
        if let Some(service) = wake_on_network.upgrade() {
            if !service.power.borrow().suspended {
                reset_all_retry_ladders(&service);
                service.reconcile_requested.store(true, Ordering::Release);
                service.wake.notify_one();
            }
        }
    }));
    let mut power_rx = service.power.subscribe();
    let mut cancel_rx = service.cancel.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            if let Err(error) = deliver_pass(&app, &service, &mut power_rx, &mut cancel_rx).await {
                tracing::warn!(target: "import", %error, "import delivery retained its local copy");
            }
            publish(&app, &service);
            let wait = take_next_wait(&service);
            wait_for_next_pass(&mut power_rx, &service.wake, wait).await;
        }
    });
}

async fn client(app: &tauri::AppHandle) -> Option<Arc<ObserverClient>> {
    let state = app.state::<crate::app::AppState>();
    pl_transport_win::access::load_current_client(&state.credential_access).await
}

async fn reconcile_reviews(
    app: &tauri::AppHandle,
    service: &Service,
    client: &Arc<ObserverClient>,
    binding: &str,
    journal: &str,
) -> std::io::Result<()> {
    let candidates = {
        let store = service.store.lock().unwrap_or_else(|p| p.into_inner());
        let Some(store) = store.as_ref() else {
            return Ok(());
        };
        store.review_candidates(journal)
    };
    for (item_id, import_id) in candidates {
        if service.power.borrow().suspended || !same_client(app, client).await {
            break;
        }
        match client.import_status(&import_id).await {
            Ok(ReconcileReply::AuthStop) => {
                *service
                    .auth_blocked
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) = Some(binding.to_owned());
                tracing::warn!(target: "import", "journal refused import reconciliation authentication");
                break;
            }
            Ok(reply) => {
                let outcome = {
                    let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
                    let Some(store) = guard.as_mut() else {
                        break;
                    };
                    store.apply_reconcile(&item_id, &import_id, journal, reply, now_ms())?
                };
                match outcome {
                    ReconcileOutcome::Confirmed => {
                        record_handoff_receipt(service, item_id, Some(import_id));
                        publish(app, service);
                    }
                    ReconcileOutcome::Expired => {
                        publish(app, service);
                    }
                    ReconcileOutcome::Unchanged | ReconcileOutcome::Stale => {}
                }
            }
            Err(error) => {
                tracing::warn!(target: "import", %error, item = %item_id, "import reconciliation left copy unchanged");
            }
        }
    }
    Ok(())
}

enum CachedProbe {
    Supported(ResumableConfig),
    Unsupported,
    AuthStop,
    Transient,
}

async fn probe_resumable_cached(
    service: &Service,
    client: &ObserverClient,
    binding: &str,
    version: &str,
) -> CachedProbe {
    if let Some((cached_binding, cached_version, cached)) = service
        .resumable_probe
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .cloned()
    {
        if cached_binding == binding && cached_version == version {
            return match cached {
                Some(cfg) => CachedProbe::Supported(cfg),
                None => CachedProbe::Unsupported,
            };
        }
    }
    match client.probe_resumable_import().await {
        Ok(ResumableProbe::Supported(cfg)) => {
            *service
                .resumable_probe
                .lock()
                .unwrap_or_else(|p| p.into_inner()) =
                Some((binding.to_owned(), version.to_owned(), Some(cfg)));
            CachedProbe::Supported(cfg)
        }
        Ok(ResumableProbe::Unsupported) => {
            *service
                .resumable_probe
                .lock()
                .unwrap_or_else(|p| p.into_inner()) =
                Some((binding.to_owned(), version.to_owned(), None));
            CachedProbe::Unsupported
        }
        Ok(ResumableProbe::AuthStop) => CachedProbe::AuthStop,
        Ok(ResumableProbe::Retry) | Err(_) => CachedProbe::Transient,
    }
}

fn cache_resumable_unsupported(service: &Service, binding: &str, version: &str) {
    *service
        .resumable_probe
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = Some((binding.to_owned(), version.to_owned(), None));
}

fn spawn_best_effort_abort(app: &tauri::AppHandle, upload_id: String, expected_journal: String) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(client) = client(&app).await else {
            return;
        };
        if client.credential().instance_id != expected_journal || !client.gate_open() {
            return;
        }
        if let Err(error) = client.abort_upload(&upload_id).await {
            tracing::warn!(target: "import", %error, upload_id = %upload_id, "best-effort upload abort failed");
        }
    });
}

enum ResumableStageOutcome {
    Staged(pl_transport_win::import_client::SavedImport),
    AlreadyDelivered {
        import_id: Option<String>,
    },
    FallbackSinglePost,
    PausedTransient,
    PairingChanged,
    AuthStop,
    InsufficientSpace,
    IntegrityFailed {
        reason_code: &'static str,
    },
    Expired,
    UnreadableLocalCopy,
    Refused {
        sentence: String,
        retryable: bool,
        reason_code: Option<String>,
        kind: Option<String>,
    },
}

fn persist_item_snapshot(service: &Service, item: &import_store::Item) -> std::io::Result<()> {
    let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(store) = guard.as_mut() {
        if store.items.contains_key(&item.id) {
            store.put(item.clone())?;
        }
    }
    Ok(())
}

fn set_active_phase(service: &Service, phase: UploadPhase) {
    let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(store) = guard.as_mut() {
        store.active_phase = Some(phase);
    }
}

async fn stage_resumable_upload(
    app: &tauri::AppHandle,
    service: &Service,
    client: &Arc<ObserverClient>,
    journal: &str,
    item: &mut import_store::Item,
    payload: &std::path::Path,
    default_chunk_bytes: u64,
) -> std::io::Result<ResumableStageOutcome> {
    // 1. Compute whole-file SHA-256 once from the stored custody copy.
    let sha256 = if let Some(existing) = item.sha256.clone() {
        existing
    } else {
        let path_for_hash = payload.to_path_buf();
        let hashed = tauri::async_runtime::spawn_blocking(move || hash_file_sha256(&path_for_hash))
            .await
            .map_err(std::io::Error::other)?;
        match hashed {
            Ok((digest, exact_len)) => {
                if exact_len == 0 {
                    return Ok(ResumableStageOutcome::UnreadableLocalCopy);
                }
                item.size = exact_len;
                item.sha256 = Some(digest.clone());
                persist_item_snapshot(service, item)?;
                digest
            }
            Err(error) => {
                tracing::warn!(target: "import", %error, item = %item.id, "could not hash custody file");
                return Ok(ResumableStageOutcome::UnreadableLocalCopy);
            }
        }
    };

    if !same_client(app, client).await {
        return Ok(ResumableStageOutcome::PairingChanged);
    }

    let mut session = match ResumableUploadSession::open(client.clone()).await {
        Ok(session) => session,
        Err(error) => {
            tracing::warn!(target: "import", %error, item = %item.id, "could not open resumable upload bridge");
            return Ok(ResumableStageOutcome::PausedTransient);
        }
    };

    // 2. Resync an existing remote upload cursor via GET, or create a new session via POST.
    let (mut upload_id, mut next_offset, mut chunk_bytes) =
        if let Some(existing) = item.upload_for(journal).cloned() {
            match session.get_status(&existing.upload_id).await {
                Ok(UploadStatusReply::Open {
                    upload_id,
                    next_offset,
                    chunk_bytes,
                    ..
                }) => {
                    let clamped = next_offset.min(item.size);
                    item.upload = Some(ResumableUploadState {
                        journal: journal.to_owned(),
                        upload_id: upload_id.clone(),
                        next_offset: clamped,
                        total_bytes: item.size,
                        chunk_bytes,
                    });
                    persist_item_snapshot(service, item)?;
                    reset_retry_ladder(service, &item.id);
                    publish(app, service);
                    (upload_id, clamped, chunk_bytes)
                }
                Ok(UploadStatusReply::UploadNotFound) => {
                    // If all bytes had reached the journal before disconnect, a prior commit
                    // may have already succeeded whose response was lost; check via POST first.
                    if existing.next_offset >= item.size {
                        let import_file = ImportFile {
                            id: &item.id,
                            name: &item.name,
                            content_type: &item.mime,
                            path: payload,
                            handoff: item.handoff.as_ref(),
                        };
                        match session
                            .create_upload(&import_file, item.size, &sha256)
                            .await
                        {
                            Ok(UploadCreateReply::AlreadyStaged(saved)) => {
                                session.close().await;
                                return Ok(ResumableStageOutcome::Staged(saved));
                            }
                            Ok(UploadCreateReply::AlreadyImported { import_id }) => {
                                session.close().await;
                                return Ok(ResumableStageOutcome::AlreadyDelivered { import_id });
                            }
                            Ok(UploadCreateReply::Open(open)) => {
                                // Journal had purged the expired session and created a brand-new
                                // one; abort it and surface the expiry fault cleanly.
                                let _ = session.abort_upload(&open.upload_id).await;
                                session.close().await;
                                return Ok(ResumableStageOutcome::Expired);
                            }
                            Ok(UploadCreateReply::AuthStop) => {
                                session.close().await;
                                return Ok(ResumableStageOutcome::AuthStop);
                            }
                            Ok(UploadCreateReply::Retry) | Err(_) => {
                                session.close().await;
                                return Ok(ResumableStageOutcome::PausedTransient);
                            }
                            _ => {
                                session.close().await;
                                return Ok(ResumableStageOutcome::Expired);
                            }
                        }
                    }
                    session.close().await;
                    return Ok(ResumableStageOutcome::Expired);
                }
                Ok(UploadStatusReply::AuthStop) => {
                    session.close().await;
                    return Ok(ResumableStageOutcome::AuthStop);
                }
                Ok(UploadStatusReply::Retry) | Err(_) => {
                    session.close().await;
                    return Ok(ResumableStageOutcome::PausedTransient);
                }
            }
        } else {
            let create_reply = {
                let import_file = ImportFile {
                    id: &item.id,
                    name: &item.name,
                    content_type: &item.mime,
                    path: payload,
                    handoff: item.handoff.as_ref(),
                };
                session
                    .create_upload(&import_file, item.size, &sha256)
                    .await
            };
            match create_reply {
                Ok(UploadCreateReply::Open(open)) => {
                    let clamped = open.next_offset.min(item.size);
                    let chunk = open.chunk_bytes.max(1);
                    item.upload = Some(ResumableUploadState {
                        journal: journal.to_owned(),
                        upload_id: open.upload_id.clone(),
                        next_offset: clamped,
                        total_bytes: item.size,
                        chunk_bytes: chunk,
                    });
                    persist_item_snapshot(service, item)?;
                    reset_retry_ladder(service, &item.id);
                    publish(app, service);
                    (open.upload_id, clamped, chunk)
                }
                Ok(UploadCreateReply::AlreadyStaged(saved)) => {
                    session.close().await;
                    return Ok(ResumableStageOutcome::Staged(saved));
                }
                Ok(UploadCreateReply::AlreadyImported { import_id }) => {
                    session.close().await;
                    return Ok(ResumableStageOutcome::AlreadyDelivered { import_id });
                }
                Ok(UploadCreateReply::Unsupported) => {
                    session.close().await;
                    return Ok(ResumableStageOutcome::FallbackSinglePost);
                }
                Ok(UploadCreateReply::InsufficientSpace { .. }) => {
                    session.close().await;
                    return Ok(ResumableStageOutcome::InsufficientSpace);
                }
                Ok(UploadCreateReply::AuthStop) => {
                    session.close().await;
                    return Ok(ResumableStageOutcome::AuthStop);
                }
                Ok(UploadCreateReply::Retry) | Err(_) => {
                    session.close().await;
                    return Ok(ResumableStageOutcome::PausedTransient);
                }
                Ok(UploadCreateReply::Refused {
                    sentence,
                    retryable,
                    reason_code,
                    kind,
                }) => {
                    session.close().await;
                    return Ok(ResumableStageOutcome::Refused {
                        sentence,
                        retryable,
                        reason_code,
                        kind,
                    });
                }
            }
        };

    if chunk_bytes == 0 {
        chunk_bytes = default_chunk_bytes.max(1);
    }

    // 3. Send chunks sequentially from `next_offset`, then commit.
    let mut chunk_bad_retries = 0usize;
    loop {
        while next_offset < item.size {
            if !same_client(app, client).await {
                session.close().await;
                return Ok(ResumableStageOutcome::PairingChanged);
            }
            let len = usize::try_from((item.size - next_offset).min(chunk_bytes))
                .expect("bounded chunk length");
            let path_for_chunk = payload.to_path_buf();
            let read_offset = next_offset;
            let bytes = match tauri::async_runtime::spawn_blocking(move || {
                read_file_chunk(&path_for_chunk, read_offset, len)
            })
            .await
            .map_err(std::io::Error::other)?
            {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(target: "import", %error, item = %item.id, offset = read_offset, "custody file became unreadable mid-upload");
                    session.close().await;
                    return Ok(ResumableStageOutcome::UnreadableLocalCopy);
                }
            };

            match session.put_chunk(&upload_id, next_offset, &bytes).await {
                Ok(UploadChunkReply::Accepted {
                    next_offset: confirmed,
                    ..
                }) => {
                    next_offset = confirmed.min(item.size);
                    chunk_bad_retries = 0;
                    reset_retry_ladder(service, &item.id);
                    if let Some(state) = item.upload.as_mut() {
                        state.next_offset = next_offset;
                    }
                    persist_item_snapshot(service, item)?;
                    publish(app, service);
                }
                Ok(UploadChunkReply::OffsetMismatch {
                    next_offset: Some(server_offset),
                }) => {
                    next_offset = server_offset.min(item.size);
                    chunk_bad_retries = 0;
                    if let Some(state) = item.upload.as_mut() {
                        state.next_offset = next_offset;
                    }
                    persist_item_snapshot(service, item)?;
                    publish(app, service);
                }
                Ok(UploadChunkReply::OffsetMismatch { next_offset: None }) => {
                    match session.get_status(&upload_id).await {
                        Ok(UploadStatusReply::Open {
                            upload_id: refreshed_id,
                            next_offset: server_offset,
                            chunk_bytes: server_chunk,
                            ..
                        }) => {
                            upload_id = refreshed_id;
                            next_offset = server_offset.min(item.size);
                            chunk_bytes = server_chunk.max(1);
                            chunk_bad_retries = 0;
                            if let Some(state) = item.upload.as_mut() {
                                state.upload_id = upload_id.clone();
                                state.next_offset = next_offset;
                                state.chunk_bytes = chunk_bytes;
                            }
                            persist_item_snapshot(service, item)?;
                            publish(app, service);
                        }
                        Ok(UploadStatusReply::UploadNotFound) => {
                            session.close().await;
                            return Ok(ResumableStageOutcome::Expired);
                        }
                        Ok(UploadStatusReply::AuthStop) => {
                            session.close().await;
                            return Ok(ResumableStageOutcome::AuthStop);
                        }
                        Ok(UploadStatusReply::Retry) | Err(_) => {
                            session.close().await;
                            return Ok(ResumableStageOutcome::PausedTransient);
                        }
                    }
                }
                Ok(UploadChunkReply::ChunkChecksumMismatch) => {
                    chunk_bad_retries += 1;
                    if chunk_bad_retries > MAX_CHUNK_CHECKSUM_RETRIES {
                        session.close().await;
                        return Ok(ResumableStageOutcome::IntegrityFailed {
                            reason_code: "chunk_checksum_mismatch",
                        });
                    }
                }
                Ok(UploadChunkReply::UploadNotFound) => {
                    session.close().await;
                    return Ok(ResumableStageOutcome::Expired);
                }
                Ok(UploadChunkReply::InsufficientSpace { .. }) => {
                    session.close().await;
                    return Ok(ResumableStageOutcome::InsufficientSpace);
                }
                Ok(UploadChunkReply::AuthStop) => {
                    session.close().await;
                    return Ok(ResumableStageOutcome::AuthStop);
                }
                Ok(UploadChunkReply::Retry) | Err(_) => {
                    session.close().await;
                    return Ok(ResumableStageOutcome::PausedTransient);
                }
                Ok(UploadChunkReply::Refused {
                    sentence,
                    retryable,
                    reason_code,
                    kind,
                }) => {
                    session.close().await;
                    return Ok(ResumableStageOutcome::Refused {
                        sentence,
                        retryable,
                        reason_code,
                        kind,
                    });
                }
            }
        }

        if !same_client(app, client).await {
            session.close().await;
            return Ok(ResumableStageOutcome::PairingChanged);
        }

        // 4. Commit and verify whole-file SHA-256 on the journal.
        set_active_phase(service, UploadPhase::Checking);
        publish(app, service);

        match session.commit_upload(&upload_id, &sha256).await {
            Ok(UploadCommitReply::Saved(saved)) => {
                session.close().await;
                return Ok(ResumableStageOutcome::Staged(saved));
            }
            Ok(UploadCommitReply::Incomplete { next_offset: srv }) => {
                set_active_phase(service, UploadPhase::Sending);
                if let Some(offset) = srv {
                    next_offset = offset.min(item.size);
                    if let Some(state) = item.upload.as_mut() {
                        state.next_offset = next_offset;
                    }
                    persist_item_snapshot(service, item)?;
                    publish(app, service);
                    continue;
                }
                match session.get_status(&upload_id).await {
                    Ok(UploadStatusReply::Open {
                        next_offset: server_offset,
                        ..
                    }) => {
                        next_offset = server_offset.min(item.size);
                        if let Some(state) = item.upload.as_mut() {
                            state.next_offset = next_offset;
                        }
                        persist_item_snapshot(service, item)?;
                        publish(app, service);
                        continue;
                    }
                    Ok(UploadStatusReply::UploadNotFound) => {
                        session.close().await;
                        return Ok(ResumableStageOutcome::Expired);
                    }
                    Ok(UploadStatusReply::AuthStop) => {
                        session.close().await;
                        return Ok(ResumableStageOutcome::AuthStop);
                    }
                    Ok(UploadStatusReply::Retry) | Err(_) => {
                        session.close().await;
                        return Ok(ResumableStageOutcome::PausedTransient);
                    }
                }
            }
            Ok(UploadCommitReply::WholeFileChecksumMismatch { .. }) => {
                session.close().await;
                return Ok(ResumableStageOutcome::IntegrityFailed {
                    reason_code: "whole_file_checksum_mismatch",
                });
            }
            Ok(UploadCommitReply::UploadNotFound) => {
                // Recover a lost-response commit via idempotent POST /uploads if already staged.
                let import_file = ImportFile {
                    id: &item.id,
                    name: &item.name,
                    content_type: &item.mime,
                    path: payload,
                    handoff: item.handoff.as_ref(),
                };
                match session
                    .create_upload(&import_file, item.size, &sha256)
                    .await
                {
                    Ok(UploadCreateReply::AlreadyStaged(saved)) => {
                        session.close().await;
                        return Ok(ResumableStageOutcome::Staged(saved));
                    }
                    Ok(UploadCreateReply::AlreadyImported { import_id }) => {
                        session.close().await;
                        return Ok(ResumableStageOutcome::AlreadyDelivered { import_id });
                    }
                    Ok(UploadCreateReply::Open(open)) => {
                        let _ = session.abort_upload(&open.upload_id).await;
                        session.close().await;
                        return Ok(ResumableStageOutcome::Expired);
                    }
                    Ok(UploadCreateReply::AuthStop) => {
                        session.close().await;
                        return Ok(ResumableStageOutcome::AuthStop);
                    }
                    Ok(UploadCreateReply::Retry) | Err(_) => {
                        session.close().await;
                        return Ok(ResumableStageOutcome::PausedTransient);
                    }
                    _ => {
                        session.close().await;
                        return Ok(ResumableStageOutcome::Expired);
                    }
                }
            }
            Ok(UploadCommitReply::InsufficientSpace { .. }) => {
                session.close().await;
                return Ok(ResumableStageOutcome::InsufficientSpace);
            }
            Ok(UploadCommitReply::AuthStop) => {
                session.close().await;
                return Ok(ResumableStageOutcome::AuthStop);
            }
            Ok(UploadCommitReply::Retry) | Err(_) => {
                session.close().await;
                return Ok(ResumableStageOutcome::PausedTransient);
            }
            Ok(UploadCommitReply::Refused {
                sentence,
                retryable,
                reason_code,
                kind,
            }) => {
                session.close().await;
                return Ok(ResumableStageOutcome::Refused {
                    sentence,
                    retryable,
                    reason_code,
                    kind,
                });
            }
        }
    }
}

async fn deliver_pass(
    app: &tauri::AppHandle,
    service: &Service,
    power_rx: &mut watch::Receiver<PowerState>,
    cancel_rx: &mut watch::Receiver<BTreeSet<String>>,
) -> std::io::Result<()> {
    if power_rx.borrow().suspended {
        return Ok(());
    }
    let sync = current_sync(app);
    let Some(client) = client(app).await else {
        return Ok(());
    };
    let binding = client.journal_identity().client_cert_sha256;
    if binding == sync.pairing.binding {
        *service.identity.lock().unwrap_or_else(|p| p.into_inner()) =
            Some((binding.clone(), client.credential().instance_id.clone()));
    }
    if service
        .auth_blocked
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        == Some(&binding)
    {
        return Ok(());
    }
    if sync.pairing.phase != PairingPhase::Paired
        || !client.gate_open()
        || client.journal_identity().client_cert_sha256 != sync.pairing.binding
    {
        return Ok(());
    }
    if client.refusal_stop().is_some() {
        return Ok(());
    }
    let journal = client.credential().instance_id.clone();

    // Re-pairing to a different journal instance resets foreign review holds and
    // discards remote upload cursors bound to the prior journal while keeping staged files.
    let reset_foreign = {
        let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(store) = guard.as_mut() {
            let reviews = store.reset_foreign_reviews(&journal)?;
            let uploads = store.reset_foreign_uploads(&journal)?;
            reviews || uploads
        } else {
            false
        }
    };
    if reset_foreign {
        publish(app, service);
    }

    // Reconcile `ready to review` items whenever triggered (close/blur, focus, reconnect, startup),
    // even when `waiting == 0`.
    if service.reconcile_requested.swap(false, Ordering::AcqRel) {
        reconcile_reviews(app, service, &client, &binding, &journal).await?;
        if service
            .auth_blocked
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            == Some(&binding)
        {
            return Ok(());
        }
    }

    if power_rx.borrow().suspended {
        return Ok(());
    }
    let mut client = client;
    let mut sync = current_sync(app);
    if !sync.journal_version_fresh
        && sync.pairing.phase == PairingPhase::Paired
        && client.journal_identity().client_cert_sha256 == sync.pairing.binding
        && has_pending_delivery_items(service, &journal)
        && client.check_journal().await.is_ok()
    {
        let fresh_now = current_sync(app).journal_version_fresh;
        if !fresh_now {
            let state = app.state::<crate::app::AppState>();
            let guard = state.credential_access.lock().await;
            if let Some(access) = guard
                .as_ref()
                .filter(|access| Arc::ptr_eq(&access.client_slot().load(), &client))
            {
                access
                    .post_connect()
                    .note_journal_answered(access.post_connect_token(), false);
            }
            drop(guard);
            for _ in 0..20 {
                tokio::time::sleep(Duration::from_millis(100)).await;
                if current_sync(app).journal_version_fresh {
                    break;
                }
            }
        }
        // Post-connect access/endpoint refresh may have rotated the client Arc on the
        // same paired binding; reload the live client if it still matches this binding.
        let Some(reloaded) = crate::import::client(app).await else {
            return Ok(());
        };
        if !reloaded.gate_open()
            || reloaded.refusal_stop().is_some()
            || reloaded.journal_identity().client_cert_sha256 != binding
        {
            return Ok(());
        }
        client = reloaded;
        sync = current_sync(app);
    }
    if sync.pairing.phase != PairingPhase::Paired
        || client.journal_identity().client_cert_sha256 != sync.pairing.binding
    {
        return Ok(());
    }
    if import_store::version_gate(sync.journal_version.as_deref(), sync.journal_version_fresh)
        != VersionGate::Supported
    {
        return Ok(());
    }
    let version = sync.journal_version.clone().unwrap_or_default();
    let ids: Vec<_> = {
        let store = service.store.lock().unwrap_or_else(|p| p.into_inner());
        let Some(store) = store.as_ref() else {
            return Ok(());
        };
        let mut items: Vec<_> = store
            .items
            .values()
            .filter(|item| {
                !item.refused_here(&journal, &version)
                    && item.review_for(&journal).is_none()
                    && store.confirming_remove.as_deref() != Some(&item.id)
            })
            .collect();
        items.sort_by_key(|item| item.added);
        items.into_iter().map(|item| item.id.clone()).collect()
    };
    if ids.is_empty() {
        return Ok(());
    }
    let mut probe = probe_resumable_cached(service, &client, &binding, &version).await;
    if matches!(probe, CachedProbe::AuthStop) {
        *service
            .auth_blocked
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(binding.clone());
        tracing::warn!(target: "import", "journal refused import capability probe");
        return Ok(());
    }
    for id in ids {
        if power_rx.borrow().suspended || !same_client(app, &client).await {
            break;
        }
        if cancel_rx.borrow().contains(&id) {
            let removed_upload = {
                let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
                guard
                    .as_mut()
                    .map(|store| store.cancel_upload(&id))
                    .transpose()?
                    .flatten()
            };
            clear_item_cancelled(&service.cancel, &id);
            reset_retry_ladder(service, &id);
            if let Some(u) = removed_upload.filter(|u| u.journal == journal) {
                spawn_best_effort_abort(app, u.upload_id, journal.clone());
            }
            publish(app, service);
            continue;
        }

        let (mut item, payload, use_resumable, resumable_chunk_bytes) = {
            let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
            let Some(store) = guard.as_mut() else {
                break;
            };
            let Some(mut item) = store.items.get(&id).cloned() else {
                continue;
            };
            if store.confirming_remove.as_deref() == Some(&id)
                || item.review_for(&journal).is_some()
            {
                continue;
            }
            let already_staged = item.progress_for(&journal).is_some();
            let has_open_upload = item.upload_for(&journal).is_some();
            let (use_resumable, chunk_bytes) = if already_staged {
                (false, 0)
            } else {
                match probe {
                    CachedProbe::Supported(cfg) if item.size >= cfg.threshold_bytes => {
                        (true, cfg.chunk_bytes)
                    }
                    CachedProbe::Supported(_) if has_open_upload => (
                        true,
                        item.upload_for(&journal)
                            .map_or(4 * 1024 * 1024, |u| u.chunk_bytes),
                    ),
                    CachedProbe::Transient
                        if has_open_upload
                            || item.size >= PROBE_UNAVAILABLE_PAUSE_THRESHOLD_BYTES =>
                    {
                        store.mark_paused(&id);
                        let delay = next_retry_delay(service, &id);
                        schedule_backoff(service, delay);
                        drop(guard);
                        publish(app, service);
                        break;
                    }
                    _ => (false, 0),
                }
            };
            item.may_have_reached = true;
            store.put(item.clone())?;
            store.set_active(
                id.clone(),
                use_resumable,
                use_resumable.then_some(UploadPhase::Sending),
            );
            (item, store.payload(&id), use_resumable, chunk_bytes)
        };
        publish(app, service);

        if use_resumable {
            let stage_step = run_while_awake_or_cancel(
                power_rx,
                cancel_rx,
                &id,
                || {
                    platform_win::power_hold::PowerHold::acquire(&message(
                        "windows.import.rowsending",
                        &[],
                    ))
                },
                stage_resumable_upload(
                    app,
                    service,
                    &client,
                    &journal,
                    &mut item,
                    &payload,
                    resumable_chunk_bytes,
                ),
            )
            .await;

            match stage_step {
                ActiveStep::PausedForSleep => {
                    {
                        let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
                        if let Some(store) = guard.as_mut() {
                            store.clear_active();
                            store.mark_paused(&id);
                        }
                    }
                    tracing::info!(
                        target: "import",
                        item = %id,
                        "resumable import paused for system sleep; will resume from confirmed offset on wake"
                    );
                    publish(app, service);
                    if !power_rx.borrow().suspended {
                        service.wake.notify_one();
                    }
                    break;
                }
                ActiveStep::Cancelled => {
                    let removed_upload = {
                        let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
                        guard
                            .as_mut()
                            .map(|store| store.cancel_upload(&id))
                            .transpose()?
                            .flatten()
                    };
                    clear_item_cancelled(&service.cancel, &id);
                    reset_retry_ladder(service, &id);
                    if let Some(u) = removed_upload.filter(|u| u.journal == journal) {
                        spawn_best_effort_abort(app, u.upload_id, journal.clone());
                    }
                    publish(app, service);
                    continue;
                }
                ActiveStep::Completed(outcome) => match outcome? {
                    ResumableStageOutcome::Staged(saved) => {
                        item.saved = Some(SavedProgress {
                            journal: journal.clone(),
                            saved,
                        });
                        item.upload = None;
                        item.refusal = None;
                        reset_retry_ladder(service, &id);
                        persist_item_snapshot(service, &item)?;
                        // Fall through below to `start_import` with `item.saved` populated.
                    }
                    ResumableStageOutcome::AlreadyDelivered { import_id } => {
                        {
                            let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
                            if let Some(store) = guard.as_mut() {
                                store.clear_active();
                                store.delivered_at(
                                    &id,
                                    &journal,
                                    import_id.as_deref(),
                                    now_ms(),
                                )?;
                            }
                        }
                        reset_retry_ladder(service, &id);
                        record_handoff_receipt(service, id.clone(), import_id);
                        publish(app, service);
                        continue;
                    }
                    ResumableStageOutcome::FallbackSinglePost => {
                        cache_resumable_unsupported(service, &binding, &version);
                        probe = CachedProbe::Unsupported;
                        {
                            let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
                            if let Some(store) = guard.as_mut() {
                                item.upload = None;
                                store.put(item.clone())?;
                                store.set_active(id.clone(), false, None);
                            }
                        }
                        publish(app, service);
                        // Fall through below to legacy single-POST `save_import_file`.
                    }
                    ResumableStageOutcome::PausedTransient => {
                        {
                            let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
                            if let Some(store) = guard.as_mut() {
                                store.clear_active();
                                store.mark_paused(&id);
                            }
                        }
                        let delay = next_retry_delay(service, &id);
                        schedule_backoff(service, delay);
                        tracing::info!(
                            target: "import",
                            item = %id,
                            backoff_ms = delay.as_millis() as u64,
                            "resumable import paused on transient error; scheduled backoff"
                        );
                        publish(app, service);
                        break;
                    }
                    ResumableStageOutcome::PairingChanged => {
                        {
                            let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
                            if let Some(store) = guard.as_mut() {
                                store.clear_active();
                            }
                        }
                        publish(app, service);
                        break;
                    }
                    ResumableStageOutcome::AuthStop => {
                        {
                            let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
                            if let Some(store) = guard.as_mut() {
                                store.clear_active();
                            }
                        }
                        *service
                            .auth_blocked
                            .lock()
                            .unwrap_or_else(|p| p.into_inner()) = Some(binding.clone());
                        tracing::warn!(target: "import", "journal refused resumable import authentication");
                        publish(app, service);
                        break;
                    }
                    ResumableStageOutcome::InsufficientSpace => {
                        {
                            let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
                            if let Some(store) = guard.as_mut() {
                                store.clear_active();
                                store.clear_paused(&id);
                                item.review = None;
                                item.refusal = Some(Refusal {
                                    journal: journal.clone(),
                                    version: version.clone(),
                                    sentence: message("windows.import.errornospace", &[]),
                                    retryable: true,
                                    reason_code: Some("insufficient_journal_space".into()),
                                    kind: None,
                                });
                                store.put(item)?;
                            }
                        }
                        reset_retry_ladder(service, &id);
                        publish(app, service);
                        continue;
                    }
                    ResumableStageOutcome::IntegrityFailed { reason_code } => {
                        {
                            let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
                            if let Some(store) = guard.as_mut() {
                                store.clear_active();
                                store.clear_paused(&id);
                                item.upload = None;
                                item.sha256 = None;
                                item.review = None;
                                item.refusal = Some(Refusal {
                                    journal: journal.clone(),
                                    version: version.clone(),
                                    sentence: message("windows.import.errorchecksum", &[]),
                                    retryable: true,
                                    reason_code: Some(reason_code.into()),
                                    kind: None,
                                });
                                store.put(item)?;
                            }
                        }
                        reset_retry_ladder(service, &id);
                        publish(app, service);
                        continue;
                    }
                    ResumableStageOutcome::Expired => {
                        {
                            let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
                            if let Some(store) = guard.as_mut() {
                                store.clear_active();
                                store.clear_paused(&id);
                                item.upload = None;
                                item.review = None;
                                item.refusal = Some(Refusal {
                                    journal: journal.clone(),
                                    version: version.clone(),
                                    sentence: message("windows.import.errorexpired", &[]),
                                    retryable: true,
                                    reason_code: Some("upload_not_found".into()),
                                    kind: None,
                                });
                                store.put(item)?;
                            }
                        }
                        reset_retry_ladder(service, &id);
                        publish(app, service);
                        continue;
                    }
                    ResumableStageOutcome::UnreadableLocalCopy => {
                        {
                            let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
                            if let Some(store) = guard.as_mut() {
                                store.clear_active();
                                store.clear_paused(&id);
                                item.upload = None;
                                item.missing_copy = true;
                                item.review = None;
                                item.refusal = Some(Refusal {
                                    journal: journal.clone(),
                                    version: version.clone(),
                                    sentence: message("windows.import.errorunreadable", &[]),
                                    retryable: false,
                                    reason_code: None,
                                    kind: None,
                                });
                                store.put(item)?;
                            }
                        }
                        reset_retry_ladder(service, &id);
                        publish(app, service);
                        continue;
                    }
                    ResumableStageOutcome::Refused {
                        sentence,
                        retryable,
                        reason_code,
                        kind,
                    } => {
                        {
                            let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
                            if let Some(store) = guard.as_mut() {
                                store.clear_active();
                                store.clear_paused(&id);
                                item.upload = None;
                                item.review = None;
                                item.refusal = Some(Refusal {
                                    journal: journal.clone(),
                                    version: version.clone(),
                                    sentence,
                                    retryable,
                                    reason_code,
                                    kind,
                                });
                                store.put(item)?;
                            }
                        }
                        reset_retry_ladder(service, &id);
                        publish(app, service);
                        continue;
                    }
                },
            }
        }

        let step: ActiveStep<
            std::io::Result<Option<Result<ImportReply, pl_transport_win::RouteError>>>,
        > = run_while_awake_or_cancel(
            power_rx,
            cancel_rx,
            &id,
            || {
                platform_win::power_hold::PowerHold::acquire(&message(
                    "windows.import.rowsending",
                    &[],
                ))
            },
            async {
                if item.progress_for(&journal).is_none() {
                    let reply = client
                        .save_import_file(ImportFile {
                            id: &item.id,
                            name: &item.name,
                            content_type: &item.mime,
                            path: &payload,
                            handoff: item.handoff.as_ref(),
                        })
                        .await;
                    match reply {
                        Ok(ImportReply::Saved(saved)) => {
                            if !same_client(app, &client).await {
                                return Ok(None);
                            }
                            item.saved = Some(SavedProgress {
                                journal: journal.clone(),
                                saved,
                            });
                            item.refusal = None;
                            if let Some(store) = service
                                .store
                                .lock()
                                .unwrap_or_else(|p| p.into_inner())
                                .as_mut()
                            {
                                store.put(item.clone())?;
                            }
                        }
                        other => return Ok(Some(other)),
                    }
                }
                if !same_client(app, &client).await {
                    return Ok(None);
                }
                Ok(Some(
                    client
                        .start_import(item.progress_for(&journal).expect("saved progress"))
                        .await,
                ))
            },
        )
        .await;
        let result = match step {
            ActiveStep::Completed(result) => result,
            ActiveStep::Cancelled => {
                let removed_upload = {
                    let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
                    guard
                        .as_mut()
                        .map(|store| store.cancel_upload(&id))
                        .transpose()?
                        .flatten()
                };
                clear_item_cancelled(&service.cancel, &id);
                reset_retry_ladder(service, &id);
                if let Some(u) = removed_upload.filter(|u| u.journal == journal) {
                    spawn_best_effort_abort(app, u.upload_id, journal.clone());
                }
                publish(app, service);
                continue;
            }
            ActiveStep::PausedForSleep => {
                {
                    let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
                    if let Some(store) = guard.as_mut() {
                        store.clear_active();
                    }
                }
                tracing::info!(
                    target: "import",
                    item = %id,
                    "import delivery paused for system sleep; will resume on wake"
                );
                publish(app, service);
                if !power_rx.borrow().suspended {
                    service.wake.notify_one();
                }
                break;
            }
        };
        {
            let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
            let Some(store) = guard.as_mut() else {
                break;
            };
            store.clear_active();
            match result? {
                Some(Ok(ImportReply::Delivered)) => {
                    let row = item
                        .progress_for(&journal)
                        .map(|saved| saved.timestamp.clone());
                    store.delivered_at(&id, &journal, row.as_deref(), now_ms())?;
                    record_handoff_receipt(service, id.clone(), row);
                }
                Some(Ok(ImportReply::ReadyToReview {
                    import_id,
                    reason_code,
                    kind,
                })) => {
                    item.refusal = None;
                    item.review = Some(ReviewState {
                        journal: journal.clone(),
                        import_id,
                        reason_code,
                        kind,
                        expired: false,
                    });
                    store.put(item)?;
                }
                Some(Ok(ImportReply::Refused {
                    sentence,
                    retryable,
                    clear_saved,
                    reason_code,
                    kind,
                })) => {
                    if clear_saved {
                        item.saved = None;
                    }
                    item.review = None;
                    item.refusal = Some(Refusal {
                        journal: journal.clone(),
                        version: version.clone(),
                        sentence,
                        retryable,
                        reason_code,
                        kind,
                    });
                    store.put(item)?;
                }
                Some(Ok(ImportReply::AuthStop)) => {
                    *service
                        .auth_blocked
                        .lock()
                        .unwrap_or_else(|p| p.into_inner()) = Some(binding.clone());
                    tracing::warn!(target: "import", "journal refused import authentication");
                    break;
                }
                Some(Err(pl_transport_win::RouteError::Transport(
                    pl_transport_win::TransportError::Io(error),
                ))) if error.kind() == std::io::ErrorKind::NotFound => {
                    item.missing_copy = true;
                    item.review = None;
                    item.refusal = Some(Refusal {
                        journal: journal.clone(),
                        version: version.clone(),
                        sentence: message("windows.ui.thecopyonthisdeviceismissing", &[]),
                        retryable: false,
                        reason_code: None,
                        kind: None,
                    });
                    store.put(item)?;
                }
                Some(Err(error)) => {
                    tracing::warn!(target: "import", %error, item = %id, "import request will retry");
                }
                Some(Ok(ImportReply::Retry | ImportReply::Saved(_))) => {
                    tracing::warn!(target: "import", item = %id, "import answer did not prove delivery; will retry");
                }
                None => break,
            }
        }
        publish(app, service);
    }
    Ok(())
}

fn has_pending_delivery_items(service: &Service, journal: &str) -> bool {
    let store = service.store.lock().unwrap_or_else(|p| p.into_inner());
    let Some(store) = store.as_ref() else {
        return false;
    };
    store.items.values().any(|item| {
        !item.missing_copy
            && !item.refused_here(journal, "")
            && item.review_for(journal).is_none()
            && store.confirming_remove.as_deref() != Some(&item.id)
    })
}

async fn same_client(app: &tauri::AppHandle, asked: &Arc<ObserverClient>) -> bool {
    client(app)
        .await
        .is_some_and(|current| Arc::ptr_eq(&current, asked) && current.gate_open())
}

pub fn add_paths(app: tauri::AppHandle, paths: Vec<PathBuf>) {
    let Some(service) = SERVICE.get().cloned() else {
        return;
    };
    tauri::async_runtime::spawn(async move {
        let _ = intake(&app, &service, paths, None).await;
    });
}

/// Files the journal page handed over: held with the page's choices, and only
/// answered once each copy is complete. `None` means custody is unavailable.
pub async fn add_handoff(
    app: &tauri::AppHandle,
    paths: Vec<PathBuf>,
    handoff: Handoff,
) -> Option<(Vec<(String, String)>, Vec<IntakeRefusal>)> {
    let service = SERVICE.get().cloned()?;
    intake(app, &service, paths, Some(handoff)).await
}

/// Copy each file into custody at the owner's gesture, one intake at a time.
async fn intake(
    app: &tauri::AppHandle,
    service: &Arc<Service>,
    paths: Vec<PathBuf>,
    handoff: Option<Handoff>,
) -> Option<(Vec<(String, String)>, Vec<IntakeRefusal>)> {
    let _intake = service.intake_lock.lock().await;
    let root = {
        let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
        let store = guard.as_mut()?;
        store.intake = Some(Intake {
            copying: true,
            total: paths.len(),
            ..Default::default()
        });
        store.root().to_path_buf()
    };
    publish(app, service);
    let mut accepted = Vec::new();
    let mut declined = Vec::new();
    for path in paths {
        let root = root.clone();
        let handoff = handoff.clone();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| message("windows.ui.file", &[]));
        let result = tauri::async_runtime::spawn_blocking(move || {
            import_store::new_id()
                .map_err(|_| IntakeRefusal {
                    name: message("windows.ui.file", &[]),
                    reason: message("windows.ui.thiscouldntbereadadditagain", &[]),
                })
                .and_then(|id| {
                    import_store::stage_with_converter(&root, &path, id, now(), handoff, |bytes| {
                        platform_win::screenshots::to_jpeg(bytes).map_err(|_| ())
                    })
                })
        })
        .await;
        let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
        let store = guard.as_mut()?;
        let mut intake = store.intake.take().unwrap_or_default();
        let mut counted_item: Option<(String, i64)> = None;
        match result {
            Ok(Ok(item)) => {
                counted_item = Some((item.id.clone(), item.added as i64));
                accepted.push((item.id.clone(), item.name.clone()));
                store.items.insert(item.id.clone(), item);
                intake.accepted += 1;
            }
            Ok(Err(refusal)) => {
                declined.push(refusal.clone());
                intake.refused.push(refusal);
            }
            Err(error) => {
                tracing::warn!(target: "import", %error, "import intake interrupted");
                store.interrupted = true;
                declined.push(IntakeRefusal {
                    name,
                    reason: message("windows.ui.thiscouldntbereadadditagain", &[]),
                });
            }
        }
        intake.done += 1;
        store.intake = Some(intake);
        drop(guard);
        if let (Some(counters), Some((id, added))) = (&service.counters, counted_item) {
            counters.add_imports(&[id], added);
        }
        publish(app, service);
    }
    if let Some(store) = service
        .store
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_mut()
    {
        if let Some(intake) = &mut store.intake {
            intake.copying = false;
        }
    }
    publish(app, service);
    mark_resumed(&service.power);
    service.wake.notify_one();
    Some((accepted, declined))
}

/// Where one handed-over item stands, in the page's words: waiting, uploading,
/// attention (with review/expiry/reason metadata), delivered (with its row) or gone.
pub fn handoff_status(app: &tauri::AppHandle, id: &str) -> serde_json::Value {
    let Some(service) = SERVICE.get() else {
        return serde_json::json!({"state": "gone"});
    };
    let delivered_row = service
        .receipts
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .find(|(item, _)| item == id)
        .map(|(_, row)| row.clone());
    if let Some(row) = delivered_row {
        return import_store::handoff_status_json(Some(row.as_deref()), None, "", "", id);
    }
    let sync = current_sync(app);
    let (journal, _) = current_identity(service, &sync);
    let version = sync.journal_version.as_deref().unwrap_or("");
    let guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
    import_store::handoff_status_json(None, guard.as_ref(), &journal, version, id)
}

pub fn remove(app: &tauri::AppHandle, id: &str) -> std::io::Result<()> {
    if let Some(service) = SERVICE.get() {
        let sync = current_sync(app);
        let (journal, _) = current_identity(service, &sync);
        let removed_upload = {
            let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
            guard
                .as_mut()
                .map(|store| store.remove_returning_upload(id))
                .transpose()?
                .flatten()
                .flatten()
        };
        reset_retry_ladder(service, id);
        if let Some(u) = removed_upload.filter(|u| !journal.is_empty() && u.journal == journal) {
            spawn_best_effort_abort(app, u.upload_id, journal);
        }
        publish(app, service);
    }
    Ok(())
}

/// Cancel an active or paused resumable transfer immediately, clearing its local
/// custody copy and sending a best-effort remote abort to reclaim journal space.
pub fn cancel_upload(app: &tauri::AppHandle, id: &str) -> std::io::Result<()> {
    if let Some(service) = SERVICE.get() {
        let sync = current_sync(app);
        let (journal, _) = current_identity(service, &sync);
        mark_item_cancelled(&service.cancel, id);
        let (was_active, removed_upload) = {
            let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(store) = guard.as_mut() {
                if store.active.as_deref() == Some(id) {
                    (true, None)
                } else {
                    (false, store.cancel_upload(id)?)
                }
            } else {
                (false, None)
            }
        };
        if !was_active {
            clear_item_cancelled(&service.cancel, id);
            reset_retry_ladder(service, id);
            if let Some(u) = removed_upload.filter(|u| !journal.is_empty() && u.journal == journal)
            {
                spawn_best_effort_abort(app, u.upload_id, journal);
            }
            publish(app, service);
        } else {
            service.wake.notify_one();
        }
    }
    Ok(())
}

pub fn retry(app: &tauri::AppHandle, id: &str) -> std::io::Result<()> {
    if let Some(service) = SERVICE.get() {
        if let Some(store) = service
            .store
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_mut()
        {
            store.retry(id)?;
        }
        reset_retry_ladder(service, id);
        publish(app, service);
        mark_resumed(&service.power);
        service.wake.notify_one();
    }
    Ok(())
}

pub fn send_again(app: &tauri::AppHandle, id: &str) -> std::io::Result<()> {
    if let Some(service) = SERVICE.get() {
        if let Some(store) = service
            .store
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_mut()
        {
            store.send_again(id)?;
        }
        publish(app, service);
        mark_resumed(&service.power);
        service.wake.notify_one();
    }
    Ok(())
}

pub fn dismiss_interrupted(app: &tauri::AppHandle) -> std::io::Result<()> {
    if let Some(service) = SERVICE.get() {
        if let Some(store) = service
            .store
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_mut()
        {
            store.dismiss_interrupted()?;
        }
        publish(app, service);
    }
    Ok(())
}

#[derive(Serialize)]
pub struct RemovePrompt {
    name: String,
    may_have_reached: bool,
    missing_copy: bool,
    ready_to_review: bool,
}

pub fn prepare_remove(id: &str) -> Option<RemovePrompt> {
    let service = SERVICE.get()?;
    let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
    let item = guard.as_mut()?.prepare_remove(id)?;
    let ready_to_review = item.review.as_ref().is_some_and(|r| !r.expired);
    Some(RemovePrompt {
        name: item.name,
        may_have_reached: item.may_have_reached,
        missing_copy: item.missing_copy
            || guard.as_ref()?.payload(id).try_exists().ok() == Some(false),
        ready_to_review,
    })
}

pub fn cancel_remove(id: Option<&str>) {
    if let Some(service) = SERVICE.get() {
        if let Some(store) = service
            .store
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_mut()
        {
            store.cancel_remove(id);
        }
        service.wake.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiting_counts_only_imports_that_can_continue_without_the_owner() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(root.path().join("import")).unwrap();
        let source = root.path().join("note.txt");
        std::fs::write(&source, b"held").unwrap();
        let mut item =
            import_store::stage(store.root(), &source, import_store::new_id().unwrap(), 1).unwrap();
        store.put(item.clone()).unwrap();
        let mut sync = SyncSnapshot::default();
        sync.pairing.phase = PairingPhase::Paired;
        sync.journal_version = Some("2.0.52".into());
        sync.journal_version_fresh = true;
        assert_eq!(document_for(Some(&store), &sync, "one", false).waiting, 1);
        sync.journal_version = Some("2.0.51".into());
        let old = document_for(Some(&store), &sync, "one", false);
        assert_eq!(old.waiting, 0);
        assert!(old.needs_owner);
        sync.journal_version = Some("2.0.52".into());
        item.refusal = Some(Refusal {
            journal: "one".into(),
            version: "2.0.52".into(),
            sentence: "refused".into(),
            retryable: true,
            reason_code: None,
            kind: None,
        });
        store.put(item).unwrap();
        let refused = document_for(Some(&store), &sync, "one", false);
        assert_eq!(refused.waiting, 0);
        assert!(refused.needs_owner);
        assert_eq!(refused.rows[0].state, "refused");
        let repaired = document_for(Some(&store), &sync, "two", false);
        assert_eq!(repaired.waiting, 1);
        assert!(!repaired.needs_owner);
        assert_eq!(repaired.rows[0].state, "waiting");
        let blocked = document_for(Some(&store), &sync, "two", true);
        assert_eq!(blocked.waiting, 0);
        assert!(blocked.needs_owner);
    }

    #[test]
    fn copying_material_prevents_a_false_caught_up_status() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(root.path().join("import")).unwrap();
        store.intake = Some(Intake {
            copying: true,
            done: 1,
            total: 2,
            ..Default::default()
        });
        let mut sync = SyncSnapshot::default();
        sync.pairing.phase = PairingPhase::Paired;
        sync.journal_version = Some("2.0.52".into());
        sync.journal_version_fresh = true;
        let copying = document_for(Some(&store), &sync, "one", false);
        assert_eq!(copying.waiting, 1);
        assert!(copying.rows.is_empty());
        sync.journal_version = Some("2.0.51".into());
        let old = document_for(Some(&store), &sync, "one", false);
        assert_eq!(old.waiting, 0);
        assert!(old.needs_owner);
        store.intake.as_mut().unwrap().done = 2;
        let complete = document_for(Some(&store), &sync, "one", false);
        assert_eq!(complete.waiting, 0);
        assert!(!complete.needs_owner);
    }

    #[test]
    fn unavailable_store_is_not_an_interrupted_addition() {
        let document = document_for(None, &SyncSnapshot::default(), "", false);
        assert!(!document.available);
        assert!(document.needs_owner);
        assert!(!document.interrupted);
        assert!(document.rows.is_empty());
    }

    #[test]
    fn pending_delivery_probe_ignores_empty_review_refused_missing_and_confirming_remove_items() {
        let root = tempfile::tempdir().unwrap();
        let (power, _) = watch::channel(PowerState::default());
        let (cancel, _) = watch::channel(BTreeSet::new());
        let (changes, _) = watch::channel(());
        let mut service = Service {
            store: Mutex::new(None),
            intake_lock: tokio::sync::Mutex::new(()),
            wake: Notify::new(),
            power,
            cancel,
            changes,
            identity: Mutex::new(None),
            auth_blocked: Mutex::new(None),
            resumable_probe: Mutex::new(None),
            retry_ladders: Mutex::new(BTreeMap::new()),
            next_backoff: Mutex::new(None),
            receipts: Mutex::new(VecDeque::new()),
            counters: None,
            reconcile_requested: AtomicBool::new(false),
        };
        assert!(!has_pending_delivery_items(&service, "one"));

        let store = Store::open(root.path().join("import")).unwrap();
        service.store = Mutex::new(Some(store));
        assert!(!has_pending_delivery_items(&service, "one"));

        let source = root.path().join("note.txt");
        std::fs::write(&source, b"held").unwrap();
        let mut item = import_store::stage(
            root.path().join("import").as_path(),
            &source,
            import_store::new_id().unwrap(),
            1,
        )
        .unwrap();
        let id = item.id.clone();

        // Held in review on this journal: silent; foreign journal review still deliverable.
        item.review = Some(import_store::ReviewState {
            journal: "one".into(),
            import_id: "20261122_120000".into(),
            reason_code: "import_preview_required".into(),
            kind: "review_required".into(),
            expired: false,
        });
        service
            .store
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .put(item.clone())
            .unwrap();
        assert!(!has_pending_delivery_items(&service, "one"));
        assert!(has_pending_delivery_items(&service, "two"));

        // Permanent preview-required refusal on this journal: silent.
        item.review = None;
        item.refusal = Some(Refusal {
            journal: "one".into(),
            version: "2.0.88".into(),
            sentence: "review required".into(),
            retryable: false,
            reason_code: Some("import_preview_required".into()),
            kind: Some("review_required".into()),
        });
        service
            .store
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .put(item.clone())
            .unwrap();
        assert!(!has_pending_delivery_items(&service, "one"));

        // Missing local copy: silent.
        item.refusal = None;
        item.missing_copy = true;
        service
            .store
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .put(item.clone())
            .unwrap();
        assert!(!has_pending_delivery_items(&service, "one"));

        // Ordinary waiting item: deliverable unless confirming removal.
        item.missing_copy = false;
        service
            .store
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .put(item)
            .unwrap();
        assert!(has_pending_delivery_items(&service, "one"));

        service
            .store
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .prepare_remove(&id)
            .unwrap();
        assert!(!has_pending_delivery_items(&service, "one"));
    }

    struct ProbeHold {
        active: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl ProbeHold {
        fn acquire(active: &Arc<std::sync::atomic::AtomicUsize>) -> Self {
            active.fetch_add(1, Ordering::AcqRel);
            Self {
                active: active.clone(),
            }
        }
    }

    impl Drop for ProbeHold {
        fn drop(&mut self) {
            self.active.fetch_sub(1, Ordering::AcqRel);
        }
    }

    #[tokio::test]
    async fn active_delivery_acquires_power_hold_and_releases_on_normal_and_error_exits() {
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (power_tx, mut power_rx) = watch::channel(PowerState::default());

        let active_in_ok = active.clone();
        let ok_step = run_while_awake(&mut power_rx, || ProbeHold::acquire(&active), async move {
            assert_eq!(active_in_ok.load(Ordering::Acquire), 1);
            Ok::<&str, &str>("delivered")
        })
        .await;
        assert!(matches!(ok_step, DeliveryStep::Completed(Ok("delivered"))));
        assert_eq!(active.load(Ordering::Acquire), 0);

        let active_in_err = active.clone();
        let err_step = run_while_awake(&mut power_rx, || ProbeHold::acquire(&active), async move {
            assert_eq!(active_in_err.load(Ordering::Acquire), 1);
            Err::<&str, &str>("transient")
        })
        .await;
        assert!(matches!(
            err_step,
            DeliveryStep::Completed(Err("transient"))
        ));
        assert_eq!(active.load(Ordering::Acquire), 0);

        drop(power_tx);
    }

    #[tokio::test]
    async fn system_suspend_pauses_active_delivery_releases_hold_and_resumes_on_wake() {
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (power_tx, mut power_rx) = watch::channel(PowerState::default());
        let wake = Arc::new(Notify::new());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();

        struct DropSignal(Option<tokio::sync::oneshot::Sender<()>>);
        impl Drop for DropSignal {
            fn drop(&mut self) {
                if let Some(tx) = self.0.take() {
                    let _ = tx.send(());
                }
            }
        }

        let active_for_task = active.clone();
        let task = tokio::spawn(async move {
            let step = run_while_awake(
                &mut power_rx,
                || ProbeHold::acquire(&active_for_task),
                async move {
                    let _drop_guard = DropSignal(Some(dropped_tx));
                    let _ = started_tx.send(());
                    std::future::pending::<()>().await;
                },
            )
            .await;
            (step, power_rx)
        });

        started_rx.await.unwrap();
        assert_eq!(active.load(Ordering::Acquire), 1);

        mark_suspending(&power_tx);
        let (step, mut power_rx) = task.await.unwrap();
        assert!(matches!(step, DeliveryStep::PausedForSleep));
        dropped_rx.await.unwrap();
        assert_eq!(active.load(Ordering::Acquire), 0);

        // Starting another attempt while still suspended never acquires a hold.
        let skipped = run_while_awake(&mut power_rx, || ProbeHold::acquire(&active), async {
            panic!("work must not run while suspended");
        })
        .await;
        assert!(matches!(skipped, DeliveryStep::PausedForSleep));
        assert_eq!(active.load(Ordering::Acquire), 0);

        // Even with a pending wake permit, the wait stays parked until resume.
        wake.notify_one();
        let wake_for_wait = wake.clone();
        let waiter = tokio::spawn(async move {
            wait_for_next_pass(&mut power_rx, &wake_for_wait, Duration::from_secs(30)).await;
            power_rx
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished());

        mark_resumed(&power_tx);
        let mut power_rx = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("waiter should wake immediately on resume")
            .unwrap();

        // After wake, a new delivery acquires the hold and completes cleanly.
        let active_after = active.clone();
        let resumed = run_while_awake(&mut power_rx, || ProbeHold::acquire(&active), async move {
            assert_eq!(active_after.load(Ordering::Acquire), 1);
            42u32
        })
        .await;
        assert!(matches!(resumed, DeliveryStep::Completed(42)));
        assert_eq!(active.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn back_to_back_suspend_and_resume_still_cancels_stale_in_flight_delivery() {
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (power_tx, mut power_rx) = watch::channel(PowerState::default());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();

        let active_for_task = active.clone();
        let task = tokio::spawn(async move {
            run_while_awake(
                &mut power_rx,
                || ProbeHold::acquire(&active_for_task),
                async move {
                    let _ = started_tx.send(());
                    std::future::pending::<()>().await;
                },
            )
            .await
        });

        started_rx.await.unwrap();
        assert_eq!(active.load(Ordering::Acquire), 1);

        // Simulates Suspending + ResumedAutomatic drained in the same pump batch.
        mark_suspending(&power_tx);
        mark_resumed(&power_tx);

        let step = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("stale in-flight upload must cancel across a suspend epoch")
            .unwrap();
        assert!(matches!(step, DeliveryStep::PausedForSleep));
        assert_eq!(active.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn resumed_while_already_awake_does_not_abort_fresh_delivery() {
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (power_tx, mut power_rx) = watch::channel(PowerState::default());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();

        let active_for_task = active.clone();
        let task = tokio::spawn(async move {
            run_while_awake(
                &mut power_rx,
                || ProbeHold::acquire(&active_for_task),
                async move {
                    let _ = started_tx.send(());
                    finish_rx.await.unwrap();
                    "finished"
                },
            )
            .await
        });

        started_rx.await.unwrap();
        assert_eq!(active.load(Ordering::Acquire), 1);

        // Simulates PBT_APMRESUMESUSPEND or SessionUnlocked arriving after a
        // fresh post-wake delivery already began on PBT_APMRESUMEAUTOMATIC.
        mark_resumed(&power_tx);
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        assert_eq!(active.load(Ordering::Acquire), 1);

        finish_tx.send(()).unwrap();
        let step = task.await.unwrap();
        assert!(matches!(step, DeliveryStep::Completed("finished")));
        assert_eq!(active.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn owner_cancel_aborts_active_resumable_upload_and_releases_power_hold() {
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (power_tx, mut power_rx) = watch::channel(PowerState::default());
        let (cancel_tx, mut cancel_rx) = watch::channel(BTreeSet::new());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();

        let active_for_task = active.clone();
        let task = tokio::spawn(async move {
            run_while_awake_or_cancel(
                &mut power_rx,
                &mut cancel_rx,
                "item-target",
                || ProbeHold::acquire(&active_for_task),
                async move {
                    let _ = started_tx.send(());
                    std::future::pending::<()>().await;
                },
            )
            .await
        });

        started_rx.await.unwrap();
        assert_eq!(active.load(Ordering::Acquire), 1);

        // Cancelling a different item leaves the active upload running.
        mark_item_cancelled(&cancel_tx, "item-other");
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        assert_eq!(active.load(Ordering::Acquire), 1);

        // Cancelling the active item aborts immediately and drops the OS power hold.
        mark_item_cancelled(&cancel_tx, "item-target");
        let step = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("active upload must abort promptly on cancel")
            .unwrap();
        assert!(matches!(step, ActiveStep::Cancelled));
        assert_eq!(active.load(Ordering::Acquire), 0);
        drop(power_tx);
    }

    #[tokio::test]
    async fn suspend_during_chunked_upload_releases_hold_and_preserves_resumability() {
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (power_tx, mut power_rx) = watch::channel(PowerState::default());
        let (_cancel_tx, mut cancel_rx) = watch::channel(BTreeSet::new());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();

        let active_for_task = active.clone();
        let task = tokio::spawn(async move {
            run_while_awake_or_cancel(
                &mut power_rx,
                &mut cancel_rx,
                "item-sleep",
                || ProbeHold::acquire(&active_for_task),
                async move {
                    let _ = started_tx.send(());
                    std::future::pending::<()>().await;
                },
            )
            .await
        });

        started_rx.await.unwrap();
        assert_eq!(active.load(Ordering::Acquire), 1);

        mark_suspending(&power_tx);
        let step = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("chunked upload must pause promptly on suspend")
            .unwrap();
        assert!(matches!(step, ActiveStep::PausedForSleep));
        assert_eq!(active.load(Ordering::Acquire), 0);
    }
}
