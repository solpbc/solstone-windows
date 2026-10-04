// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Durable acceptance and custody of browser text on this PC.
//!
//! ```text
//! <root>/open/<period>/period.json
//! <root>/open/<period>/<seq>-<ih>-<batch_id>.jsonl
//! <root>/receipts/<ih>-<batch_id>.json
//! <root>/outbox/<start>-<period>/{browser_pages.jsonl,period.json}
//! <root>/outbox/<start>-<period>/terminal.json
//! ```
//!
//! Batch bytes are committed before their receipt. A missing receipt is repaired
//! from the open batch filename on restart. Receipts remain as dedup tombstones
//! after delivery or discard.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use native_browser_frame::{
    canonical_stringify, FILE_MAX, FUTURE_SKEW_MS_MAX, SPOOL_AGE_MS_MAX, SPOOL_BYTES_MAX,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::identity::hex;

const PERIOD_METADATA_BYTES: u64 = 1024;
const TERMINAL_METADATA_BYTES: u64 = 64;
const RECEIPT_METADATA_BYTES: u64 = 256;

/// The bounds the store enforces. Defaults are the contract's policy.
#[derive(Debug, Clone)]
pub struct Policy {
    pub spool_bytes: u64,
    pub file_max: u64,
    pub spool_age_ms: u64,
    pub future_skew_ms: u64,
    /// The period grid, aligned with the capture segments.
    pub period_ms: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            spool_bytes: SPOOL_BYTES_MAX as u64,
            file_max: FILE_MAX as u64,
            spool_age_ms: SPOOL_AGE_MS_MAX,
            future_skew_ms: FUTURE_SKEW_MS_MAX,
            period_ms: 300_000,
        }
    }
}

/// Names a finalized period for the journal: `(YYYYMMDD, HHMMSS_LEN)` for a
/// period starting at `start_secs` (Unix) lasting `len_secs`, in local time.
pub type Namer = Box<dyn Fn(u64, u64) -> (String, String) + Send + Sync>;

/// The result of offering one batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchResult {
    Accepted { period_id: String },
    Duplicate { period_id: String },
    Rejected { reason: &'static str },
}

impl BatchResult {
    /// The contract class of a rejection reason.
    pub fn class(reason: &str) -> &'static str {
        if native_browser_frame::PERMANENT_REASONS.contains(&reason) {
            "permanent"
        } else {
            "retryable"
        }
    }
}

/// One batch as the session decoded it.
pub struct BatchInput<'a> {
    pub generation: &'a str,
    pub inst: &'a str,
    pub batch_id: &'a str,
    pub queued_at_ms: u64,
    pub records: &'a [Value],
}

/// A finalized period waiting for delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxEntry {
    pub dir: PathBuf,
    pub period_id: String,
    pub day: String,
    pub segment: String,
    pub start_secs: u64,
    pub len_secs: u64,
    pub size: u64,
    pub sha256: String,
}

impl OutboxEntry {
    pub fn pages_path(&self) -> PathBuf {
        self.dir.join(crate::PAGES_FILE)
    }
}

/// What the store can report about its pending custody.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CustodyStatus {
    pub period_id: Option<String>,
    pub held_bytes: u64,
    pub held_periods: usize,
    pub full: bool,
    pub stale: bool,
    pub failed: bool,
    pub waiting: bool,
}

#[derive(Serialize, Deserialize)]
struct OpenPeriodFile {
    period_id: String,
    start_ms: u64,
    end_ms: u64,
}

