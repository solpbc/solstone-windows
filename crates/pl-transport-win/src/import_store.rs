// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Durable local custody for files chosen for import.

use observer_model::i18n::message;
use observer_model::{is_valid_import_id, JournalLink, PairingPhase, SyncSnapshot};

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::import_client::{
    Handoff, ImportFile, ReconcileReply, SavedImport, MAX_IMPORT_FILE_BYTES, REQUEST_BODY_LIMIT,
};

pub const RECENT_WINDOW_MS: u64 = 24 * 60 * 60 * 1000;
pub const RECENT_MAX_ITEMS: usize = 5;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedProgress {
    pub journal: String,
    pub saved: SavedImport,
}

/// Durable journal-scoped resumable upload cursor.
///
/// Bound to `journal` (`upload_instance_id`): if the owner pairs with a
/// different journal mid-upload, this state is discarded while keeping the
/// staged local file and its precomputed `sha256`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResumableUploadState {
    pub journal: String,
    pub upload_id: String,
    pub next_offset: u64,
    pub total_bytes: u64,
    pub chunk_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UploadPhase {
    Sending,
    Paused,
    Checking,
}

impl UploadPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sending => "sending",
            Self::Paused => "paused",
            Self::Checking => "checking",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Refusal {
    pub journal: String,
    pub version: String,
    pub sentence: String,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// Durable confirmation-required review state for an import saved in a specific journal.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewState {
    pub journal: String,
    pub import_id: String,
    pub reason_code: String,
    pub kind: String,
    #[serde(default)]
    pub expired: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecentReceipt {
    pub filename: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_id: Option<String>,
    pub instance_id: String,
    pub delivered_at_epoch_ms: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
struct RecentFileV2 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delivered_to: Option<String>,
    #[serde(default)]
    receipts: Vec<RecentReceipt>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RecentFileDisk {
    LegacyJournal(String),
    V2(RecentFileV2),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub added: u64,
    pub saved: Option<SavedProgress>,
    pub refusal: Option<Refusal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewState>,
    #[serde(default)]
    pub may_have_reached: bool,
    #[serde(default)]
    pub missing_copy: bool,
    /// Present when the journal page handed this file to the app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff: Option<Handoff>,
    /// Whole-file SHA-256 computed once from the stored custody copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Resumable chunked upload cursor scoped to one paired journal instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload: Option<ResumableUploadState>,
}

impl Item {
    pub fn refused_here(&self, journal: &str, version: &str) -> bool {
        self.refusal.as_ref().is_some_and(|r| {
            r.journal == journal
                && (r.version == version
                    || r.reason_code.as_deref() == Some("import_preview_required"))
        })
    }

    pub fn progress_for(&self, journal: &str) -> Option<&SavedImport> {
        self.saved
            .as_ref()
            .filter(|p| p.journal == journal)
            .map(|p| &p.saved)
    }

    pub fn review_for(&self, journal: &str) -> Option<&ReviewState> {
        self.review
            .as_ref()
            .filter(|r| !journal.is_empty() && r.journal == journal)
    }

    pub fn upload_for(&self, journal: &str) -> Option<&ResumableUploadState> {
        self.upload
            .as_ref()
            .filter(|u| !journal.is_empty() && u.journal == journal)
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct IntakeRefusal {
    pub name: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct Intake {
    pub copying: bool,
    pub done: usize,
    pub total: usize,
    pub accepted: usize,
    pub refused: Vec<IntakeRefusal>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconcileOutcome {
    Confirmed,
    Expired,
    Unchanged,
    Stale,
}

pub struct Store {
    root: PathBuf,
    pub items: BTreeMap<String, Item>,
    pub active: Option<String>,
    pub active_phase: Option<UploadPhase>,
    pub active_resumable: bool,
    pub paused_uploads: BTreeSet<String>,
    pub confirming_remove: Option<String>,
    pub intake: Option<Intake>,
    pub interrupted: bool,
    pub delivered_to: Option<String>,
    pub recent: Vec<RecentReceipt>,
}

fn valid_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

pub fn new_id() -> io::Result<String> {
    let key = rcgen::KeyPair::generate().map_err(io::Error::other)?;
    Ok(spl_core::ca::sha256_hex(&key.public_key_der()))
}

fn atomic_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    let mut file = File::create(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temporary, path)
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn prune_recent_receipts(receipts: &mut Vec<RecentReceipt>, now_ms: u64) {
    receipts.retain(|r| {
        !r.instance_id.is_empty()
            && now_ms.saturating_sub(r.delivered_at_epoch_ms) <= RECENT_WINDOW_MS
    });
    for receipt in receipts.iter_mut() {
        if receipt
            .import_id
            .as_deref()
            .is_some_and(|id| !is_valid_import_id(id))
        {
            receipt.import_id = None;
        }
    }
    receipts.sort_by(|a, b| {
        b.delivered_at_epoch_ms
            .cmp(&a.delivered_at_epoch_ms)
            .then_with(|| a.filename.cmp(&b.filename))
    });
    let mut seen_ids = BTreeSet::new();
    let mut per_instance: BTreeMap<String, usize> = BTreeMap::new();
    receipts.retain(|r| {
        if let Some(id) = &r.import_id {
            if !seen_ids.insert((r.instance_id.clone(), id.clone())) {
                return false;
            }
        }
        let count = per_instance.entry(r.instance_id.clone()).or_insert(0);
        if *count >= RECENT_MAX_ITEMS {
            return false;
        }
        *count += 1;
        true
    });
}

impl Store {
    pub fn open(root: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&root)?;
        let mut items = BTreeMap::new();
        let mut interrupted = root.join("interrupted.json").exists();
        let mut partials = Vec::new();
        for entry in fs::read_dir(&root)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(".staging-") && valid_id(&name[9..]) {
                interrupted = true;
                partials.push(entry.path());
                continue;
            }
            if !valid_id(&name) || !entry.file_type()?.is_dir() {
                continue;
            }
            let path = entry.path().join("item.json");
            let bytes = fs::read(path)?;
            let item: Item = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            if item.id != name {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "import item identity differs",
                ));
            }
            items.insert(name, item);
        }
        let (delivered_to, mut recent) = match fs::read(root.join("recent.json")) {
            Ok(bytes) => {
                match serde_json::from_slice::<RecentFileDisk>(&bytes).map_err(io::Error::other)? {
                    RecentFileDisk::LegacyJournal(journal) => (Some(journal), Vec::new()),
                    RecentFileDisk::V2(file) => (file.delivered_to, file.receipts),
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => (None, Vec::new()),
            Err(error) => return Err(error),
        };
        prune_recent_receipts(&mut recent, now_epoch_ms());
        // Validate every published record before removing an unpublished copy.
        // An open failure leaves all material in place. Cleanup trouble is an
        // interrupted intake, with its durable notice, rather than an open failure.
        if !partials.is_empty() {
            atomic_json(&root.join("interrupted.json"), &true)?;
            for partial in partials {
                if let Err(error) = fs::remove_dir_all(partial) {
                    tracing::warn!(target: "import", %error, "partial import copy retained");
                }
            }
        }
        Ok(Self {
            root,
            items,
            active: None,
            active_phase: None,
            active_resumable: false,
            paused_uploads: BTreeSet::new(),
            confirming_remove: None,
            intake: None,
            interrupted,
            delivered_to,
            recent,
        })
    }

    pub fn set_active(&mut self, id: String, resumable: bool, phase: Option<UploadPhase>) {
        self.paused_uploads.remove(&id);
        self.active = Some(id);
        self.active_resumable = resumable;
        self.active_phase = phase;
    }

    pub fn clear_active(&mut self) {
        self.active = None;
        self.active_resumable = false;
        self.active_phase = None;
    }

    pub fn mark_paused(&mut self, id: &str) {
        if self.items.contains_key(id) {
            self.paused_uploads.insert(id.to_owned());
        }
    }

    pub fn clear_paused(&mut self, id: &str) {
        self.paused_uploads.remove(id);
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn payload(&self, id: &str) -> PathBuf {
        self.root.join(id).join("payload")
    }

    pub fn put(&mut self, item: Item) -> io::Result<()> {
        if !valid_id(&item.id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid import item",
            ));
        }
        atomic_json(&self.root.join(&item.id).join("item.json"), &item)?;
        self.items.insert(item.id.clone(), item);
        Ok(())
    }

    pub fn prepare_remove(&mut self, id: &str) -> Option<Item> {
        if self.active.as_deref() == Some(id) {
            return None;
        }
        let item = self.items.get(id)?.clone();
        self.confirming_remove = Some(id.to_string());
        Some(item)
    }

    pub fn cancel_remove(&mut self, id: Option<&str>) {
        if id.is_none() || self.confirming_remove.as_deref() == id {
            self.confirming_remove = None;
        }
    }

    pub fn remove(&mut self, id: &str) -> io::Result<bool> {
        Ok(self.remove_returning_upload(id)?.is_some())
    }

    pub fn remove_returning_upload(
        &mut self,
        id: &str,
    ) -> io::Result<Option<Option<ResumableUploadState>>> {
        if self.active.as_deref() == Some(id)
            || self.confirming_remove.as_deref() != Some(id)
            || !self.items.contains_key(id)
        {
            return Ok(None);
        }
        match fs::remove_dir_all(self.root.join(id)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let removed = self.items.remove(id);
        self.paused_uploads.remove(id);
        self.confirming_remove = None;
        Ok(Some(removed.and_then(|item| item.upload)))
    }

    /// Cancel an active or paused resumable upload immediately, removing its
    /// local custody copy and returning any journal-scoped upload cursor so the
    /// caller can send best-effort `DELETE /app/import/api/uploads/{upload_id}`.
    pub fn cancel_upload(&mut self, id: &str) -> io::Result<Option<ResumableUploadState>> {
        if !self.items.contains_key(id) {
            return Ok(None);
        }
        if self.active.as_deref() == Some(id) {
            self.clear_active();
        }
        match fs::remove_dir_all(self.root.join(id)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let removed = self.items.remove(id);
        self.paused_uploads.remove(id);
        if self.confirming_remove.as_deref() == Some(id) {
            self.confirming_remove = None;
        }
        Ok(removed.and_then(|item| item.upload))
    }

    /// Called only after the journal proved terminal custody. A failed cleanup
    /// keeps the same id and progress so replay can finish it on a later pass.
    pub fn delivered(&mut self, id: &str, journal: &str) -> io::Result<()> {
        let import_id = self
            .items
            .get(id)
            .and_then(|item| item.progress_for(journal))
            .map(|saved| saved.timestamp.clone());
        self.delivered_at(id, journal, import_id.as_deref(), now_epoch_ms())
    }

    /// Persist a 24-hour UI recent receipt durably FIRST, then delete local bytes.
    pub fn delivered_at(
        &mut self,
        id: &str,
        journal: &str,
        import_id: Option<&str>,
        now_ms: u64,
    ) -> io::Result<()> {
        let Some(item) = self.items.get(id) else {
            return Ok(());
        };
        let mut next_recent = self.recent.clone();
        next_recent.push(RecentReceipt {
            filename: item.name.clone(),
            import_id: import_id
                .filter(|candidate| is_valid_import_id(candidate))
                .map(str::to_owned),
            instance_id: journal.to_owned(),
            delivered_at_epoch_ms: now_ms,
        });
        prune_recent_receipts(&mut next_recent, now_ms);
        let disk = RecentFileV2 {
            delivered_to: Some(journal.to_owned()),
            receipts: next_recent.clone(),
        };
        atomic_json(&self.root.join("recent.json"), &disk)?;
        self.delivered_to = Some(journal.to_owned());
        self.recent = next_recent;
        match fs::remove_dir_all(self.root.join(id)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        self.items.remove(id);
        self.paused_uploads.remove(id);
        if self.confirming_remove.as_deref() == Some(id) {
            self.confirming_remove = None;
        }
        Ok(())
    }

    /// Pruned 24h recent receipts scoped strictly to `journal` (max 5, newest first).
    pub fn recent_for(&self, journal: &str, now_ms: u64) -> Vec<RecentReceipt> {
        if journal.is_empty() {
            return Vec::new();
        }
        let mut filtered: Vec<RecentReceipt> = self
            .recent
            .iter()
            .filter(|r| r.instance_id == journal)
            .cloned()
            .collect();
        prune_recent_receipts(&mut filtered, now_ms);
        filtered.truncate(RECENT_MAX_ITEMS);
        filtered
    }

    /// On re-pair to a different journal instance, discard remote upload cursors
    /// bound to the prior journal while keeping the staged local file and sha256.
    pub fn reset_foreign_uploads(&mut self, current_journal: &str) -> io::Result<bool> {
        if current_journal.is_empty() {
            return Ok(false);
        }
        let foreign_ids: Vec<String> = self
            .items
            .values()
            .filter(|item| {
                self.active.as_deref() != Some(&item.id)
                    && item
                        .upload
                        .as_ref()
                        .is_some_and(|u| u.journal != current_journal)
            })
            .map(|item| item.id.clone())
            .collect();
        let mut changed = false;
        for id in foreign_ids {
            if let Some(mut item) = self.items.get(&id).cloned() {
                item.upload = None;
                self.paused_uploads.remove(&id);
                self.put(item)?;
                changed = true;
            }
        }
        Ok(changed)
    }

    /// On re-pair to a different journal instance, reset `ready to review` and
    /// `review expired` items back to pending/waiting (clearing saved fields).
    pub fn reset_foreign_reviews(&mut self, current_journal: &str) -> io::Result<bool> {
        if current_journal.is_empty() {
            return Ok(false);
        }
        let foreign_ids: Vec<String> = self
            .items
            .values()
            .filter(|item| {
                self.active.as_deref() != Some(&item.id)
                    && item
                        .review
                        .as_ref()
                        .is_some_and(|r| r.journal != current_journal)
            })
            .map(|item| item.id.clone())
            .collect();
        let mut changed = false;
        for id in foreign_ids {
            if let Some(mut item) = self.items.get(&id).cloned() {
                item.review = None;
                item.saved = None;
                item.refusal = None;
                item.may_have_reached = false;
                self.put(item)?;
                changed = true;
            }
        }
        Ok(changed)
    }

    /// Non-expired `ready to review` items scoped strictly to `current_journal`.
    pub fn review_candidates(&self, current_journal: &str) -> Vec<(String, String)> {
        if current_journal.is_empty() {
            return Vec::new();
        }
        let mut rows: Vec<(&Item, &str)> = self
            .items
            .values()
            .filter(|item| {
                self.active.as_deref() != Some(&item.id)
                    && self.confirming_remove.as_deref() != Some(&item.id)
            })
            .filter_map(|item| {
                let review = item.review_for(current_journal)?;
                if review.expired || !is_valid_import_id(&review.import_id) {
                    return None;
                }
                Some((item, review.import_id.as_str()))
            })
            .collect();
        rows.sort_by_key(|(item, _)| item.added);
        rows.into_iter()
            .map(|(item, import_id)| (item.id.clone(), import_id.to_owned()))
            .collect()
    }

    /// Apply one reconciliation response after re-verifying `(item_id, saved_import_id, saved_instance_id)`.
    pub fn apply_reconcile(
        &mut self,
        id: &str,
        expected_import_id: &str,
        expected_journal: &str,
        reply: ReconcileReply,
        now_ms: u64,
    ) -> io::Result<ReconcileOutcome> {
        if self.active.as_deref() == Some(id) {
            return Ok(ReconcileOutcome::Stale);
        }
        let Some(item) = self.items.get(id).cloned() else {
            return Ok(ReconcileOutcome::Stale);
        };
        let Some(review) = &item.review else {
            return Ok(ReconcileOutcome::Stale);
        };
        if review.journal != expected_journal
            || review.import_id != expected_import_id
            || review.expired
        {
            return Ok(ReconcileOutcome::Stale);
        }
        match reply {
            ReconcileReply::Confirmed => {
                self.delivered_at(id, expected_journal, Some(expected_import_id), now_ms)?;
                Ok(ReconcileOutcome::Confirmed)
            }
            ReconcileReply::Expired => {
                let mut updated = item;
                if let Some(review) = &mut updated.review {
                    review.expired = true;
                }
                self.put(updated)?;
                Ok(ReconcileOutcome::Expired)
            }
            ReconcileReply::Unchanged | ReconcileReply::AuthStop => Ok(ReconcileOutcome::Unchanged),
        }
    }

    /// Re-queue a `ready to review` / `review expired` item by clearing saved and review markers.
    pub fn send_again(&mut self, id: &str) -> io::Result<bool> {
        if self.active.as_deref() == Some(id) {
            return Ok(false);
        }
        let Some(mut item) = self.items.get(id).cloned() else {
            return Ok(false);
        };
        if item.review.is_none() {
            return Ok(false);
        }
        item.review = None;
        item.saved = None;
        item.upload = None;
        item.refusal = None;
        item.may_have_reached = false;
        self.paused_uploads.remove(id);
        self.put(item)?;
        Ok(true)
    }

    pub fn dismiss_interrupted(&mut self) -> io::Result<()> {
        match fs::remove_file(self.root.join("interrupted.json")) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        self.interrupted = false;
        Ok(())
    }

    pub fn retry(&mut self, id: &str) -> io::Result<()> {
        if self.active.as_deref() == Some(id) {
            return Ok(());
        }
        if let Some(mut item) = self.items.get(id).cloned() {
            if item.refusal.as_ref().is_some_and(|r| r.retryable) {
                item.refusal = None;
                self.put(item)?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub name: String,
    pub state: &'static str,
    pub reason: Option<String>,
    pub retryable: bool,
    pub may_have_reached: bool,
    pub missing_copy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub import_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_kind: Option<String>,
    pub review_expired: bool,
    pub added: u64,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_sent: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_total: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resumable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<&'static str>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub can_cancel: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Document {
    pub recent: String,
    pub recent_receipts: Vec<RecentReceipt>,
    pub hold_reason: Option<String>,
    pub rows: Vec<Row>,
    pub intake: Option<Intake>,
    pub interrupted: bool,
    pub waiting: u64,
    pub ready_to_review: u64,
    pub needs_owner: bool,
    pub available: bool,
}

fn row_sort_rank(state: &str) -> u8 {
    match state {
        "ready_to_review" => 0,
        "review_expired" => 1,
        "refused" => 2,
        "sending" => 3,
        "paused" => 4,
        _ => 5,
    }
}

/// Pure, host-testable projection of the import custody store for Settings and SyncSnapshot.
pub fn build_document(
    store: Option<&Store>,
    sync: &SyncSnapshot,
    journal: &str,
    auth_blocked: bool,
    now_ms: u64,
) -> Document {
    let Some(store) = store else {
        return Document {
            recent: message(
                "windows.ui.couldntreadimportsonthisdevicenothinghasbeenremo",
                &[],
            ),
            recent_receipts: Vec::new(),
            hold_reason: None,
            rows: Vec::new(),
            intake: None,
            interrupted: false,
            waiting: 0,
            ready_to_review: 0,
            needs_owner: true,
            available: false,
        };
    };
    let gate = version_gate(sync.journal_version.as_deref(), sync.journal_version_fresh);
    let version = sync.journal_version.as_deref().unwrap_or("");
    let mut rows = Vec::new();
    let mut pending = store
        .intake
        .as_ref()
        .filter(|intake| intake.copying)
        .map_or(0, |intake| intake.total.saturating_sub(intake.done) as u64);
    let mut attention = 0u64;
    let mut ready_to_review = 0u64;
    let mut review_expired_count = 0u64;

    for item in store.items.values() {
        let active = store.active.as_deref() == Some(&item.id);
        let review = if active {
            None
        } else {
            item.review_for(journal)
        };
        let refusal = if active || review.is_some() {
            None
        } else {
            item.refusal
                .as_ref()
                .filter(|_| item.refused_here(journal, version))
        };

        let upload = item.upload_for(journal);
        let row = if active {
            pending += 1;
            let is_resumable = store.active_resumable || upload.is_some();
            let phase = if is_resumable {
                Some(store.active_phase.unwrap_or(UploadPhase::Sending).as_str())
            } else {
                None
            };
            let bytes_sent = if is_resumable {
                if phase == Some(UploadPhase::Checking.as_str()) {
                    Some(item.size)
                } else {
                    Some(upload.map_or(0, |u| u.next_offset.min(item.size)))
                }
            } else {
                None
            };
            Row {
                id: item.id.clone(),
                name: item.name.clone(),
                state: "sending",
                reason: None,
                retryable: false,
                may_have_reached: item.may_have_reached,
                missing_copy: item.missing_copy,
                import_id: item
                    .progress_for(journal)
                    .map(|s| s.timestamp.clone())
                    .filter(|id| is_valid_import_id(id)),
                reason_code: None,
                review_kind: None,
                review_expired: false,
                added: item.added,
                size: item.size,
                bytes_sent,
                bytes_total: is_resumable.then_some(item.size),
                resumable: is_resumable,
                phase,
                can_cancel: is_resumable && phase != Some("checking"),
            }
        } else if let Some(review) = review {
            if review.expired {
                review_expired_count += 1;
            } else {
                ready_to_review += 1;
            }
            Row {
                id: item.id.clone(),
                name: item.name.clone(),
                state: if review.expired {
                    "review_expired"
                } else {
                    "ready_to_review"
                },
                reason: Some(if review.expired {
                    message("windows.import.reviewexpired", &[])
                } else if review.kind == "body_confirmation_required" {
                    message("windows.import.reviewdetailbody", &[])
                } else {
                    message("windows.import.reviewdetail", &[])
                }),
                retryable: review.expired,
                may_have_reached: true,
                missing_copy: item.missing_copy,
                import_id: Some(review.import_id.clone()),
                reason_code: Some(review.reason_code.clone()),
                review_kind: Some(review.kind.clone()),
                review_expired: review.expired,
                added: item.added,
                size: item.size,
                bytes_sent: None,
                bytes_total: None,
                resumable: false,
                phase: None,
                can_cancel: false,
            }
        } else if let Some(refusal) = refusal {
            attention += 1;
            Row {
                id: item.id.clone(),
                name: item.name.clone(),
                state: "refused",
                reason: Some(refusal.sentence.clone()),
                retryable: refusal.retryable,
                may_have_reached: item.may_have_reached,
                missing_copy: item.missing_copy,
                import_id: item
                    .progress_for(journal)
                    .map(|s| s.timestamp.clone())
                    .filter(|id| is_valid_import_id(id)),
                reason_code: refusal.reason_code.clone(),
                review_kind: refusal.kind.clone(),
                review_expired: false,
                added: item.added,
                size: item.size,
                bytes_sent: upload.map(|u| u.next_offset.min(item.size)),
                bytes_total: upload.map(|_| item.size),
                resumable: upload.is_some(),
                phase: None,
                can_cancel: false,
            }
        } else if store.paused_uploads.contains(&item.id) || upload.is_some() {
            pending += 1;
            Row {
                id: item.id.clone(),
                name: item.name.clone(),
                state: "paused",
                reason: None,
                retryable: false,
                may_have_reached: item.may_have_reached,
                missing_copy: item.missing_copy,
                import_id: item
                    .progress_for(journal)
                    .map(|s| s.timestamp.clone())
                    .filter(|id| is_valid_import_id(id)),
                reason_code: None,
                review_kind: None,
                review_expired: false,
                added: item.added,
                size: item.size,
                bytes_sent: Some(upload.map_or(0, |u| u.next_offset.min(item.size))),
                bytes_total: Some(item.size),
                resumable: true,
                phase: Some(UploadPhase::Paused.as_str()),
                can_cancel: true,
            }
        } else {
            pending += 1;
            Row {
                id: item.id.clone(),
                name: item.name.clone(),
                state: "waiting",
                reason: None,
                retryable: false,
                may_have_reached: item.may_have_reached,
                missing_copy: item.missing_copy,
                import_id: item
                    .progress_for(journal)
                    .map(|s| s.timestamp.clone())
                    .filter(|id| is_valid_import_id(id)),
                reason_code: None,
                review_kind: None,
                review_expired: false,
                added: item.added,
                size: item.size,
                bytes_sent: None,
                bytes_total: None,
                resumable: false,
                phase: None,
                can_cancel: false,
            }
        };
        rows.push(row);
    }

    rows.sort_by(|a, b| {
        row_sort_rank(a.state)
            .cmp(&row_sort_rank(b.state))
            .then_with(|| Reverse(a.added).cmp(&Reverse(b.added)))
            .then_with(|| a.id.cmp(&b.id))
    });

    let hold_reason = if pending > 0 {
        if auth_blocked || sync.pairing.phase == PairingPhase::Failed {
            Some(message(
                "windows.ui.yourjournalstoppedacceptingthisdeviceimportswait",
                &[],
            ))
        } else {
            match sync.pairing.phase {
                PairingPhase::NotPaired | PairingPhase::Pairing => Some(message(
                    "windows.ui.importswaitonthisdeviceuntilyouconnectajournal",
                    &[],
                )),
                PairingPhase::AwaitingConfirmation => Some(message(
                    "windows.ui.importswaituntilyouconfirmyourjournalsmark",
                    &[],
                )),
                PairingPhase::Failed => Some(message(
                    "windows.ui.yourjournalstoppedacceptingthisdeviceimportswait",
                    &[],
                )),
                PairingPhase::Paired => {
                    if gate == VersionGate::Old {
                        Some(message(
                            "windows.ui.importswaituntilyourjournalisupdated",
                            &[],
                        ))
                    } else if sync.journal_link == Some(JournalLink::Offline) {
                        Some(message("windows.import.holdoffline", &[]))
                    } else {
                        None
                    }
                }
            }
        }
    } else {
        None
    };

    let recent_receipts = if sync.pairing.phase == PairingPhase::Paired && !journal.is_empty() {
        store.recent_for(journal, now_ms)
    } else {
        Vec::new()
    };

    let recent = if let Some(hold) = &hold_reason {
        hold.clone()
    } else if pending > 0 {
        match gate {
            VersionGate::Unknown => message("windows.ui.checkingyourjournal", &[]),
            VersionGate::Old => message("windows.ui.importswaituntilyourjournalisupdated", &[]),
            VersionGate::Supported if store.active.is_some() => {
                message("windows.import.rowsending", &[])
            }
            VersionGate::Supported => message("windows.ui.waitingtogotoyourjournal", &[]),
        }
    } else if attention > 0 || review_expired_count > 0 {
        message(
            "windows.ui.notgoingtoyourjournalrightnoweachonesayswhy",
            &[],
        )
    } else if ready_to_review > 0 {
        message("windows.import.reviewdetail", &[])
    } else if !recent_receipts.is_empty()
        || (sync.pairing.phase == PairingPhase::Paired
            && !journal.is_empty()
            && store.delivered_to.as_deref() == Some(journal))
    {
        message("windows.ui.inyourjournal", &[])
    } else {
        message("windows.ui.nothingyetxec5a8da6", &[])
    };

    Document {
        available: true,
        recent,
        recent_receipts,
        hold_reason,
        rows,
        intake: store.intake.clone(),
        interrupted: store.interrupted,
        waiting: if gate == VersionGate::Old || auth_blocked {
            0
        } else {
            pending
        },
        ready_to_review,
        needs_owner: attention > 0
            || review_expired_count > 0
            || (pending > 0 && gate == VersionGate::Old)
            || store.interrupted
            || (pending > 0 && auth_blocked),
    }
}

/// Pure, host-testable `window.solstoneJournalHost` v2 `import.status` payload builder.
/// Preserves the closed `state` set (`waiting | uploading | attention | delivered | gone`)
/// and emits additive v2 fields for review, expiry, retryability, device noun, and byte size.
pub fn handoff_status_json(
    delivered_row: Option<Option<&str>>,
    store: Option<&Store>,
    journal: &str,
    version: &str,
    id: &str,
) -> serde_json::Value {
    if let Some(row) = delivered_row {
        return json!({
            "state": "delivered",
            "import_id": row.filter(|candidate| is_valid_import_id(candidate)),
        });
    }
    let Some(store) = store else {
        return json!({"state": "gone"});
    };
    let Some(item) = store.items.get(id) else {
        return json!({"state": "gone"});
    };
    let upload = item.upload_for(journal);
    if store.active.as_deref() == Some(id) {
        if store.active_resumable || upload.is_some() {
            let phase = store.active_phase.unwrap_or(UploadPhase::Sending);
            let bytes_sent = if phase == UploadPhase::Checking {
                item.size
            } else {
                upload.map_or(0, |u| u.next_offset.min(item.size))
            };
            return json!({
                "state": "uploading",
                "size": item.size,
                "size_bytes": item.size,
                "bytes_sent": bytes_sent,
                "bytes_total": item.size,
                "resumable": true,
                "phase": phase.as_str(),
                "device_noun": "PC",
            });
        }
        return json!({
            "state": "uploading",
            "size": item.size,
            "size_bytes": item.size,
            "device_noun": "PC",
        });
    }
    if let Some(review) = item.review_for(journal) {
        if review.expired {
            return json!({
                "state": "attention",
                "review_required": false,
                "review_expired": true,
                "reason_code": review.reason_code,
                "kind": review.kind,
                "import_id": review.import_id,
                "retryable": true,
                "reason": message("windows.import.reviewexpired", &[]),
                "device_noun": "PC",
                "size_bytes": item.size,
            });
        }
        return json!({
            "state": "attention",
            "review_required": true,
            "review_expired": false,
            "reason_code": review.reason_code,
            "kind": review.kind,
            "import_id": review.import_id,
            "retryable": false,
            "reason": message("windows.import.reviewdetail", &[]),
            "device_noun": "PC",
            "size_bytes": item.size,
        });
    }
    if let Some(refusal) = item
        .refusal
        .as_ref()
        .filter(|_| item.refused_here(journal, version))
    {
        return json!({
            "state": "attention",
            "review_required": false,
            "review_expired": false,
            "retryable": refusal.retryable,
            "reason": refusal.sentence,
            "reason_code": refusal.reason_code,
            "kind": refusal.kind,
            "import_id": item
                .progress_for(journal)
                .map(|saved| saved.timestamp.as_str())
                .filter(|candidate| is_valid_import_id(candidate)),
            "device_noun": "PC",
            "size_bytes": item.size,
        });
    }
    if store.paused_uploads.contains(id) || upload.is_some() {
        return json!({
            "state": "waiting",
            "size": item.size,
            "size_bytes": item.size,
            "bytes_sent": upload.map_or(0, |u| u.next_offset.min(item.size)),
            "bytes_total": item.size,
            "resumable": true,
            "phase": UploadPhase::Paused.as_str(),
            "device_noun": "PC",
        });
    }
    json!({
        "state": "waiting",
        "size_bytes": item.size,
        "device_noun": "PC",
    })
}

/// Copy once at the owner's gesture. A staging directory is never deliverable;
/// only the final rename publishes both the complete bytes and their identity.
pub fn stage(root: &Path, source: &Path, id: String, now: u64) -> Result<Item, IntakeRefusal> {
    stage_with_converter(root, source, id, now, None, |_| Err(()))
}

/// Copy once at the owner's gesture, converting HEIC/HEIF photos into JPEG with
/// the platform decoder when selected.
pub fn stage_with_converter<F>(
    root: &Path,
    source: &Path,
    id: String,
    now: u64,
    handoff: Option<Handoff>,
    convert_photo: F,
) -> Result<Item, IntakeRefusal>
where
    F: FnOnce(&[u8]) -> Result<Vec<u8>, ()>,
{
    let name = source
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let refuse = |reason| IntakeRefusal {
        name: name.clone(),
        reason,
    };
    if !valid_id(&id) {
        return Err(refuse(message(
            "windows.ui.thiscouldntbereadadditagain",
            &[],
        )));
    }
    let convertible = is_convertible_photo(source);
    // The journal decides what a file is, so every kind is held and sent.
    let mime = if convertible {
        "image/jpeg"
    } else {
        mime_for(source).unwrap_or("application/octet-stream")
    };
    let mut input = File::open(source)
        .map_err(|_| refuse(message("windows.ui.thiscouldntbereadadditagain", &[])))?;
    let info = input
        .metadata()
        .map_err(|_| refuse(message("windows.ui.thiscouldntbereadadditagain", &[])))?;
    if !info.is_file() {
        return Err(refuse(message(
            "windows.ui.yourjournalcanttakethiskindoffile",
            &[],
        )));
    }
    let source_size = info.len();
    if source_size > MAX_IMPORT_FILE_BYTES {
        return Err(refuse(message(
            "windows.ui.itslargerthanyourjournaltakes",
            &[],
        )));
    }

    let (staged_name, converted_bytes, size, mut source_file) = if convertible {
        let mut raw = Vec::with_capacity(usize::try_from(source_size).unwrap_or(0));
        let copied = io::copy(&mut Read::by_ref(&mut input).take(source_size), &mut raw)
            .map_err(|_| refuse(message("windows.ui.thiscouldntbereadadditagain", &[])))?;
        if copied != source_size {
            return Err(refuse(message(
                "windows.ui.thiscouldntbereadadditagain",
                &[],
            )));
        }
        drop(input);
        let jpeg = convert_photo(&raw)
            .map_err(|_| refuse(message("windows.ui.thisdevicecantconvertthisphoto", &[])))?;
        let jpeg_size = jpeg.len() as u64;
        (converted_jpeg_name(source), Some(jpeg), jpeg_size, None)
    } else {
        (name.clone(), None, source_size, Some(input))
    };

    let item = Item {
        id,
        name: staged_name,
        mime: mime.into(),
        size,
        added: now,
        saved: None,
        refusal: None,
        review: None,
        may_have_reached: false,
        missing_copy: false,
        handoff,
        sha256: None,
        upload: None,
    };
    let framing = ImportFile {
        id: &item.id,
        name: &item.name,
        content_type: &item.mime,
        path: source,
        handoff: item.handoff.as_ref(),
    };
    if size > MAX_IMPORT_FILE_BYTES || framing.request_length(size) > REQUEST_BODY_LIMIT {
        return Err(refuse(message(
            "windows.ui.itslargerthanyourjournaltakes",
            &[],
        )));
    }
    let staging = root.join(format!(".staging-{}", item.id));
    let result = (|| -> io::Result<()> {
        fs::create_dir(&staging)?;
        atomic_json(&staging.join("item.json"), &item)?;
        let mut output = File::create(staging.join("payload"))?;
        if let Some(bytes) = converted_bytes {
            output.write_all(&bytes)?;
        } else if let Some(mut input) = source_file.take() {
            let copied = io::copy(&mut Read::by_ref(&mut input).take(size), &mut output)?;
            if copied != size {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "import source changed",
                ));
            }
            drop(input);
        }
        output.sync_all()?;
        drop(output);
        fs::rename(&staging, root.join(&item.id))
    })();
    if let Err(error) = result {
        let _ = fs::remove_dir_all(staging);
        return Err(refuse(if error.kind() == io::ErrorKind::StorageFull {
            message("windows.ui.thereisntenoughroomonthisdevice", &[])
        } else {
            message("windows.ui.thiscouldntbereadadditagain", &[])
        }));
    }
    Ok(item)
}

pub fn is_convertible_photo(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "heic" | "heif"))
}

pub fn converted_jpeg_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "photo".into());
    format!("{stem}.jpg")
}

/// Content types for the kinds the phones name. Any other file is sent as bytes
/// and the journal decides what it is. Video containers are not inferred as audio.
pub fn mime_for(path: &Path) -> Option<&'static str> {
    Some(
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "pdf" => "application/pdf",
            "txt" | "md" | "markdown" | "log" => "text/plain",
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "mp3" => "audio/mpeg",
            "m4a" => "audio/mp4",
            "aac" => "audio/aac",
            "ogg" | "oga" => "audio/ogg",
            "opus" => "audio/opus",
            "flac" => "audio/flac",
            "wav" => "audio/wav",
            "amr" => "audio/amr",
            "aif" | "aiff" => "audio/aiff",
            _ => return None,
        },
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VersionGate {
    Unknown,
    Old,
    Supported,
}

pub fn version_gate(version: Option<&str>, fresh: bool) -> VersionGate {
    if !fresh {
        return VersionGate::Unknown;
    }
    let Some(version) = version else {
        return VersionGate::Unknown;
    };
    let version = version.trim_start_matches('v');
    let parts: Vec<_> = version.split('.').map(str::parse::<u64>).collect();
    match parts.as_slice() {
        [Ok(major), Ok(minor), Ok(patch)] if (*major, *minor, *patch) >= (2, 0, 52) => {
            VersionGate::Supported
        }
        [Ok(_), Ok(_), Ok(_)] => VersionGate::Old,
        _ => VersionGate::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_fresh_supported_version_opens_delivery() {
        assert_eq!(version_gate(Some("2.0.51"), true), VersionGate::Old);
        assert_eq!(version_gate(Some("2.0.52"), true), VersionGate::Supported);
        assert_eq!(version_gate(Some("2.0.75"), false), VersionGate::Unknown);
        assert_eq!(version_gate(Some("2.0.75x"), true), VersionGate::Unknown);
        assert_eq!(version_gate(None, true), VersionGate::Unknown);
    }

    #[test]
    fn saved_progress_and_refusal_are_not_a_pairing_filter_on_held_material() {
        let item = Item {
            id: "a".repeat(64),
            name: "file.txt".into(),
            mime: "text/plain".into(),
            size: 7,
            added: 1,
            saved: Some(SavedProgress {
                journal: "old".into(),
                saved: SavedImport {
                    path: "p".into(),
                    timestamp: "t".into(),
                },
            }),
            refusal: Some(Refusal {
                journal: "old".into(),
                version: "2.0.52".into(),
                sentence: "refusal".into(),
                retryable: false,
                reason_code: None,
                kind: None,
            }),
            review: None,
            may_have_reached: true,
            missing_copy: false,
            handoff: None,
            sha256: None,
            upload: None,
        };
        assert!(item.progress_for("new").is_none());
        assert!(!item.refused_here("new", "2.0.52"));
        assert!(!item.refused_here("old", "2.0.53"));
        assert!(item.refused_here("old", "2.0.52"));

        // A `409 import_preview_required` refusal never auto-retries on version bump.
        let mut preview_failed = item;
        preview_failed.refusal.as_mut().unwrap().reason_code =
            Some("import_preview_required".into());
        preview_failed.refusal.as_mut().unwrap().kind = Some("plan_failed".into());
        assert!(preview_failed.refused_here("old", "2.0.53"));
        assert!(!preview_failed.refused_here("new", "2.0.53"));
    }

    fn make_item(id_char: char, name: &str, added: u64) -> Item {
        Item {
            id: id_char.to_string().repeat(64),
            name: name.into(),
            mime: "application/zip".into(),
            size: 1234,
            added,
            saved: None,
            refusal: None,
            review: None,
            may_have_reached: false,
            missing_copy: false,
            handoff: None,
            sha256: None,
            upload: None,
        }
    }

    #[test]
    fn deterministic_row_sort_ranks_attention_then_sending_then_waiting_newest_first() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(root.path().join("import")).unwrap();

        // Give SHA-256 hex ids deliberately inverted relative to expected sort order.
        let mut w_old = make_item('a', "waiting-old.txt", 10);
        let mut w_new = make_item('f', "waiting-new.txt", 30);
        let sending = make_item('e', "sending.txt", 20);
        let mut r_old = make_item('d', "refused-old.txt", 15);
        r_old.refusal = Some(Refusal {
            journal: "j1".into(),
            version: "2.0.56".into(),
            sentence: "bad".into(),
            retryable: false,
            reason_code: Some("import_preview_required".into()),
            kind: Some("plan_failed".into()),
        });
        let mut r_new = make_item('b', "refused-new.txt", 40);
        r_new.refusal = Some(Refusal {
            journal: "j1".into(),
            version: "2.0.56".into(),
            sentence: "retryable".into(),
            retryable: true,
            reason_code: Some("import_queue_unreachable".into()),
            kind: None,
        });
        let mut rev = make_item('c', "export.zip", 25);
        rev.review = Some(ReviewState {
            journal: "j1".into(),
            import_id: "20261122_143000".into(),
            reason_code: "import_preview_required".into(),
            kind: "body_confirmation_required".into(),
            expired: false,
        });
        // Ensure waiting items have no review/refusal.
        w_old.refusal = None;
        w_new.refusal = None;

        for item in [w_old, w_new, sending.clone(), r_old, r_new, rev] {
            fs::create_dir_all(store.root().join(&item.id)).unwrap();
            store.put(item).unwrap();
        }
        store.active = Some(sending.id);

        let mut sync = SyncSnapshot::default();
        sync.pairing.phase = PairingPhase::Paired;
        sync.journal_version = Some("2.0.56".into());
        sync.journal_version_fresh = true;
        sync.journal_link = Some(JournalLink::Connected);

        let doc = build_document(Some(&store), &sync, "j1", false, 100_000);
        let names: Vec<&str> = doc.rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "export.zip",      // ready_to_review
                "refused-new.txt", // refused (newest)
                "refused-old.txt", // refused (older)
                "sending.txt",     // sending
                "waiting-new.txt", // waiting (newest)
                "waiting-old.txt", // waiting (older)
            ]
        );
        assert_eq!(doc.ready_to_review, 1);
        assert_eq!(doc.waiting, 3); // sending + 2 waiting; NOT ready_to_review or refused
        assert!(doc.needs_owner); // refused items
    }

    #[test]
    fn recent_json_decodes_legacy_bare_string_and_prunes_v2_receipts_to_24h_and_max_5() {
        let root = tempfile::tempdir().unwrap();
        let import_dir = root.path().join("import");
        fs::create_dir_all(&import_dir).unwrap();

        // Legacy bare JSON string upgrades cleanly.
        fs::write(import_dir.join("recent.json"), br#""journal-legacy""#).unwrap();
        let mut store = Store::open(import_dir.clone()).unwrap();
        assert_eq!(store.delivered_to.as_deref(), Some("journal-legacy"));
        assert!(store.recent.is_empty());

        let base_ms: u64 = 1_795_000_000_000;
        let source = root.path().join("f.txt");
        fs::write(&source, b"payload").unwrap();

        // Deliver 6 files within 24h plus 1 older than 24h on journal-a, and 1 on journal-b.
        for (idx, offset_ms, journal, import_id) in [
            (0u64, 0u64, "journal-a", Some("20261121_000001")), // >24h old at query time
            (
                1,
                RECENT_WINDOW_MS - 60_000,
                "journal-a",
                Some("20261122_000001"),
            ),
            (
                2,
                RECENT_WINDOW_MS - 50_000,
                "journal-a",
                Some("20261122_000002"),
            ),
            (
                3,
                RECENT_WINDOW_MS - 40_000,
                "journal-a",
                Some("20261122_000003"),
            ),
            (
                4,
                RECENT_WINDOW_MS - 30_000,
                "journal-a",
                Some("20261122_000004"),
            ),
            (5, RECENT_WINDOW_MS - 20_000, "journal-a", None), // e.g. content_already_imported
            (
                6,
                RECENT_WINDOW_MS - 10_000,
                "journal-a",
                Some("20261122_000006"),
            ),
            (
                7,
                RECENT_WINDOW_MS - 5_000,
                "journal-b",
                Some("20261122_000007"),
            ),
        ] {
            let id = format!("{:064x}", idx + 1);
            let mut item = stage(store.root(), &source, id.clone(), idx + 1).unwrap();
            item.name = format!("file-{idx}.txt");
            store.put(item).unwrap();
            assert!(store.payload(&id).exists());
            store
                .delivered_at(&id, journal, import_id, base_ms + offset_ms)
                .unwrap();
            assert!(
                !store.payload(&id).exists(),
                "local bytes deleted on delivery"
            );
        }

        let query_ms = base_ms + RECENT_WINDOW_MS + 1;
        let a_recent = store.recent_for("journal-a", query_ms);
        assert_eq!(a_recent.len(), 5);
        assert_eq!(
            a_recent
                .iter()
                .map(|r| r.filename.as_str())
                .collect::<Vec<_>>(),
            vec![
                "file-6.txt",
                "file-5.txt",
                "file-4.txt",
                "file-3.txt",
                "file-2.txt",
            ]
        );
        assert_eq!(a_recent[1].import_id, None);
        assert_eq!(a_recent[0].import_id.as_deref(), Some("20261122_000006"));

        let b_recent = store.recent_for("journal-b", query_ms);
        assert_eq!(b_recent.len(), 1);
        assert_eq!(b_recent[0].filename, "file-7.txt");

        // Unpaired / unconfirmed hides recent receipts.
        let mut sync = SyncSnapshot::default();
        sync.pairing.phase = PairingPhase::AwaitingConfirmation;
        assert!(
            build_document(Some(&store), &sync, "journal-a", false, query_ms)
                .recent_receipts
                .is_empty()
        );
        sync.pairing.phase = PairingPhase::Paired;
        assert_eq!(
            build_document(Some(&store), &sync, "journal-a", false, query_ms)
                .recent_receipts
                .len(),
            5
        );
    }

    #[test]
    fn reconcile_and_repair_reset_preserve_bytes_until_confirmed_and_reject_stale() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(root.path().join("import")).unwrap();
        let source = root.path().join("export.zip");
        fs::write(&source, b"apple-health-bytes").unwrap();

        let id1 = format!("{:064x}", 10);
        let mut item1 = stage(store.root(), &source, id1.clone(), 100).unwrap();
        item1.saved = Some(SavedProgress {
            journal: "journal-a".into(),
            saved: SavedImport {
                path: "/imports/20261122_143000/export.zip".into(),
                timestamp: "20261122_143000".into(),
            },
        });
        item1.review = Some(ReviewState {
            journal: "journal-a".into(),
            import_id: "20261122_143000".into(),
            reason_code: "import_preview_required".into(),
            kind: "body_confirmation_required".into(),
            expired: false,
        });
        store.put(item1).unwrap();

        // Unchanged & AuthStop preserve local bytes.
        assert_eq!(
            store
                .apply_reconcile(
                    &id1,
                    "20261122_143000",
                    "journal-a",
                    ReconcileReply::Unchanged,
                    5000
                )
                .unwrap(),
            ReconcileOutcome::Unchanged
        );
        assert!(store.payload(&id1).exists());

        // Stale mismatched journal or import_id is discarded safely.
        assert_eq!(
            store
                .apply_reconcile(
                    &id1,
                    "20261122_143000",
                    "journal-b",
                    ReconcileReply::Confirmed,
                    5000
                )
                .unwrap(),
            ReconcileOutcome::Stale
        );
        assert!(store.payload(&id1).exists());

        // 404 marks expired AND preserves local bytes; send_again resets to waiting.
        assert_eq!(
            store
                .apply_reconcile(
                    &id1,
                    "20261122_143000",
                    "journal-a",
                    ReconcileReply::Expired,
                    5000
                )
                .unwrap(),
            ReconcileOutcome::Expired
        );
        assert!(store.payload(&id1).exists());
        assert!(store.items[&id1].review.as_ref().unwrap().expired);

        // Stale Confirmed after expiry or send_again is rejected.
        assert_eq!(
            store
                .apply_reconcile(
                    &id1,
                    "20261122_143000",
                    "journal-a",
                    ReconcileReply::Confirmed,
                    6000
                )
                .unwrap(),
            ReconcileOutcome::Stale
        );
        assert!(store.send_again(&id1).unwrap());
        assert!(store.items[&id1].review.is_none());
        assert!(store.items[&id1].saved.is_none());
        assert!(store.payload(&id1).exists());

        // Confirmed writes 24h recent receipt durably BEFORE deleting bytes.
        let mut item1_again = store.items[&id1].clone();
        item1_again.review = Some(ReviewState {
            journal: "journal-a".into(),
            import_id: "20261122_150000".into(),
            reason_code: "import_preview_required".into(),
            kind: "body_confirmation_required".into(),
            expired: false,
        });
        store.put(item1_again).unwrap();
        assert_eq!(
            store
                .apply_reconcile(
                    &id1,
                    "20261122_150000",
                    "journal-a",
                    ReconcileReply::Confirmed,
                    9000
                )
                .unwrap(),
            ReconcileOutcome::Confirmed
        );
        assert!(!store.payload(&id1).exists());
        let recent = store.recent_for("journal-a", 9500);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].import_id.as_deref(), Some("20261122_150000"));

        // Re-pair to a different journal resets ready_to_review and review_expired items to waiting.
        let id2 = format!("{:064x}", 11);
        let mut item2 = stage(store.root(), &source, id2.clone(), 200).unwrap();
        item2.saved = Some(SavedProgress {
            journal: "journal-a".into(),
            saved: SavedImport {
                path: "/imports/20261122_150100/export.zip".into(),
                timestamp: "20261122_150100".into(),
            },
        });
        item2.review = Some(ReviewState {
            journal: "journal-a".into(),
            import_id: "20261122_150100".into(),
            reason_code: "import_preview_required".into(),
            kind: "body_confirmation_required".into(),
            expired: false,
        });
        store.put(item2).unwrap();
        assert!(store.reset_foreign_reviews("journal-b").unwrap());
        assert!(store.items[&id2].review.is_none());
        assert!(store.items[&id2].saved.is_none());
        assert!(store.payload(&id2).exists());
    }

    #[test]
    fn handoff_status_emits_additive_v2_fields_without_breaking_states() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(root.path().join("import")).unwrap();
        let mut item = make_item('a', "export.zip", 10);
        item.size = 42_000;
        item.review = Some(ReviewState {
            journal: "j1".into(),
            import_id: "20261122_143000".into(),
            reason_code: "import_preview_required".into(),
            kind: "body_confirmation_required".into(),
            expired: false,
        });
        fs::create_dir_all(store.root().join(&item.id)).unwrap();
        store.put(item.clone()).unwrap();

        let ready = handoff_status_json(None, Some(&store), "j1", "2.0.56", &item.id);
        assert_eq!(ready["state"], "attention");
        assert_eq!(ready["review_required"], true);
        assert_eq!(ready["review_expired"], false);
        assert_eq!(ready["reason_code"], "import_preview_required");
        assert_eq!(ready["kind"], "body_confirmation_required");
        assert_eq!(ready["import_id"], "20261122_143000");
        assert_eq!(ready["retryable"], false);
        assert_eq!(ready["device_noun"], "PC");
        assert_eq!(ready["size_bytes"], 42_000);
        assert_eq!(
            ready["reason"],
            "saved in your journal. preview and confirm to start."
        );

        item.review.as_mut().unwrap().expired = true;
        store.put(item.clone()).unwrap();
        let expired = handoff_status_json(None, Some(&store), "j1", "2.0.56", &item.id);
        assert_eq!(expired["state"], "attention");
        assert_eq!(expired["review_required"], false);
        assert_eq!(expired["review_expired"], true);
        assert_eq!(expired["retryable"], true);
        assert_eq!(
            expired["reason"],
            "the waiting copy in your journal expired. this device still holds the file."
        );

        item.review = None;
        store.put(item.clone()).unwrap();
        store.active = Some(item.id.clone());
        let uploading = handoff_status_json(None, Some(&store), "j1", "2.0.56", &item.id);
        assert_eq!(uploading["state"], "uploading");
        assert_eq!(uploading["size"], 42_000);
        assert_eq!(uploading["size_bytes"], 42_000);
        assert_eq!(uploading["device_noun"], "PC");

        store.active = None;
        let waiting = handoff_status_json(None, Some(&store), "j1", "2.0.56", &item.id);
        assert_eq!(waiting["state"], "waiting");
        assert_eq!(waiting["size_bytes"], 42_000);
        assert_eq!(waiting["device_noun"], "PC");

        let delivered = handoff_status_json(
            Some(Some("20261122_143000")),
            Some(&store),
            "j1",
            "2.0.56",
            &item.id,
        );
        assert_eq!(delivered["state"], "delivered");
        assert_eq!(delivered["import_id"], "20261122_143000");

        let gone = handoff_status_json(None, Some(&store), "j1", "2.0.56", "missing");
        assert_eq!(gone["state"], "gone");
    }

    #[cfg(feature = "transport-tests")]
    #[test]
    fn copying_and_reopening_preserve_bytes_and_saved_progress() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("note.txt");
        fs::write(&source, b"chosen bytes").unwrap();
        let mut store = Store::open(root.path().join("import")).unwrap();
        let mut item = stage(store.root(), &source, new_id().unwrap(), 1).unwrap();
        fs::write(&source, b"later edit").unwrap();
        assert_eq!(fs::read(store.payload(&item.id)).unwrap(), b"chosen bytes");
        item.saved = Some(SavedProgress {
            journal: "one".into(),
            saved: SavedImport {
                path: "saved".into(),
                timestamp: "time".into(),
            },
        });
        item.may_have_reached = true;
        store.put(item.clone()).unwrap();
        let mut reopened = Store::open(store.root().into()).unwrap();
        assert_eq!(
            reopened.items[&item.id].progress_for("one"),
            item.progress_for("one")
        );
        reopened.active = Some(item.id.clone());
        assert!(!reopened.remove(&item.id).unwrap());
        reopened.delivered(&item.id, "one").unwrap();
        assert!(!reopened.payload(&item.id).exists());
        assert_eq!(
            Store::open(reopened.root().into())
                .unwrap()
                .delivered_to
                .as_deref(),
            Some("one")
        );
    }

    #[cfg(feature = "transport-tests")]
    #[test]
    fn confirmation_reserves_a_copy_and_partial_intake_recovers_once() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("note.txt");
        fs::write(&source, b"held").unwrap();
        let mut store = Store::open(root.path().join("import")).unwrap();
        let item = stage(store.root(), &source, new_id().unwrap(), 1).unwrap();
        store.put(item.clone()).unwrap();
        assert!(!store.remove(&item.id).unwrap());
        store.prepare_remove(&item.id).unwrap();
        assert_eq!(store.confirming_remove.as_deref(), Some(item.id.as_str()));
        store.cancel_remove(Some("another id"));
        assert!(store.confirming_remove.is_some());
        store.cancel_remove(Some(&item.id));
        assert!(!store.remove(&item.id).unwrap());
        store.active = Some(item.id.clone());
        assert!(store.prepare_remove(&item.id).is_none());
        store.active = None;
        store.prepare_remove(&item.id).unwrap();
        assert!(store.remove(&item.id).unwrap());
        // The owner may already have removed the whole directory externally.
        // Confirmation still clears its in-memory row without recreating it.
        let gone = stage(store.root(), &source, new_id().unwrap(), 2).unwrap();
        store.put(gone.clone()).unwrap();
        fs::remove_dir_all(store.root().join(&gone.id)).unwrap();
        store.prepare_remove(&gone.id).unwrap();
        assert!(store.remove(&gone.id).unwrap());
        assert!(!store.items.contains_key(&gone.id));
        let delivered = stage(store.root(), &source, new_id().unwrap(), 3).unwrap();
        store.put(delivered.clone()).unwrap();
        fs::remove_dir_all(store.root().join(&delivered.id)).unwrap();
        store.delivered(&delivered.id, "one").unwrap();
        assert!(!store.items.contains_key(&delivered.id));
        let partial = store.root().join(format!(".staging-{}", new_id().unwrap()));
        fs::create_dir(&partial).unwrap();
        fs::write(partial.join("payload"), b"unfinished").unwrap();
        let mut recovered = Store::open(store.root().into()).unwrap();
        assert!(recovered.interrupted);
        assert!(!partial.exists());
        assert!(Store::open(store.root().into()).unwrap().interrupted);
        recovered.dismiss_interrupted().unwrap();
        assert!(!Store::open(store.root().into()).unwrap().interrupted);
    }

    #[cfg(feature = "transport-tests")]
    #[test]
    fn any_file_is_held_and_a_handoff_survives_reopening() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path().join("import")).unwrap();
        let export = root.path().join("chatgpt-export.zip");
        fs::write(&export, b"PK zip bytes").unwrap();
        let handoff = Handoff {
            area: "chatgpt".into(),
            context: serde_json::from_str(r#"{"setting":"s","future":{"nested":[1,2]}}"#).unwrap(),
        };
        let item = stage_with_converter(
            store.root(),
            &export,
            new_id().unwrap(),
            1,
            Some(handoff.clone()),
            |_| Err(()),
        )
        .unwrap();
        assert_eq!(item.mime, "application/octet-stream");
        let unnamed = root.path().join("no-extension");
        fs::write(&unnamed, b"bytes").unwrap();
        let plain = stage(store.root(), &unnamed, new_id().unwrap(), 2).unwrap();
        assert!(plain.handoff.is_none());
        let reopened = Store::open(store.root().into()).unwrap();
        assert_eq!(reopened.items[&item.id].handoff.as_ref(), Some(&handoff));
        assert_eq!(
            fs::read(reopened.payload(&item.id)).unwrap(),
            b"PK zip bytes"
        );
        assert_eq!(reopened.items[&plain.id].name, "no-extension");
    }

    #[cfg(feature = "transport-tests")]
    #[test]
    fn failed_open_never_cleans_up_material() {
        let root = tempfile::tempdir().unwrap();
        let ready = root.path().join("a".repeat(64));
        fs::create_dir(&ready).unwrap();
        fs::write(ready.join("item.json"), b"unreadable metadata").unwrap();
        let partial = root.path().join(format!(".staging-{}", "b".repeat(64)));
        fs::create_dir(&partial).unwrap();
        fs::write(partial.join("payload"), b"partial").unwrap();
        assert!(Store::open(root.path().into()).is_err());
        assert_eq!(fs::read(partial.join("payload")).unwrap(), b"partial");
    }

    #[cfg(feature = "transport-tests")]
    #[test]
    fn heic_photo_converts_to_jpeg_in_custody_and_refuses_when_undecodable() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path().join("import")).unwrap();
        let heic = root.path().join("IMG_0412.HEIC");
        fs::write(&heic, b"fake-heic-bytes").unwrap();

        let item = stage_with_converter(store.root(), &heic, new_id().unwrap(), 10, None, |raw| {
            assert_eq!(raw, b"fake-heic-bytes");
            Ok(b"\xff\xd8converted-jpeg\xff\xd9".to_vec())
        })
        .unwrap();
        assert_eq!(item.name, "IMG_0412.jpg");
        assert_eq!(item.mime, "image/jpeg");
        assert_eq!(
            fs::read(store.payload(&item.id)).unwrap(),
            b"\xff\xd8converted-jpeg\xff\xd9"
        );

        let heif = root.path().join("IMG_0413.heif");
        fs::write(&heif, b"bad-heif").unwrap();
        let err =
            stage_with_converter(
                store.root(),
                &heif,
                new_id().unwrap(),
                11,
                None,
                |_| Err(()),
            )
            .unwrap_err();
        assert_eq!(err.name, "IMG_0413.heif");
        assert!(!err.reason.is_empty());
        assert_eq!(Store::open(store.root().into()).unwrap().items.len(), 1);
    }

    #[test]
    fn resumable_upload_progress_respects_pairing_switch_and_emits_honest_host_status() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(root.path().join("import")).unwrap();
        let mut item = make_item('b', "archive.zip", 100);
        item.size = 32 * 1024 * 1024;
        item.sha256 = Some("f".repeat(64));
        item.upload = Some(ResumableUploadState {
            journal: "journal-a".into(),
            upload_id: "550e8400-e29b-41d4-a716-446655440000".into(),
            next_offset: 8 * 1024 * 1024,
            total_bytes: 32 * 1024 * 1024,
            chunk_bytes: 4 * 1024 * 1024,
        });
        fs::create_dir_all(store.root().join(&item.id)).unwrap();
        fs::write(store.payload(&item.id), b"payload").unwrap();
        store.put(item.clone()).unwrap();

        let mut sync = SyncSnapshot::default();
        sync.pairing.phase = PairingPhase::Paired;
        sync.journal_version = Some("2.0.59".into());
        sync.journal_version_fresh = true;

        // Before delivery wakes on journal-a, persisted offset renders as paused (not attention).
        let paused_doc = build_document(Some(&store), &sync, "journal-a", false, 1000);
        assert_eq!(paused_doc.waiting, 1);
        assert!(!paused_doc.needs_owner);
        assert_eq!(paused_doc.rows[0].state, "paused");
        assert_eq!(paused_doc.rows[0].bytes_sent, Some(8 * 1024 * 1024));
        assert_eq!(paused_doc.rows[0].bytes_total, Some(32 * 1024 * 1024));
        assert!(paused_doc.rows[0].resumable);
        assert_eq!(paused_doc.rows[0].phase, Some("paused"));
        assert!(paused_doc.rows[0].can_cancel);

        let host_paused = handoff_status_json(None, Some(&store), "journal-a", "2.0.59", &item.id);
        assert_eq!(host_paused["state"], "waiting");
        assert_eq!(host_paused["bytes_sent"], 8 * 1024 * 1024);
        assert_eq!(host_paused["bytes_total"], 32 * 1024 * 1024);
        assert_eq!(host_paused["resumable"], true);
        assert_eq!(host_paused["phase"], "paused");
        assert_eq!(host_paused["device_noun"], "PC");

        // Active sending and checking phases emit honest live fields.
        store.set_active(item.id.clone(), true, Some(UploadPhase::Sending));
        let host_sending = handoff_status_json(None, Some(&store), "journal-a", "2.0.59", &item.id);
        assert_eq!(host_sending["state"], "uploading");
        assert_eq!(host_sending["bytes_sent"], 8 * 1024 * 1024);
        assert_eq!(host_sending["bytes_total"], 32 * 1024 * 1024);
        assert_eq!(host_sending["resumable"], true);
        assert_eq!(host_sending["phase"], "sending");

        store.active_phase = Some(UploadPhase::Checking);
        let checking_doc = build_document(Some(&store), &sync, "journal-a", false, 1000);
        assert_eq!(checking_doc.rows[0].phase, Some("checking"));
        assert!(!checking_doc.rows[0].can_cancel);
        let host_checking =
            handoff_status_json(None, Some(&store), "journal-a", "2.0.59", &item.id);
        assert_eq!(host_checking["phase"], "checking");

        // Pairing-switch rule: switching to journal-b clears upload_id/next_offset
        // while preserving local custody bytes and precomputed sha256.
        store.clear_active();
        assert!(store.reset_foreign_uploads("journal-b").unwrap());
        let reset = &store.items[&item.id];
        assert!(reset.upload.is_none());
        assert_eq!(reset.sha256.as_deref(), Some("f".repeat(64).as_str()));
        assert_eq!(fs::read(store.payload(&item.id)).unwrap(), b"payload");
        let fresh_doc = build_document(Some(&store), &sync, "journal-b", false, 1000);
        assert_eq!(fresh_doc.rows[0].state, "waiting");
        assert!(!fresh_doc.rows[0].resumable);

        // Reopen backward compatibility: legacy item.json without sha256/upload opens cleanly.
        let legacy_id = "c".repeat(64);
        let legacy_dir = store.root().join(&legacy_id);
        fs::create_dir_all(&legacy_dir).unwrap();
        fs::write(
            legacy_dir.join("item.json"),
            format!(
                r#"{{"id":"{legacy_id}","name":"old.txt","mime":"text/plain","size":5,"added":1,"saved":null,"refusal":null}}"#
            ),
        )
        .unwrap();
        let reopened = Store::open(store.root().into()).unwrap();
        assert!(reopened.items[&legacy_id].sha256.is_none());
        assert!(reopened.items[&legacy_id].upload.is_none());
    }

    #[test]
    fn cancel_upload_removes_custody_and_returns_remote_cursor() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(root.path().join("import")).unwrap();
        let mut item = make_item('d', "video.mp4", 10);
        item.upload = Some(ResumableUploadState {
            journal: "journal-a".into(),
            upload_id: "upload-abc".into(),
            next_offset: 4 * 1024 * 1024,
            total_bytes: 20 * 1024 * 1024,
            chunk_bytes: 4 * 1024 * 1024,
        });
        fs::create_dir_all(store.root().join(&item.id)).unwrap();
        fs::write(store.payload(&item.id), b"bytes").unwrap();
        store.put(item.clone()).unwrap();
        store.set_active(item.id.clone(), true, Some(UploadPhase::Sending));

        let cancelled = store.cancel_upload(&item.id).unwrap().unwrap();
        assert_eq!(cancelled.upload_id, "upload-abc");
        assert!(store.active.is_none());
        assert!(!store.items.contains_key(&item.id));
        assert!(!store.root().join(&item.id).exists());
    }
}