#[derive(Serialize, Deserialize)]
struct OutboxPeriodFile {
    period_id: String,
    day: String,
    segment: String,
    start_ms: u64,
    len_secs: u64,
    size: u64,
    sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReceiptState {
    Pending,
    Discarded,
    Delivered,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReceiptFile {
    period_id: String,
    at_ms: u64,
    state: ReceiptState,
}

#[derive(Serialize, Deserialize)]
struct OutboxTerminalFile {
    state: ReceiptState,
}

struct OpenPeriod {
    id: String,
    start_ms: u64,
    end_ms: u64,
    bytes: u64,
    contexts: HashSet<String>,
    has_dir: bool,
}

struct Active {
    open: OpenPeriod,
    receipts: HashMap<(String, String), ReceiptFile>,
    next_seq: u64,
    reserved: HashSet<PathBuf>,
}

pub struct Store {
    root: PathBuf,
    policy: Policy,
    namer: Namer,
    active: Option<Active>,
    failed: bool,
    full: bool,
    counter: u64,
    #[cfg(test)]
    stop_after_durable_writes: Option<usize>,
    #[cfg(test)]
    fail_next_payload_removes: usize,
}

impl Store {
    /// Open the store, completing any interrupted tombstones before loading
    /// content. Periods left open by a previous run are finalized as they stand.
    pub fn open(root: impl Into<PathBuf>, policy: Policy, namer: Namer, now_ms: u64) -> Self {
        let mut store = Store {
            root: root.into(),
            policy,
            namer,
            active: None,
            failed: false,
            full: false,
            counter: 0,
            #[cfg(test)]
            stop_after_durable_writes: None,
            #[cfg(test)]
            fail_next_payload_removes: 0,
        };
        if let Err(error) = store.load(now_ms) {
            tracing::warn!(target: "browser", component = "custody", outcome = "open_failed", error = %error, "custody open");
            store.failed = true;
            store.active = None;
        }
        store
    }

    fn load(&mut self, now_ms: u64) -> io::Result<()> {
        for legacy in [self.root.join("gen"), self.root.join("retired")] {
            if legacy.exists() {
                fs::remove_dir_all(legacy)?;
            }
        }
        let active_path = self.root.join("active.json");
        if active_path.exists() {
            fs::remove_file(active_path)?;
        }
        fs::create_dir_all(self.root.join("open"))?;
        fs::create_dir_all(self.root.join("receipts"))?;
        fs::create_dir_all(self.root.join("outbox"))?;

        let mut active = Active {
            open: self.new_open(now_ms, false),
            receipts: HashMap::new(),
            next_seq: 0,
            reserved: HashSet::new(),
        };
        for entry in fs::read_dir(self.root.join("receipts"))? {
            let path = entry?.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(stem) = name.strip_suffix(".json") else {
                let _ = fs::remove_file(path);
                continue;
            };
            let Some((ih, batch)) = stem.split_once('-') else {
                continue;
            };
            if let Some(receipt) = read_json::<ReceiptFile>(&path)? {
                active
                    .receipts
                    .insert((ih.to_string(), batch.to_string()), receipt);
            }
        }

        // Recover only missing receipts from batch filenames. Empty period
        // directories are dropped and never become offered content.
        let mut open_dirs: Vec<PathBuf> = fs::read_dir(self.root.join("open"))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir())
            .collect();
        open_dirs.sort();
        for dir in &open_dirs {
            let Some(meta) = read_json::<OpenPeriodFile>(&dir.join("period.json"))? else {
                self.remove_payload_dir(dir)?;
                continue;
            };
            let files = batch_files(dir)?;
            if files.is_empty() {
                self.remove_payload_dir(dir)?;
                continue;
            }
            for (_, ih, batch) in files {
                let key = (ih.clone(), batch.clone());
                if let std::collections::hash_map::Entry::Vacant(entry) = active.receipts.entry(key)
                {
                    let receipt = ReceiptFile {
                        period_id: meta.period_id.clone(),
                        at_ms: now_ms,
                        state: ReceiptState::Pending,
                    };
                    self.write_json_atomic(&self.receipt_path(&ih, &batch), &receipt)?;
                    entry.insert(receipt);
                }
            }
        }

        // A finalized directory's marker is the durable intent for that
        // directory. Finish its receipt transitions before removing payload.
        for dir in list_dirs(&self.root.join("outbox"))? {
            if dir
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(".tmp-"))
            {
                continue;
            }
            let Some(marker) = read_json::<OutboxTerminalFile>(&dir.join("terminal.json"))? else {
                continue;
            };
            let Some(meta) = read_json::<OutboxPeriodFile>(&dir.join("period.json"))? else {
                self.remove_payload_dir(&dir)?;
                continue;
            };
            let pending: Vec<_> = active
                .receipts
                .iter()
                .filter(|(_, receipt)| {
                    receipt.period_id == meta.period_id && receipt.state == ReceiptState::Pending
                })
                .map(|(key, receipt)| (key.clone(), receipt.clone()))
                .collect();
            for ((ih, batch), mut receipt) in pending {
                receipt.state = marker.state;
                self.write_json_atomic(&self.receipt_path(&ih, &batch), &receipt)?;
                active.receipts.insert((ih, batch), receipt);
            }
            self.remove_payload_dir(&dir)?;
        }

        // Open files are individually identified by their receipt key. Clean
        // terminal batches after marker recovery so a leftover open copy of a
        // finalized period cannot be restarted after its outbox is removed.
        for dir in &open_dirs {
            if !dir.is_dir() {
                continue;
            }
            for (name, ih, batch) in batch_files(dir)? {
                if active
                    .receipts
                    .get(&(ih, batch))
                    .is_some_and(|receipt| receipt.state != ReceiptState::Pending)
                {
                    fs::remove_file(dir.join(name))?;
                }
            }
            if batch_files(dir)?.is_empty() {
                self.remove_payload_dir(dir)?;
            }
        }

        // Open periods from the previous process are finalized after receipt
        // repair and tombstone recovery.
        for dir in open_dirs {
            if !dir.exists() {
                continue;
            }
            let Some(meta) = read_json::<OpenPeriodFile>(&dir.join("period.json"))? else {
                continue;
            };
            if batch_files(&dir)?.is_empty() {
                continue;
            }
            let period = OpenPeriod {
                id: meta.period_id,
                start_ms: meta.start_ms,
                end_ms: meta.end_ms,
                bytes: dir_payload_bytes(&dir),
                contexts: HashSet::new(),
                has_dir: true,
            };
            self.finalize_dir(&period, &dir, meta.end_ms.min(now_ms))?;
        }
        for dir in list_dirs(&self.root.join("outbox"))? {
            if dir
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(".tmp-"))
            {
                self.remove_payload_dir(&dir)?;
            }
        }
        self.active = Some(active);
        self.refresh_full();
        Ok(())
    }

    fn receipt_path(&self, ih: &str, batch: &str) -> PathBuf {
        self.root
            .join("receipts")
            .join(format!("{ih}-{batch}.json"))
    }

    fn next_token(&mut self, salt: &str, now_ms: u64) -> String {
        self.counter = self.counter.wrapping_add(1);
        let mut h = Sha256::new();
        h.update(salt.as_bytes());
        h.update(now_ms.to_le_bytes());
        h.update(self.counter.to_le_bytes());
        h.update(std::process::id().to_le_bytes());
        h.update(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
                .to_le_bytes(),
        );
        hex(&h.finalize())
    }

    fn new_open(&mut self, now_ms: u64, early: bool) -> OpenPeriod {
        let floor = now_ms - now_ms % self.policy.period_ms;
        let start_ms = if early { now_ms } else { floor };
        let token = self.next_token("period", now_ms);
        OpenPeriod {
            id: format!("{}-{}", start_ms / 1000, &token[..8]),
            start_ms,
            end_ms: floor + self.policy.period_ms,
            bytes: 0,
            contexts: HashSet::new(),
            has_dir: false,
        }
    }

    pub fn period_id(&self) -> Option<&str> {
        self.active.as_ref().map(|a| a.open.id.as_str())
    }

    pub fn status(&self, now_ms: u64) -> CustodyStatus {
        let Some(active) = &self.active else {
            return CustodyStatus {
                failed: self.failed,
                full: self.full,
                ..CustodyStatus::default()
            };
        };
        let entries = self.all_outbox();
        let open_bytes = if active.open.has_dir {
            dir_payload_bytes(&self.open_dir(&active.open.id))
        } else {
            0
        };
        let held_bytes = entries.iter().map(|e| e.size).sum::<u64>() + open_bytes;
        let held_periods = entries.len() + usize::from(open_bytes > 0);
        let oldest = entries
            .iter()
            .map(|e| e.start_secs * 1000)
            .chain((open_bytes > 0).then_some(active.open.start_ms))
            .min();
        CustodyStatus {
            period_id: Some(active.open.id.clone()),
            held_bytes,
            held_periods,
            full: self.full,
            stale: oldest.is_some_and(|at| now_ms.saturating_sub(at) >= self.policy.spool_age_ms),
            failed: self.failed,
            waiting: self.waiting(),
        }
    }

    /// Offer one decoded batch. Generation and age ceilings are connection
    /// concerns; the durable receipt key is only `(inst hash, batch id)`.
    pub fn offer(&mut self, batch: &BatchInput<'_>, now_ms: u64) -> BatchResult {
        if self.failed {
            return BatchResult::Rejected {
                reason: "resource_exhausted",
            };
        }
        let ih = inst_hash(batch.inst);
        if let Some(receipt) = self
            .active
            .as_ref()
            .and_then(|a| a.receipts.get(&(ih.clone(), batch.batch_id.to_string())))
        {
            return BatchResult::Duplicate {
                period_id: receipt.period_id.clone(),
            };
        }
        let open_id = self
            .active
            .as_ref()
            .filter(|active| active.open.has_dir)
            .map(|active| active.open.id.clone());
        if open_id
            .as_deref()
            .is_some_and(|id| self.open_has_terminal_batch(id).unwrap_or(true))
        {
            return BatchResult::Rejected {
                reason: "resource_exhausted",
            };
        }
        if batch.queued_at_ms > now_ms.saturating_add(self.policy.future_skew_ms) {
            return BatchResult::Rejected {
                reason: "age_policy",
            };
        }
        let mut body = String::new();
        for record in batch.records {
            if canonical_stringify(record, &mut body).is_err() {
                return BatchResult::Rejected {
                    reason: "malformed",
                };
            }
            body.push('\n');
        }
        let body_bytes = body.len() as u64;
        if body_bytes > self.policy.file_max {
            return BatchResult::Rejected { reason: "oversize" };
        }

        let open_end = self.active.as_ref().map_or(0, |a| a.open.end_ms);
        if now_ms >= open_end {
            if let Err(e) = self.rotate(now_ms, false) {
                return self.io_rejection(e);
            }
        }
        let open_bytes = self.active.as_ref().map_or(0, |a| a.open.bytes);
        if open_bytes > 0 && open_bytes.saturating_add(body_bytes) > self.policy.file_max {
            if let Err(e) = self.rotate(now_ms, true) {
                return self.io_rejection(e);
            }
        }
        let period_id = self
            .period_id()
            .expect("open store has a current period")
            .to_string();
        let receipt = ReceiptFile {
            period_id: period_id.clone(),
            at_ms: now_ms,
            state: ReceiptState::Pending,
        };
        let receipt_bytes = match serde_json::to_vec(&receipt) {
            Ok(bytes) => (bytes.len() as u64).max(RECEIPT_METADATA_BYTES),
            Err(_) => {
                return BatchResult::Rejected {
                    reason: "resource_exhausted",
                }
            }
        };
        let period_bytes = self.active.as_ref().map_or(0, |active| {
            if active.open.has_dir {
                0
            } else {
                PERIOD_METADATA_BYTES + TERMINAL_METADATA_BYTES
            }
        });
        if self
            .spool_bytes()
            .saturating_add(body_bytes.saturating_mul(2))
            .saturating_add(receipt_bytes)
            .saturating_add(period_bytes)
            > self.policy.spool_bytes
        {
            self.full = true;
            return BatchResult::Rejected {
                reason: "queue_full",
            };
        }
        self.refresh_full();
        let first = batch.records.first();
        let is_delta = first.and_then(|r| r.get("t")).and_then(Value::as_str) == Some("delta");
        let ctx = first
            .and_then(|r| r.get("ctx"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let active = self
            .active
            .as_mut()
            .expect("store load creates an open period");
        if is_delta && !active.open.contexts.contains(&ctx) {
            return BatchResult::Rejected {
                reason: "snapshot_required",
            };
        }
        match commit(self, &ih, batch.batch_id, body.as_bytes(), receipt, now_ms) {
            Ok(()) => {
                if !is_delta {
                    self.active.as_mut().unwrap().open.contexts.insert(ctx);
                }
                BatchResult::Accepted {
                    period_id: self.period_id().unwrap().to_string(),
                }
            }
            Err(e) => self.io_rejection(e),
        }
    }

    fn io_rejection(&mut self, error: io::Error) -> BatchResult {
        tracing::warn!(target: "browser", component = "custody", outcome = "commit_failed", error = %error, "custody commit");
        BatchResult::Rejected {
            reason: "resource_exhausted",
        }
    }

    /// Advance the 300-second period grid. Receipts are durable dedup tombstones
    /// and are never aged out.
    pub fn tick(&mut self, now_ms: u64) -> bool {
        if self.active.as_ref().is_none_or(|a| now_ms < a.open.end_ms) {
            return false;
        }
        if let Err(error) = self.rotate(now_ms, false) {
            tracing::warn!(target: "browser", component = "custody", outcome = "finalize_failed", error = %error, "custody tick");
        }
        true
    }

    pub fn finalize_now(&mut self, now_ms: u64) -> bool {
        if self.active.as_ref().is_some_and(|a| a.open.has_dir) {
            return self.rotate(now_ms, true).is_ok();
        }
        false
    }

    fn rotate(&mut self, now_ms: u64, early: bool) -> io::Result<()> {
        let old_id = self
            .active
            .as_ref()
            .filter(|active| active.open.has_dir)
            .map(|active| active.open.id.clone());
        if old_id
            .as_deref()
            .is_some_and(|id| self.open_has_terminal_batch(id).unwrap_or(true))
        {
            return Err(io::Error::other(
                "open period contains a terminal batch awaiting discard",
            ));
        }
        let next = self.new_open(now_ms, early);
        let active = self.active.as_mut().expect("open store");
        let old = std::mem::replace(&mut active.open, next);
        if old.has_dir {
            let dir = self.open_dir(&old.id);
            let end = old.end_ms.min(now_ms);
            let size = self.finalize_dir(&old, &dir, end)?;
            let active = self.active.as_mut().unwrap();
            active.open.bytes = 0;
            active.open.contexts.clear();
            if size > 0 {
                self.refresh_full();
            }
        }
        Ok(())
    }

    fn finalize_dir(&mut self, period: &OpenPeriod, dir: &Path, end_ms: u64) -> io::Result<u64> {
        let outbox = self.root.join("outbox");
        let name = format!("{:015}-{}", period.start_ms, period.id);
        let final_dir = outbox.join(&name);
        if final_dir.is_dir() {
            self.remove_payload_dir(dir)?;
            return Ok(0);
        }
        let mut files = batch_files(dir)?;
        files.sort();
        let staging = outbox.join(format!(".tmp-{name}"));
        if staging.exists() {
            self.remove_payload_dir(&staging)?;
        }
        fs::create_dir_all(&staging)?;
        let mut out = fs::File::create(staging.join(crate::PAGES_FILE))?;
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        for (seq_name, _, _) in &files {
            let bytes = fs::read(dir.join(seq_name))?;
            hasher.update(&bytes);
            size += bytes.len() as u64;
            out.write_all(&bytes)?;
        }
        out.sync_all()?;
        drop(out);
        if size == 0 {
            self.remove_payload_dir(&staging)?;
            self.remove_payload_dir(dir)?;
            return Ok(0);
        }
        let start_secs = period.start_ms / 1000;
        let len_secs = (end_ms.saturating_sub(period.start_ms) / 1000).max(1);
        let (day, segment) = (self.namer)(start_secs, len_secs);
        let metadata = OutboxPeriodFile {
            period_id: period.id.clone(),
            day,
            segment,
            start_ms: period.start_ms,
            len_secs,
            size,
            sha256: hex(&hasher.finalize()),
        };
        self.write_json_atomic(&staging.join("period.json"), &metadata)?;
        fs::rename(&staging, &final_dir)?;
        self.remove_payload_dir(dir)?;
        Ok(size)
    }

    fn open_dir(&self, id: &str) -> PathBuf {
        self.root.join("open").join(id)
    }

    fn open_has_terminal_batch(&self, id: &str) -> io::Result<bool> {
        let dir = self.open_dir(id);
        if !dir.is_dir() {
            return Ok(false);
        }
        let Some(active) = self.active.as_ref() else {
            return Ok(false);
        };
        for (_, ih, batch) in batch_files(&dir)? {
            if active
                .receipts
                .get(&(ih, batch))
                .is_some_and(|receipt| receipt.state != ReceiptState::Pending)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn all_outbox(&self) -> Vec<OutboxEntry> {
        let mut out = list_dirs(&self.root.join("outbox"))
            .unwrap_or_default()
            .into_iter()
            .filter(|dir| {
                !dir.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(".tmp-"))
            })
            .filter_map(|dir| {
                let meta = read_json::<OutboxPeriodFile>(&dir.join("period.json")).ok()??;
                Some(OutboxEntry {
                    dir,
                    period_id: meta.period_id,
                    day: meta.day,
                    segment: meta.segment,
                    start_secs: meta.start_ms / 1000,
                    len_secs: meta.len_secs,
                    size: meta.size,
                    sha256: meta.sha256,
                })
            })
            .collect::<Vec<_>>();
        out.sort_by(|a, b| a.dir.cmp(&b.dir));
        out
    }

    /// Finalized pending periods, oldest first. Tombstoned payload is never sent.
    pub fn outbox(&self) -> Vec<OutboxEntry> {
        self.all_outbox()
            .into_iter()
            .filter(|e| !e.dir.join("terminal.json").exists())
            .collect()
    }

    pub fn reserve(&mut self, entry: &OutboxEntry) -> bool {
        if entry.dir.join("terminal.json").exists() || !entry.dir.is_dir() {
            return false;
        }
        let Some(active) = self.active.as_mut() else {
            return false;
        };
        active.reserved.insert(entry.dir.clone())
    }

    pub fn release(&mut self, entry: &OutboxEntry) {
        if let Some(active) = self.active.as_mut() {
            active.reserved.remove(&entry.dir);
        }
        self.refresh_full();
    }

    /// Receipt-proven delivery is tombstoned before its payload is removed.
    pub fn delivered(&mut self, entry: &OutboxEntry) -> io::Result<()> {
        let state = self.write_outbox_terminal(&entry.dir, ReceiptState::Delivered)?;
        self.mark_period(&entry.period_id, state, self.now_for_receipt(entry))?;
        self.remove_payload_dir(&entry.dir)?;
        if let Some(active) = self.active.as_mut() {
            active.reserved.remove(&entry.dir);
        }
        self.refresh_full();
        Ok(())
    }

    fn now_for_receipt(&self, _entry: &OutboxEntry) -> u64 {
        // Receipt acceptance time is retained across terminal state changes.
        self.active
            .as_ref()
            .and_then(|a| {
                a.receipts
                    .values()
                    .filter(|r| r.period_id == _entry.period_id)
                    .map(|r| r.at_ms)
                    .max()
            })
            .unwrap_or(0)
    }

    fn mark_period(&mut self, period: &str, target: ReceiptState, at_ms: u64) -> io::Result<()> {
        let keys: Vec<_> = self
            .active
            .as_ref()
            .map(|a| {
                a.receipts
                    .iter()
                    .filter(|(_, r)| r.period_id == period)
                    .map(|(k, _)| k.clone())
                    .collect()
            })
            .unwrap_or_default();
        for (ih, batch) in keys {
            let mut receipt =
                self.active.as_ref().unwrap().receipts[&(ih.clone(), batch.clone())].clone();
            if receipt.state == ReceiptState::Pending {
                receipt.state = target;
                receipt.at_ms = at_ms.max(receipt.at_ms);
                self.write_json_atomic(&self.receipt_path(&ih, &batch), &receipt)?;
                self.active
                    .as_mut()
                    .unwrap()
                    .receipts
                    .insert((ih, batch), receipt);
            }
        }
        Ok(())
    }

    fn write_outbox_terminal(
        &mut self,
        dir: &Path,
        target: ReceiptState,
    ) -> io::Result<ReceiptState> {
        let path = dir.join("terminal.json");
        if let Some(marker) = read_json::<OutboxTerminalFile>(&path)? {
            return Ok(marker.state);
        }
        self.write_json_atomic(&path, &OutboxTerminalFile { state: target })?;
        Ok(target)
    }

    fn waiting(&self) -> bool {
        let Some(active) = &self.active else {
            return false;
        };
        if active.open.has_dir && dir_payload_bytes(&self.open_dir(&active.open.id)) > 0 {
            return true;
        }
        self.all_outbox()
            .iter()
            .any(|e| !active.reserved.contains(&e.dir))
    }

    /// Discard all currently waiting finalized periods and the populated open
    /// period. Reservations exclude in-flight uploads.
    pub fn discard_waiting(&mut self, now_ms: u64) -> usize {
        let Some(active) = &self.active else { return 0 };
        let open = active.open.has_dir.then(|| active.open.id.clone());
        let mut snapshot: Vec<(String, PathBuf, bool)> = self
            .all_outbox()
            .into_iter()
            .filter(|e| !active.reserved.contains(&e.dir))
            .map(|e| (e.period_id, e.dir, false))
            .collect();
        if let Some(id) = open {
            let dir = self.open_dir(&id);
            if dir_payload_bytes(&dir) > 0 {
                snapshot.push((id, dir, true));
            }
        }
        for (period, dir, is_open) in &snapshot {
            let state = if *is_open {
                Ok(ReceiptState::Discarded)
            } else {
                self.write_outbox_terminal(dir, ReceiptState::Discarded)
            };
            let Ok(state) = state else {
                continue;
            };
            if self.mark_period(period, state, now_ms).is_err() {
                continue;
            }
            if self.remove_payload_dir(dir).is_ok() && *is_open {
                if let Some(active) = self.active.as_mut() {
                    active.open.has_dir = false;
                    active.open.bytes = 0;
                    active.open.contexts.clear();
                }
            }
        }
        self.refresh_full();
        snapshot.iter().filter(|(_, dir, _)| dir.exists()).count()
    }

    pub fn spool_bytes(&self) -> u64 {
        // Keep room for the durable finalized copy while open chunks still exist.
        let payload = dir_payload_bytes(&self.root.join("open"))
            .saturating_mul(2)
            .saturating_add(dir_payload_bytes(&self.root.join("outbox")));
        let receipts = fs::read_dir(self.root.join("receipts"))
            .map(|entries| {
                entries.flatten().fold(0_u64, |bytes, entry| {
                    bytes.saturating_add(
                        entry
                            .metadata()
                            .map_or(0, |metadata| metadata.len())
                            .max(RECEIPT_METADATA_BYTES),
                    )
                })
            })
            .unwrap_or(0);
        let periods = [self.root.join("open"), self.root.join("outbox")]
            .iter()
            .flat_map(|root| list_dirs(root).unwrap_or_default())
            .fold(0_u64, |bytes, directory| {
                let metadata =
                    dir_file_bytes(&directory).saturating_sub(dir_payload_bytes(&directory));
                bytes.saturating_add(metadata.max(PERIOD_METADATA_BYTES + TERMINAL_METADATA_BYTES))
            });
        payload.saturating_add(receipts).saturating_add(periods)
    }

    fn refresh_full(&mut self) {
        if self.spool_bytes() < self.policy.spool_bytes {
            self.full = false;
        }
    }

    fn write_bytes_atomic(&mut self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        #[cfg(test)]
        if let Some(remaining) = self.stop_after_durable_writes.as_mut() {
            if *remaining == 0 {
                self.stop_after_durable_writes = None;
                return Err(io::Error::other("injected durable write interruption"));
            }
        }
        let result = write_bytes_atomic(path, bytes);
        #[cfg(test)]
        if result.is_ok() {
            if let Some(remaining) = self.stop_after_durable_writes.as_mut() {
                *remaining -= 1;
                if *remaining == 0 {
                    self.stop_after_durable_writes = None;
                    return Err(io::Error::other("injected durable write interruption"));
                }
            }
        }
        result
    }

    fn write_json_atomic<T: Serialize>(&mut self, path: &Path, value: &T) -> io::Result<()> {
        let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
        let budget = if path.parent() == Some(self.root.join("receipts").as_path()) {
            RECEIPT_METADATA_BYTES
        } else if path.file_name().is_some_and(|name| name == "terminal.json") {
            TERMINAL_METADATA_BYTES
        } else {
            PERIOD_METADATA_BYTES
        };
        if bytes.len() as u64 > budget {
            return Err(io::Error::other(
                "browser metadata exceeds reserved capacity",
            ));
        }
        self.write_bytes_atomic(path, &bytes)
    }

    fn remove_payload_dir(&mut self, path: &Path) -> io::Result<()> {
        #[cfg(test)]
        if self.fail_next_payload_removes > 0 {
            self.fail_next_payload_removes -= 1;
            return Err(io::Error::other("injected payload remove failure"));
        }
        match fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    #[cfg(test)]
    pub fn stop_after_durable_writes(&mut self, n: usize) {
        self.stop_after_durable_writes = Some(n);
    }

    #[cfg(test)]
    pub fn fail_next_payload_removes(&mut self, n: usize) {
        self.fail_next_payload_removes = n;
    }
}

fn commit(
    store: &mut Store,
    ih: &str,
    batch_id: &str,
    body: &[u8],
    receipt: ReceiptFile,
    now_ms: u64,
) -> io::Result<()> {
    let (period_id, has_dir, seq) = {
        let active = store.active.as_ref().expect("open store");
        (active.open.id.clone(), active.open.has_dir, active.next_seq)
    };
    let dir = store.open_dir(&period_id);
    if !has_dir {
        fs::create_dir_all(&dir)?;
        let open = &store.active.as_ref().unwrap().open;
        store.write_json_atomic(
            &dir.join("period.json"),
            &OpenPeriodFile {
                period_id: period_id.clone(),
                start_ms: open.start_ms,
                end_ms: open.end_ms,
            },
        )?;
        store.active.as_mut().unwrap().open.has_dir = true;
    }
    store.write_bytes_atomic(&dir.join(format!("{seq:012}-{ih}-{batch_id}.jsonl")), body)?;
    {
        let active = store.active.as_mut().unwrap();
        active.next_seq += 1;
        active.open.bytes += body.len() as u64;
    }
    let receipt_path = store.receipt_path(ih, batch_id);
    store.write_json_atomic(&receipt_path, &receipt)?;
    store
        .active
        .as_mut()
        .unwrap()
        .receipts
        .insert((ih.to_string(), batch_id.to_string()), receipt);
    let _ = now_ms;
    Ok(())
}

fn inst_hash(inst: &str) -> String {
    hex(&Sha256::digest(inst.as_bytes()))[..16].to_string()
}

/// `(file name, inst hash, batch id)` for each committed batch in `dir`.
fn batch_files(dir: &Path) -> io::Result<Vec<(String, String, String)>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".jsonl") else {
            continue;
        };
        let mut parts = stem.splitn(3, '-');
        if let (Some(_seq), Some(ih), Some(batch)) = (parts.next(), parts.next(), parts.next()) {
            out.push((name.to_string(), ih.to_string(), batch.to_string()));
        }
    }
    Ok(out)
}

fn list_dirs(path: &Path) -> io::Result<Vec<PathBuf>> {
    Ok(fs::read_dir(path)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect())
}

fn dir_payload_bytes(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| {
            let p = e.path();
            if p.is_dir() {
                dir_payload_bytes(&p)
            } else if p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n == crate::PAGES_FILE || n.ends_with(".jsonl"))
            {
                e.metadata().map(|m| m.len()).unwrap_or(0)
            } else {
                0
            }
        })
        .sum()
}

fn dir_file_bytes(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| e.metadata().map(|m| m.len()).unwrap_or(0))
        .sum()
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> io::Result<Option<T>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Write, fsync, then rename into place: the file is whole or absent.
fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("partial");
    let result = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const T0: u64 = 1_790_000_100_000; // inside a 5-minute window

    fn namer() -> Namer {
        Box::new(|start, len| (format!("d{start}"), format!("s{start}_{len}")))
    }

    fn open(dir: &Path, now: u64) -> Store {
        Store::open(dir, Policy::default(), namer(), now)
    }

    fn snapshot(ctx: &str, text: &str) -> Value {
        json!({"t": "segment_start", "ts": 1, "ctx": ctx, "site": "example.com", "blocks": [{"id": "b1", "text": text}]})
    }

    fn delta(ctx: &str, text: &str) -> Value {
        json!({"t": "delta", "ts": 2, "ctx": ctx, "site": "example.com", "op": "add", "block": {"id": "b2", "text": text}})
    }

    fn offer(store: &mut Store, batch_id: &str, records: &[Value], now: u64) -> BatchResult {
        let generation = "connection-generation".to_string();
        store.offer(
            &BatchInput {
                generation: &generation,
                inst: "inst-1",
                batch_id,
                queued_at_ms: now,
                records,
            },
            now,
        )
    }

    fn id(n: u32) -> String {
        format!("{n:032x}")
    }

    fn all_text(root: &Path) -> String {
        let mut out = String::new();
        fn walk(p: &Path, out: &mut String) {
            for e in fs::read_dir(p).unwrap().flatten() {
                let path = e.path();
                if path.is_dir() {
                    walk(&path, out);
                } else {
                    out.push_str(&String::from_utf8_lossy(&fs::read(&path).unwrap()));
                }
            }
        }
        walk(root, &mut out);
        out
    }

    #[test]
    fn intake_bound_includes_first_period_metadata() {
        fn all_file_bytes(root: &Path) -> u64 {
            fs::read_dir(root)
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    if entry.path().is_dir() {
                        all_file_bytes(&entry.path())
                    } else {
                        entry.metadata().unwrap().len()
                    }
                })
                .sum()
        }
        let dir = tempfile::tempdir().unwrap();
        let mut store = open(dir.path(), T0);
        let record = snapshot("c1", "small");
        let mut body = String::new();
        canonical_stringify(&record, &mut body).unwrap();
        body.push('\n');
        let receipt = ReceiptFile {
            period_id: store.period_id().unwrap().to_owned(),
            at_ms: T0,
            state: ReceiptState::Pending,
        };
        store.policy.spool_bytes =
            body.len() as u64 + serde_json::to_vec(&receipt).unwrap().len() as u64 + 1;
        let _ = offer(&mut store, &id(201), &[record], T0);
        assert!(all_file_bytes(dir.path()) <= store.policy.spool_bytes);
    }

    #[test]
    fn intake_reserves_the_finalized_copy_before_acceptance() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open(dir.path(), T0);
        let record = snapshot("c1", &"x".repeat(1024));
        let mut body = String::new();
        canonical_stringify(&record, &mut body).unwrap();
        body.push('\n');
        let metadata = PERIOD_METADATA_BYTES + TERMINAL_METADATA_BYTES + RECEIPT_METADATA_BYTES;
        store.policy.spool_bytes = metadata + 2 * body.len() as u64 - 1;
        assert!(matches!(
            offer(&mut store, &id(202), std::slice::from_ref(&record), T0),
            BatchResult::Rejected {
                reason: "queue_full"
            }
        ));
        store.policy.spool_bytes += 1;
        assert!(matches!(
            offer(&mut store, &id(202), &[record], T0),
            BatchResult::Accepted { .. }
        ));
        assert_eq!(store.spool_bytes(), store.policy.spool_bytes);
        assert!(store.finalize_now(T0 + 1));
        assert!(store.spool_bytes() <= store.policy.spool_bytes);
    }

    #[test]
    fn accepted_batches_dedup_with_their_original_period() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = open(dir.path(), T0);
        let first = offer(&mut s, &id(1), &[snapshot("c1", "hello")], T0);
        let BatchResult::Accepted { period_id } = first else {
            panic!("{first:?}")
        };
        assert_eq!(
            offer(&mut s, &id(1), &[snapshot("c1", "hello")], T0 + 1000),
            BatchResult::Duplicate { period_id }
        );
    }

    #[test]
    fn a_delta_needs_its_context_snapshot_in_the_same_period() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = open(dir.path(), T0);
        assert_eq!(
            offer(&mut s, &id(1), &[delta("c1", "x")], T0),
            BatchResult::Rejected {
                reason: "snapshot_required"
            }
        );
        assert!(matches!(
            offer(&mut s, &id(2), &[snapshot("c1", "a")], T0),
            BatchResult::Accepted { .. }
        ));
        assert!(matches!(
            offer(&mut s, &id(3), &[delta("c1", "b")], T0),
            BatchResult::Accepted { .. }
        ));
        // A new period needs a new snapshot.
        assert!(s.tick(T0 + 300_000));
        assert_eq!(
            offer(&mut s, &id(4), &[delta("c1", "c")], T0 + 300_001),
            BatchResult::Rejected {
                reason: "snapshot_required"
            }
        );
    }

    #[test]
    fn old_batches_are_admitted_but_future_skew_is_retryable() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = open(dir.path(), T0);
        let old = s.offer(
            &BatchInput {
                generation: "old-generation",
                inst: "i",
                batch_id: &id(1),
                queued_at_ms: T0 - 600_001,
                records: &[snapshot("c", "old")],
            },
            T0,
        );
        assert!(matches!(old, BatchResult::Accepted { .. }));
        let future = s.offer(
            &BatchInput {
                generation: "other-generation",
                inst: "i",
                batch_id: &id(2),
                queued_at_ms: T0 + 61_000,
                records: &[snapshot("c", "future")],
            },
            T0,
        );
        assert_eq!(
            future,
            BatchResult::Rejected {
                reason: "age_policy"
            }
        );
        assert_eq!(BatchResult::class("age_policy"), "retryable");
        assert_eq!(BatchResult::class("stale_generation"), "retryable");
        assert_eq!(BatchResult::class("expired_unaccepted"), "retryable");
    }

    #[test]
    fn a_full_spool_refuses_with_queue_full_until_delivery_frees_room() {
        let dir = tempfile::tempdir().unwrap();
        let policy = Policy {
            spool_bytes: 2000,
            ..Policy::default()
        };
        let mut s = Store::open(dir.path(), policy, namer(), T0);
        let big = "x".repeat(150);
        assert!(matches!(
            offer(&mut s, &id(1), &[snapshot("c1", &big)], T0),
            BatchResult::Accepted { .. }
        ));
        assert_eq!(
            offer(&mut s, &id(2), &[snapshot("c2", &big)], T0),
            BatchResult::Rejected {
                reason: "queue_full"
            }
        );
        assert!(s.status(T0).full);
        s.tick(T0 + 300_000);
        let entry = s.outbox().pop().unwrap();
        s.delivered(&entry).unwrap();
        let st = s.status(T0 + 300_000);
        assert!(!st.full);
        assert_eq!(st.held_bytes, 0);
    }

    #[test]
    fn the_clock_finalizes_one_jsonl_per_period_named_from_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = open(dir.path(), T0);
        offer(&mut s, &id(1), &[snapshot("c1", "alpha")], T0);
        offer(&mut s, &id(2), &[delta("c1", "beta")], T0 + 5);
        let before = s.period_id().unwrap().to_string();
        assert!(!s.tick(T0 + 10));
        assert!(s.tick(T0 + 300_000));
        assert_ne!(s.period_id().unwrap(), before);
        let out = s.outbox();
        assert_eq!(out.len(), 1);
        let window = (T0 - T0 % 300_000) / 1000;
        assert_eq!(out[0].day, format!("d{window}"));
        assert_eq!(out[0].segment, format!("s{window}_300"));
        let body = fs::read_to_string(out[0].pages_path()).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("alpha") && lines[1].contains("beta"));
        assert_eq!(out[0].size, body.len() as u64);
    }

    #[test]
    fn the_file_cap_rotates_the_period_early() {
        let dir = tempfile::tempdir().unwrap();
        let policy = Policy {
            file_max: 200,
            ..Policy::default()
        };
        let mut s = Store::open(dir.path(), policy, namer(), T0);
        let text = "y".repeat(100);
        let BatchResult::Accepted { period_id: p1 } =
            offer(&mut s, &id(1), &[snapshot("c1", &text)], T0)
        else {
            panic!()
        };
        let BatchResult::Accepted { period_id: p2 } =
            offer(&mut s, &id(2), &[snapshot("c2", &text)], T0 + 2000)
        else {
            panic!()
        };
        assert_ne!(p1, p2);
        assert_eq!(s.outbox().len(), 1);
    }

    #[test]
    fn a_restart_finalizes_the_open_period_and_keeps_its_receipts() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = {
            let mut s = open(dir.path(), T0);
            let BatchResult::Accepted { period_id } =
                offer(&mut s, &id(1), &[snapshot("c1", "kept")], T0)
            else {
                panic!()
            };
            period_id
        };
        let mut s = open(dir.path(), T0 + 1000);
        assert_eq!(s.outbox().len(), 1);
        assert_eq!(
            offer(&mut s, &id(1), &[snapshot("c1", "kept")], T0 + 2000),
            BatchResult::Duplicate { period_id: p1 }
        );
    }

    #[test]
    fn a_lost_receipt_is_repaired_from_the_batch_file() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut s = open(dir.path(), T0);
            offer(&mut s, &id(7), &[snapshot("c1", "kept")], T0);
        }
        let receipts = dir.path().join("receipts");
        for e in fs::read_dir(&receipts).unwrap() {
            fs::remove_file(e.unwrap().path()).unwrap();
        }
        let mut s = open(dir.path(), T0 + 1000);
        assert!(matches!(
            offer(&mut s, &id(7), &[snapshot("c1", "kept")], T0 + 1000),
            BatchResult::Duplicate { .. }
        ));
    }

    #[test]
    fn open_removes_legacy_layout_and_does_not_count_its_bytes() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("gen/g1")).unwrap();
        fs::write(dir.path().join("gen/g1/pages"), b"legacy marker").unwrap();
        fs::create_dir_all(dir.path().join("retired/g0")).unwrap();
        fs::write(dir.path().join("retired/g0/pages"), b"retired marker").unwrap();
        fs::write(
            dir.path().join("active.json"),
            b"not json and must not be read",
        )
        .unwrap();
        let s = open(dir.path(), T0);
        assert!(!dir.path().join("gen").exists());
        assert!(!dir.path().join("retired").exists());
        assert!(!dir.path().join("active.json").exists());
        assert_eq!(s.spool_bytes(), 0);
        assert!(!all_text(dir.path()).contains("legacy marker"));
    }

    #[test]
    fn discarded_and_delivered_replays_stay_duplicates_after_restart() {
        for terminal in [ReceiptState::Discarded, ReceiptState::Delivered] {
            let dir = tempfile::tempdir().unwrap();
            {
                let mut s = open(dir.path(), T0);
                offer(&mut s, &id(11), &[snapshot("c", "terminal")], T0);
                s.tick(T0 + 300_000);
                let entry = s.outbox().pop().unwrap();
                if terminal == ReceiptState::Discarded {
                    assert_eq!(s.discard_waiting(T0 + 300_000), 0);
                } else {
                    s.delivered(&entry).unwrap();
                }
            }
            let mut s = open(dir.path(), T0 + 3_700_000);
            let replay = s.offer(
                &BatchInput {
                    generation: "a different generation",
                    inst: "inst-1",
                    batch_id: &id(11),
                    queued_at_ms: T0 + 3_700_000,
                    records: &[snapshot("c", "new payload")],
                },
                T0 + 3_700_000,
            );
            assert!(matches!(replay, BatchResult::Duplicate { .. }));
            assert!(s.outbox().is_empty());
            assert!(!all_text(dir.path()).contains("new payload"));
        }
    }

    #[test]
    fn discard_and_delivery_tombstones_finish_after_restart() {
        for delivered in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            {
                let mut s = open(dir.path(), T0);
                offer(&mut s, &id(12), &[snapshot("c", "interrupt marker")], T0);
                s.tick(T0 + 300_000);
                let entry = s.outbox().pop().unwrap();
                s.stop_after_durable_writes(1);
                if delivered {
                    assert!(s.delivered(&entry).is_err());
                } else {
                    assert_eq!(s.discard_waiting(T0 + 300_000), 1);
                }
            }
            let s = open(dir.path(), T0 + 300_001);
            assert!(s.outbox().is_empty());
            assert!(!all_text(dir.path()).contains("interrupt marker"));
            let receipt: ReceiptFile = read_json(
                &fs::read_dir(dir.path().join("receipts"))
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .path(),
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                receipt.state,
                if delivered {
                    ReceiptState::Delivered
                } else {
                    ReceiptState::Discarded
                }
            );
        }
    }

    #[test]
    fn terminal_marker_recovery_removes_a_leftover_open_copy() {
        for delivered in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let batch_id = id(34);
            let receipt_path = dir
                .path()
                .join("receipts")
                .join(format!("{}-{batch_id}.json", inst_hash("inst-1")));
            {
                let mut s = open(dir.path(), T0);
                let BatchResult::Accepted { period_id } =
                    offer(&mut s, &batch_id, &[snapshot("c", "leftover payload")], T0)
                else {
                    panic!()
                };
                let open_dir = s.open_dir(&period_id);
                s.fail_next_payload_removes(1);
                assert!(s.tick(T0 + 300_000));
                let entry = s.outbox().pop().unwrap();
                assert!(open_dir.is_dir());
                assert!(entry.dir.is_dir());
                assert_eq!(
                    read_json::<ReceiptFile>(&receipt_path)
                        .unwrap()
                        .unwrap()
                        .state,
                    ReceiptState::Pending
                );

                s.stop_after_durable_writes(1);
                if delivered {
                    assert!(s.delivered(&entry).is_err());
                } else {
                    assert_eq!(s.discard_waiting(T0 + 300_000), 1);
                }
                assert!(entry.dir.join("terminal.json").is_file());
                assert!(open_dir.is_dir());
                assert_eq!(
                    read_json::<ReceiptFile>(&receipt_path)
                        .unwrap()
                        .unwrap()
                        .state,
                    ReceiptState::Pending
                );
            }

            let mut s = open(dir.path(), T0 + 300_001);
            assert!(s.outbox().is_empty());
            assert!(!all_text(&dir.path().join("open")).contains("leftover payload"));
            assert!(!all_text(&dir.path().join("outbox")).contains("leftover payload"));
            let receipt = read_json::<ReceiptFile>(&receipt_path).unwrap().unwrap();
            assert_eq!(
                receipt.state,
                if delivered {
                    ReceiptState::Delivered
                } else {
                    ReceiptState::Discarded
                }
            );

            let bytes_before_replay = s.spool_bytes();
            assert_eq!(
                s.offer(
                    &BatchInput {
                        generation: "a different generation",
                        inst: "inst-1",
                        batch_id: &batch_id,
                        queued_at_ms: T0 + 3_700_000,
                        records: &[snapshot("c", "replacement payload")],
                    },
                    T0 + 3_700_000,
                ),
                BatchResult::Duplicate {
                    period_id: receipt.period_id
                }
            );
            assert_eq!(s.spool_bytes(), bytes_before_replay);
            assert!(s.outbox().is_empty());
            assert!(!all_text(&dir.path().join("open")).contains("replacement payload"));
            assert!(!all_text(&dir.path().join("outbox")).contains("replacement payload"));
        }
    }

    #[test]
    fn period_metadata_without_a_batch_is_not_offered() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = open(dir.path(), T0);
        let id = s.period_id().unwrap().to_string();
        let dirpath = s.open_dir(&id);
        fs::create_dir_all(&dirpath).unwrap();
        s.write_json_atomic(
            &dirpath.join("period.json"),
            &OpenPeriodFile {
                period_id: id,
                start_ms: T0,
                end_ms: T0 + 300_000,
            },
        )
        .unwrap();
        drop(s);
        s = open(dir.path(), T0 + 1);
        assert!(s.outbox().is_empty());
        assert!(!s.status(T0 + 1).waiting);
    }

    #[test]
    fn discard_removes_finalized_and_open_payload_and_clears_context() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = open(dir.path(), T0);
        offer(&mut s, &id(13), &[snapshot("c", "finalized")], T0);
        s.tick(T0 + 300_000);
        let open_id = s.period_id().unwrap().to_string();
        offer(
            &mut s,
            &id(14),
            &[snapshot("open", "open payload")],
            T0 + 300_000,
        );
        assert_eq!(s.discard_waiting(T0 + 300_001), 0);
        assert_eq!(s.period_id(), Some(open_id.as_str()));
        assert!(!s.status(T0 + 300_001).waiting);
        assert_eq!(
            offer(
                &mut s,
                &id(15),
                &[delta("open", "needs snapshot")],
                T0 + 300_002
            ),
            BatchResult::Rejected {
                reason: "snapshot_required"
            }
        );
        assert!(matches!(
            offer(&mut s, &id(16), &[snapshot("open", "fresh")], T0 + 300_002),
            BatchResult::Accepted { .. }
        ));
    }

    #[test]
    fn discarded_open_period_can_finalize_new_snapshot_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let stable_period = {
            let mut s = open(dir.path(), T0);
            let BatchResult::Accepted { period_id } =
                offer(&mut s, &id(31), &[snapshot("c", "discarded text")], T0)
            else {
                panic!()
            };
            assert_eq!(s.discard_waiting(T0), 0);
            assert!(!s.status(T0).waiting);
            assert_eq!(s.period_id(), Some(period_id.as_str()));

            assert_eq!(
                offer(&mut s, &id(32), &[delta("c", "dependent delta")], T0 + 1),
                BatchResult::Rejected {
                    reason: "snapshot_required"
                }
            );
            assert!(matches!(
                offer(&mut s, &id(33), &[snapshot("c", "fresh text")], T0 + 2),
                BatchResult::Accepted { .. }
            ));
            assert!(all_text(&s.open_dir(&period_id)).contains("fresh text"));
            assert!(!all_text(&s.open_dir(&period_id)).contains("discarded text"));

            assert!(s.tick(T0 + 300_000));
            let outbox = s.outbox();
            assert_eq!(outbox.len(), 1);
            let body = fs::read_to_string(outbox[0].pages_path()).unwrap();
            assert!(body.contains("fresh text"));
            assert!(!body.contains("discarded text"));
            period_id
        };

        let mut s = open(dir.path(), T0 + 300_001);
        let outbox = s.outbox();
        assert_eq!(outbox.len(), 1);
        let body = fs::read_to_string(outbox[0].pages_path()).unwrap();
        assert!(body.contains("fresh text"));
        assert!(!body.contains("discarded text"));
        let bytes_before_replay = s.spool_bytes();
        assert_eq!(
            s.offer(
                &BatchInput {
                    generation: "a different generation",
                    inst: "inst-1",
                    batch_id: &id(31),
                    queued_at_ms: T0 + 3_700_000,
                    records: &[snapshot("c", "replacement text")],
                },
                T0 + 3_700_000,
            ),
            BatchResult::Duplicate {
                period_id: stable_period
            }
        );
        assert_eq!(s.spool_bytes(), bytes_before_replay);
        let body = fs::read_to_string(s.outbox()[0].pages_path()).unwrap();
        assert!(body.contains("fresh text"));
        assert!(!body.contains("discarded text"));
        assert!(!body.contains("replacement text"));
    }

    #[test]
    fn discard_skips_reserved_period_until_delivery_releases_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = open(dir.path(), T0);
        offer(&mut s, &id(21), &[snapshot("one", "reserved")], T0);
        s.tick(T0 + 300_000);
        offer(&mut s, &id(22), &[snapshot("two", "waiting")], T0 + 300_000);
        s.tick(T0 + 600_000);
        let mut entries = s.outbox();
        assert_eq!(entries.len(), 2);
        let reserved = entries.remove(0);
        assert!(s.reserve(&reserved));
        assert_eq!(s.discard_waiting(T0 + 600_001), 0);
        assert_eq!(s.outbox().len(), 1);
        assert!(!s.status(T0 + 600_001).waiting);
        s.delivered(&reserved).unwrap();
        assert!(s.outbox().is_empty());
        assert_eq!(s.status(T0 + 600_001).held_bytes, 0);
    }

    #[test]
    fn reserved_payload_is_not_waiting_and_failed_delete_remains_waiting() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = open(dir.path(), T0);
        offer(&mut s, &id(17), &[snapshot("c", "reserved")], T0);
        s.tick(T0 + 300_000);
        let entry = s.outbox().pop().unwrap();
        assert!(s.reserve(&entry));
        assert!(!s.status(T0 + 300_000).waiting);
        assert_eq!(s.discard_waiting(T0 + 300_001), 0);
        s.release(&entry);
        s.fail_next_payload_removes(1);
        assert_eq!(s.discard_waiting(T0 + 300_002), 1);
        assert!(s.status(T0 + 300_002).waiting);
        assert!(entry.pages_path().exists());
        assert_eq!(s.discard_waiting(T0 + 300_003), 0);
        assert!(!s.status(T0 + 300_003).waiting);
    }

    #[test]
    fn pending_receipts_survive_longer_than_the_old_retention() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = open(dir.path(), T0);
        let first = offer(&mut s, &id(18), &[snapshot("c", "kept")], T0);
        let BatchResult::Accepted { period_id } = first else {
            panic!()
        };
        drop(s);
        let mut s = open(dir.path(), T0 + 3_700_000);
        assert_eq!(
            offer(&mut s, &id(18), &[snapshot("c", "kept")], T0 + 3_700_000),
            BatchResult::Duplicate { period_id }
        );
    }

    #[test]
    fn capacity_counts_receipts_and_duplicates_bypass_queue_full() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store::open(
            dir.path(),
            Policy {
                spool_bytes: 1800,
                ..Policy::default()
            },
            namer(),
            T0,
        );
        let content = snapshot("c", &"x".repeat(70));
        assert!(matches!(
            offer(&mut s, &id(19), std::slice::from_ref(&content), T0),
            BatchResult::Accepted { .. }
        ));
        let known = offer(&mut s, &id(19), std::slice::from_ref(&content), T0 + 1);
        assert!(matches!(known, BatchResult::Duplicate { .. }));
        let mut n = 20;
        while !s.status(T0).full && n < 40 {
            if matches!(
                offer(
                    &mut s,
                    &id(n),
                    &[snapshot("c", &"x".repeat(70))],
                    T0 + n as u64
                ),
                BatchResult::Rejected {
                    reason: "queue_full"
                }
            ) {
                break;
            }
            n += 1;
        }
        assert!(s.status(T0).full);
        assert!(matches!(
            offer(&mut s, &id(19), &[snapshot("c", "replay")], T0),
            BatchResult::Duplicate { .. }
        ));
        assert!(s.status(T0).full);
    }
}
