// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Durable acceptance and custody of browser text on this PC.
//!
//! One bounded on-disk spool, one custody generation per strict journal
//! identity ([`crate::identity`]), and periods the app owns:
//!
//! ```text
//! <root>/active.json                         {"generation","identity"}
//! <root>/gen/<g>/identity                    the identity this generation is for
//! <root>/gen/<g>/open/<period>/period.json   the open period's window
//! <root>/gen/<g>/open/<period>/<seq>-<ih>-<batch_id>.jsonl   one accepted batch
//! <root>/gen/<g>/receipts/<ih>-<batch_id>.json               its dedup receipt
//! <root>/gen/<g>/outbox/<start>-<period>/{browser_pages.jsonl,period.json}
//! <root>/retired/<g>/...                     a previous journal's generation
//! ```
//!
//! A batch is accepted only after its records file is written, fsynced and
//! renamed into place; its receipt follows, so a replayed `batch_id` answers
//! `duplicate` with the original period. A crash between the two is repaired on
//! open from the batch file's name. At-least-once from the extension, dedup
//! here, and an idempotent journal upload make delivery at-least-once end to
//! end without duplicates in the journal.
//!
//! A journal switch renames the whole generation directory into `retired/`.
//! Retired text is held, counted for the owner to see, never delivered, never
//! counted against the spool, and removed only when the owner discards it.
//!
//! Deliberately not here (MVP): clock-rollback handling, legacy migration and
//! page-level accounting.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use native_browser_frame::{
    canonical_stringify, ACCEPTED_RETENTION_MS_MIN, FILE_MAX, FUTURE_SKEW_MS_MAX,
    OUTBOX_AGE_MS_MAX, SPOOL_AGE_MS_MAX, SPOOL_BYTES_MAX,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::identity::hex;

/// The bounds the store enforces. Defaults are the contract's policy.
#[derive(Debug, Clone)]
pub struct Policy {
    pub spool_bytes: u64,
    pub file_max: u64,
    pub spool_age_ms: u64,
    pub outbox_age_ms: u64,
    pub future_skew_ms: u64,
    pub accepted_retention_ms: u64,
    /// The period grid, aligned with the capture segments.
    pub period_ms: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            spool_bytes: SPOOL_BYTES_MAX as u64,
            file_max: FILE_MAX as u64,
            spool_age_ms: SPOOL_AGE_MS_MAX,
            outbox_age_ms: OUTBOX_AGE_MS_MAX,
            future_skew_ms: FUTURE_SKEW_MS_MAX,
            accepted_retention_ms: ACCEPTED_RETENTION_MS_MIN,
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
    pub generation: String,
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

/// What the owner can see about a previous journal's text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RetiredSummary {
    pub generations: usize,
    pub bytes: u64,
}

/// The store's status for the `state` message and the health dump.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CustodyStatus {
    pub generation: Option<String>,
    pub period_id: Option<String>,
    pub held_bytes: u64,
    pub held_periods: usize,
    pub full: bool,
    pub stale: bool,
    pub failed: bool,
    pub retired: RetiredSummary,
}

#[derive(Serialize, Deserialize)]
struct ActiveFile {
    generation: String,
    identity: String,
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

#[derive(Serialize, Deserialize)]
struct ReceiptFile {
    period_id: String,
    at_ms: u64,
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
    generation: String,
    identity: String,
    open: OpenPeriod,
    receipts: HashMap<(String, String), (String, u64)>,
    held_bytes: u64,
    held_periods: usize,
    oldest_held_ms: Option<u64>,
    next_seq: u64,
}

pub struct Store {
    root: PathBuf,
    policy: Policy,
    namer: Namer,
    active: Option<Active>,
    failed: bool,
    full: bool,
    retired: RetiredSummary,
    counter: u64,
}

impl Store {
    /// Open (creating) the store at `root`, recovering any period left open by
    /// a previous run: it is finalized as it stands.
    pub fn open(root: impl Into<PathBuf>, policy: Policy, namer: Namer, now_ms: u64) -> Self {
        let mut store = Store {
            root: root.into(),
            policy,
            namer,
            active: None,
            failed: false,
            full: false,
            retired: RetiredSummary::default(),
            counter: 0,
        };
        if let Err(error) = store.load(now_ms) {
            tracing::warn!(target: "browser", component = "custody", outcome = "open_failed", error = %error, "custody open");
            store.failed = true;
            store.active = None;
        }
        store.refresh_retired();
        store
    }

    fn load(&mut self, now_ms: u64) -> io::Result<()> {
        fs::create_dir_all(self.root.join("gen"))?;
        fs::create_dir_all(self.root.join("retired"))?;
        let active_path = self.root.join("active.json");
        let Some(active) = read_json::<ActiveFile>(&active_path)? else {
            return Ok(());
        };
        let gen_dir = self.gen_dir(&active.generation);
        if !gen_dir.is_dir() {
            // Retired (or removed) after active.json was written.
            return Ok(());
        }
        let on_disk_identity = fs::read_to_string(gen_dir.join("identity"))?;
        if on_disk_identity != active.identity {
            return Err(io::Error::other("generation identity mismatch"));
        }
        let mut a = Active {
            generation: active.generation.clone(),
            identity: active.identity,
            open: self.new_open(now_ms, false),
            receipts: HashMap::new(),
            held_bytes: 0,
            held_periods: 0,
            oldest_held_ms: None,
            next_seq: 0,
        };
        fs::create_dir_all(gen_dir.join("open"))?;
        fs::create_dir_all(gen_dir.join("receipts"))?;
        fs::create_dir_all(gen_dir.join("outbox"))?;

        // Receipts, then any batch whose receipt the crash lost.
        for entry in fs::read_dir(gen_dir.join("receipts"))? {
            let path = entry?.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if let Some(stem) = name.strip_suffix(".json") {
                if let (Some((ih, batch)), Some(r)) =
                    (stem.split_once('-'), read_json::<ReceiptFile>(&path)?)
                {
                    a.receipts
                        .insert((ih.to_string(), batch.to_string()), (r.period_id, r.at_ms));
                }
            } else {
                let _ = fs::remove_file(&path);
            }
        }

        // Finalize every period a previous run left open.
        let mut open_dirs: Vec<PathBuf> = fs::read_dir(gen_dir.join("open"))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir())
            .collect();
        open_dirs.sort();
        for dir in open_dirs {
            let Some(meta) = read_json::<OpenPeriodFile>(&dir.join("period.json"))? else {
                fs::remove_dir_all(&dir)?;
                continue;
            };
            for (seq_name, ih, batch) in batch_files(&dir)? {
                let _ = seq_name;
                a.receipts
                    .entry((ih, batch))
                    .or_insert_with(|| (meta.period_id.clone(), now_ms));
            }
            let period = OpenPeriod {
                id: meta.period_id,
                start_ms: meta.start_ms,
                end_ms: meta.end_ms,
                bytes: 0,
                contexts: HashSet::new(),
                has_dir: true,
            };
            self.finalize_dir(&a.generation, &period, &dir, meta.end_ms.min(now_ms))?;
        }
        for ((ih, batch), (period, at)) in &a.receipts {
            let path = gen_dir.join("receipts").join(format!("{ih}-{batch}.json"));
            if !path.exists() {
                write_json_atomic(
                    &path,
                    &ReceiptFile {
                        period_id: period.clone(),
                        at_ms: *at,
                    },
                )?;
            }
        }

        // Outbox: drop half-built staging directories; count what is held.
        for entry in fs::read_dir(gen_dir.join("outbox"))? {
            let path = entry?.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with(".tmp-") {
                fs::remove_dir_all(&path)?;
                continue;
            }
            if let Some(meta) = read_json::<OutboxPeriodFile>(&path.join("period.json"))? {
                a.held_bytes += meta.size;
                a.held_periods += 1;
                a.oldest_held_ms = Some(
                    a.oldest_held_ms
                        .map_or(meta.start_ms, |o| o.min(meta.start_ms)),
                );
            }
        }
        self.active = Some(a);
        Ok(())
    }

    fn gen_dir(&self, generation: &str) -> PathBuf {
        self.root.join("gen").join(generation)
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

    /// Bind custody to the paired journal. A different identity retires the
    /// current generation first; the same identity keeps it. Returns whether the
    /// generation changed.
    pub fn ensure_generation(&mut self, identity: &str, now_ms: u64) -> bool {
        if self.failed {
            return false;
        }
        if self.active.as_ref().is_some_and(|a| a.identity == identity) {
            return false;
        }
        if let Err(error) = self.try_switch(identity, now_ms) {
            tracing::warn!(target: "browser", component = "custody", outcome = "switch_failed", error = %error, "custody generation");
            self.failed = true;
            self.active = None;
        }
        self.refresh_retired();
        true
    }

    fn try_switch(&mut self, identity: &str, now_ms: u64) -> io::Result<()> {
        if let Some(old) = self.active.take() {
            let from = self.gen_dir(&old.generation);
            let to = self.root.join("retired").join(&old.generation);
            fs::rename(&from, &to)?;
            tracing::info!(target: "browser", component = "custody", outcome = "retired", held_bytes = old.held_bytes, "custody generation");
        }
        let token = self.next_token(identity, now_ms);
        let generation = token[..32].to_string();
        let dir = self.gen_dir(&generation);
        fs::create_dir_all(dir.join("open"))?;
        fs::create_dir_all(dir.join("receipts"))?;
        fs::create_dir_all(dir.join("outbox"))?;
        write_bytes_atomic(&dir.join("identity"), identity.as_bytes())?;
        write_json_atomic(
            &self.root.join("active.json"),
            &ActiveFile {
                generation: generation.clone(),
                identity: identity.to_string(),
            },
        )?;
        let open = self.new_open(now_ms, false);
        self.active = Some(Active {
            generation,
            identity: identity.to_string(),
            open,
            receipts: HashMap::new(),
            held_bytes: 0,
            held_periods: 0,
            oldest_held_ms: None,
            next_seq: 0,
        });
        self.full = false;
        Ok(())
    }

    pub fn generation(&self) -> Option<&str> {
        self.active.as_ref().map(|a| a.generation.as_str())
    }

    pub fn identity(&self) -> Option<&str> {
        self.active.as_ref().map(|a| a.identity.as_str())
    }

    pub fn period_id(&self) -> Option<&str> {
        self.active.as_ref().map(|a| a.open.id.as_str())
    }

    pub fn status(&self, now_ms: u64) -> CustodyStatus {
        let (generation, period_id, held_bytes, held_periods, stale) = match &self.active {
            Some(a) => {
                let open_bytes = if a.open.has_dir { a.open.bytes } else { 0 };
                let oldest = match (a.oldest_held_ms, a.open.has_dir) {
                    (Some(o), true) => Some(o.min(a.open.start_ms)),
                    (Some(o), false) => Some(o),
                    (None, true) => Some(a.open.start_ms),
                    (None, false) => None,
                };
                (
                    Some(a.generation.clone()),
                    Some(a.open.id.clone()),
                    a.held_bytes + open_bytes,
                    a.held_periods + usize::from(a.open.has_dir),
                    oldest.is_some_and(|o| now_ms.saturating_sub(o) >= self.policy.spool_age_ms),
                )
            }
            None => (None, None, 0, 0, false),
        };
        CustodyStatus {
            generation,
            period_id,
            held_bytes,
            held_periods,
            full: self.full,
            stale,
            failed: self.failed,
            retired: self.retired.clone(),
        }
    }

    /// Offer one decoded batch. Only `Accepted`/`Duplicate` mean the records are
    /// durably held under the active generation.
    pub fn offer(&mut self, batch: &BatchInput<'_>, now_ms: u64) -> BatchResult {
        if self.failed {
            return BatchResult::Rejected {
                reason: "resource_exhausted",
            };
        }
        let ih = inst_hash(batch.inst);
        let Some(active) = self.active.as_ref() else {
            return BatchResult::Rejected {
                reason: "stale_generation",
            };
        };
        if let Some((period, _)) = active
            .receipts
            .get(&(ih.clone(), batch.batch_id.to_string()))
        {
            if batch.generation == active.generation {
                return BatchResult::Duplicate {
                    period_id: period.clone(),
                };
            }
        }
        if batch.generation != active.generation {
            return BatchResult::Rejected {
                reason: "stale_generation",
            };
        }
        if batch.queued_at_ms > now_ms.saturating_add(self.policy.future_skew_ms) {
            return BatchResult::Rejected {
                reason: "age_policy",
            };
        }
        if now_ms.saturating_sub(batch.queued_at_ms) >= self.policy.outbox_age_ms {
            return BatchResult::Rejected {
                reason: "expired_unaccepted",
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
        let bytes = body.len() as u64;
        let held = self.status(now_ms).held_bytes;
        if held.saturating_add(bytes) > self.policy.spool_bytes {
            self.full = true;
            return BatchResult::Rejected {
                reason: "queue_full",
            };
        }

        // Rotate on the clock, then on the per-period file cap.
        if now_ms >= self.active.as_ref().map_or(0, |a| a.open.end_ms) {
            if let Err(e) = self.rotate(now_ms, false) {
                return self.io_rejection(e);
            }
        }
        let open_bytes = self.active.as_ref().map_or(0, |a| a.open.bytes);
        if open_bytes > 0 && open_bytes + bytes > self.policy.file_max {
            if let Err(e) = self.rotate(now_ms, true) {
                return self.io_rejection(e);
            }
        }

        let first = batch.records.first();
        let is_delta = first.and_then(|r| r.get("t")).and_then(Value::as_str) == Some("delta");
        let ctx = first
            .and_then(|r| r.get("ctx"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let active = self.active.as_mut().expect("active checked above");
        if is_delta && !active.open.contexts.contains(&ctx) {
            return BatchResult::Rejected {
                reason: "snapshot_required",
            };
        }

        match commit(
            &self.root,
            active,
            &ih,
            batch.batch_id,
            body.as_bytes(),
            now_ms,
        ) {
            Ok(()) => {
                if !is_delta {
                    active.open.contexts.insert(ctx);
                }
                BatchResult::Accepted {
                    period_id: active.open.id.clone(),
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

    /// Advance the clock: finalize the open period once its window has passed,
    /// and age out receipts. Returns whether the period id changed.
    pub fn tick(&mut self, now_ms: u64) -> bool {
        let Some(active) = self.active.as_mut() else {
            return false;
        };
        let retention = self.policy.accepted_retention_ms;
        let receipts_dir = self
            .root
            .join("gen")
            .join(&active.generation)
            .join("receipts");
        let open_id = active.open.id.clone();
        active.receipts.retain(|(ih, batch), (period, at)| {
            let keep = *period == open_id || now_ms.saturating_sub(*at) < retention;
            if !keep {
                let _ = fs::remove_file(receipts_dir.join(format!("{ih}-{batch}.json")));
            }
            keep
        });
        if now_ms < active.open.end_ms {
            return false;
        }
        if let Err(error) = self.rotate(now_ms, false) {
            tracing::warn!(target: "browser", component = "custody", outcome = "finalize_failed", error = %error, "custody tick");
        }
        true
    }

    /// Finalize the open period now (an update or quit), so what it holds is
    /// ready to deliver at the next start.
    pub fn finalize_now(&mut self, now_ms: u64) -> bool {
        if self.active.as_ref().is_some_and(|a| a.open.has_dir) {
            return self.rotate(now_ms, true).is_ok();
        }
        false
    }

    fn rotate(&mut self, now_ms: u64, early: bool) -> io::Result<()> {
        let next = self.new_open(now_ms, early);
        let active = self
            .active
            .as_mut()
            .expect("rotate needs an active generation");
        let generation = active.generation.clone();
        let old = std::mem::replace(&mut active.open, next);
        if old.has_dir {
            let dir = self
                .root
                .join("gen")
                .join(&generation)
                .join("open")
                .join(&old.id);
            let end = old.end_ms.min(now_ms);
            let size = self.finalize_dir(&generation, &old, &dir, end)?;
            if size == 0 {
                return Ok(());
            }
            let active = self.active.as_mut().expect("still active");
            active.held_bytes += size;
            active.held_periods += 1;
            active.oldest_held_ms = Some(
                active
                    .oldest_held_ms
                    .map_or(old.start_ms, |o| o.min(old.start_ms)),
            );
        }
        Ok(())
    }

    /// Concatenate a period's batches into one outbox entry; returns its size.
    fn finalize_dir(
        &self,
        generation: &str,
        period: &OpenPeriod,
        dir: &Path,
        end_ms: u64,
    ) -> io::Result<u64> {
        let outbox = self.gen_dir(generation).join("outbox");
        let name = format!("{:015}-{}", period.start_ms, period.id);
        let final_dir = outbox.join(&name);
        if final_dir.is_dir() {
            fs::remove_dir_all(dir)?;
            return Ok(0);
        }
        let mut files: Vec<_> = batch_files(dir)?;
        files.sort();
        let staging = outbox.join(format!(".tmp-{name}"));
        if staging.exists() {
            fs::remove_dir_all(&staging)?;
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
            fs::remove_dir_all(&staging)?;
            fs::remove_dir_all(dir)?;
            return Ok(0);
        }
        let start_secs = period.start_ms / 1000;
        let len_secs = (end_ms.saturating_sub(period.start_ms) / 1000).max(1);
        let (day, segment) = (self.namer)(start_secs, len_secs);
        write_json_atomic(
            &staging.join("period.json"),
            &OutboxPeriodFile {
                period_id: period.id.clone(),
                day,
                segment,
                start_ms: period.start_ms,
                len_secs,
                size,
                sha256: hex(&hasher.finalize()),
            },
        )?;
        fs::rename(&staging, &final_dir)?;
        fs::remove_dir_all(dir)?;
        Ok(size)
    }

    /// Finalized periods of the active generation, oldest first.
    pub fn outbox(&self) -> Vec<OutboxEntry> {
        let Some(active) = &self.active else {
            return Vec::new();
        };
        let outbox = self.gen_dir(&active.generation).join("outbox");
        let Ok(entries) = fs::read_dir(&outbox) else {
            return Vec::new();
        };
        let mut out: Vec<OutboxEntry> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| !n.starts_with(".tmp-"))
            })
            .filter_map(|dir| {
                let meta = read_json::<OutboxPeriodFile>(&dir.join("period.json")).ok()??;
                Some(OutboxEntry {
                    dir,
                    generation: active.generation.clone(),
                    period_id: meta.period_id,
                    day: meta.day,
                    segment: meta.segment,
                    start_secs: meta.start_ms / 1000,
                    len_secs: meta.len_secs,
                    size: meta.size,
                    sha256: meta.sha256,
                })
            })
            .collect();
        out.sort_by(|a, b| a.dir.cmp(&b.dir));
        out
    }

    /// The journal holds `entry`: release it. Refuses an entry of another
    /// generation, so a retired period can never be released as delivered.
    pub fn delivered(&mut self, entry: &OutboxEntry) -> io::Result<()> {
        let Some(active) = self.active.as_mut() else {
            return Err(io::Error::other("no active generation"));
        };
        if entry.generation != active.generation {
            return Err(io::Error::other("entry is not of the active generation"));
        }
        fs::remove_dir_all(&entry.dir)?;
        active.held_bytes = active.held_bytes.saturating_sub(entry.size);
        active.held_periods = active.held_periods.saturating_sub(1);
        if active.held_periods == 0 {
            active.oldest_held_ms = None;
        }
        self.full = false;
        Ok(())
    }

    pub fn retired(&self) -> RetiredSummary {
        self.retired.clone()
    }

    fn refresh_retired(&mut self) {
        let mut summary = RetiredSummary::default();
        if let Ok(entries) = fs::read_dir(self.root.join("retired")) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    summary.generations += 1;
                    summary.bytes += dir_size(&entry.path());
                }
            }
        }
        self.retired = summary;
    }

    /// The owner discards every previous journal's text. Returns what is left
    /// (zero on success).
    pub fn discard_retired(&mut self) -> RetiredSummary {
        if let Ok(entries) = fs::read_dir(self.root.join("retired")) {
            for entry in entries.flatten() {
                if let Err(error) = fs::remove_dir_all(entry.path()) {
                    tracing::warn!(target: "browser", component = "custody", outcome = "discard_failed", error = %error, "retired discard");
                }
            }
        }
        self.refresh_retired();
        self.retired.clone()
    }
}

/// Durably write one batch and its receipt.
fn commit(
    root: &Path,
    active: &mut Active,
    ih: &str,
    batch_id: &str,
    body: &[u8],
    now_ms: u64,
) -> io::Result<()> {
    let gen_dir = root.join("gen").join(&active.generation);
    let dir = gen_dir.join("open").join(&active.open.id);
    if !active.open.has_dir {
        fs::create_dir_all(&dir)?;
        write_json_atomic(
            &dir.join("period.json"),
            &OpenPeriodFile {
                period_id: active.open.id.clone(),
                start_ms: active.open.start_ms,
                end_ms: active.open.end_ms,
            },
        )?;
        active.open.has_dir = true;
    }
    let seq = active.next_seq;
    active.next_seq += 1;
    write_bytes_atomic(&dir.join(format!("{seq:012}-{ih}-{batch_id}.jsonl")), body)?;
    active.open.bytes += body.len() as u64;
    active.receipts.insert(
        (ih.to_string(), batch_id.to_string()),
        (active.open.id.clone(), now_ms),
    );
    write_json_atomic(
        &gen_dir
            .join("receipts")
            .join(format!("{ih}-{batch_id}.json")),
        &ReceiptFile {
            period_id: active.open.id.clone(),
            at_ms: now_ms,
        },
    )
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

fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| {
            let p = e.path();
            if p.is_dir() {
                dir_size(&p)
            } else {
                e.metadata().map(|m| m.len()).unwrap_or(0)
            }
        })
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

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    write_bytes_atomic(path, &bytes)
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

    const G_A: &str = "sha256:journal-a";
    const G_B: &str = "sha256:journal-b";
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
        let generation = store.generation().unwrap().to_string();
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
    fn accepted_batches_dedup_with_their_original_period() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = open(dir.path(), T0);
        assert!(s.ensure_generation(G_A, T0));
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
        s.ensure_generation(G_A, T0);
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
    fn stale_generation_and_age_policy() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = open(dir.path(), T0);
        s.ensure_generation(G_A, T0);
        let r = s.offer(
            &BatchInput {
                generation: "other",
                inst: "i",
                batch_id: &id(1),
                queued_at_ms: T0,
                records: &[snapshot("c", "t")],
            },
            T0,
        );
        assert_eq!(
            r,
            BatchResult::Rejected {
                reason: "stale_generation"
            }
        );
        let g = s.generation().unwrap().to_string();
        let future = s.offer(
            &BatchInput {
                generation: &g,
                inst: "i",
                batch_id: &id(2),
                queued_at_ms: T0 + 61_000,
                records: &[snapshot("c", "t")],
            },
            T0,
        );
        assert_eq!(
            future,
            BatchResult::Rejected {
                reason: "age_policy"
            }
        );
        let old = s.offer(
            &BatchInput {
                generation: &g,
                inst: "i",
                batch_id: &id(3),
                queued_at_ms: T0 - 600_000,
                records: &[snapshot("c", "t")],
            },
            T0,
        );
        assert_eq!(
            old,
            BatchResult::Rejected {
                reason: "expired_unaccepted"
            }
        );
        assert_eq!(BatchResult::class("expired_unaccepted"), "permanent");
        assert_eq!(BatchResult::class("age_policy"), "retryable");
    }

    #[test]
    fn a_full_spool_refuses_with_queue_full_until_delivery_frees_room() {
        let dir = tempfile::tempdir().unwrap();
        let policy = Policy {
            spool_bytes: 300,
            ..Policy::default()
        };
        let mut s = Store::open(dir.path(), policy, namer(), T0);
        s.ensure_generation(G_A, T0);
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
        s.ensure_generation(G_A, T0);
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
        s.ensure_generation(G_A, T0);
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
            s.ensure_generation(G_A, T0);
            let BatchResult::Accepted { period_id } =
                offer(&mut s, &id(1), &[snapshot("c1", "kept")], T0)
            else {
                panic!()
            };
            period_id
        };
        let mut s = open(dir.path(), T0 + 1000);
        assert!(
            !s.ensure_generation(G_A, T0 + 1000),
            "same journal keeps its generation"
        );
        assert_eq!(s.outbox().len(), 1);
        assert_eq!(
            offer(&mut s, &id(1), &[snapshot("c1", "kept")], T0 + 2000),
            BatchResult::Duplicate { period_id: p1 }
        );
    }

    #[test]
    fn a_lost_receipt_is_repaired_from_the_batch_file() {
        let dir = tempfile::tempdir().unwrap();
        let generation = {
            let mut s = open(dir.path(), T0);
            s.ensure_generation(G_A, T0);
            offer(&mut s, &id(7), &[snapshot("c1", "kept")], T0);
            s.generation().unwrap().to_string()
        };
        let receipts = dir.path().join("gen").join(&generation).join("receipts");
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
    fn a_different_journal_retires_custody_which_is_never_delivered_or_counted() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = open(dir.path(), T0);
        s.ensure_generation(G_A, T0);
        let gen_a = s.generation().unwrap().to_string();
        offer(&mut s, &id(1), &[snapshot("c1", "MARKER_FOR_A_OPEN")], T0);
        s.tick(T0 + 300_000);
        offer(
            &mut s,
            &id(2),
            &[snapshot("c1", "MARKER_FOR_A_HELD")],
            T0 + 300_000,
        );
        let a_entry = s.outbox().pop().unwrap();

        assert!(s.ensure_generation(G_B, T0 + 300_500));
        let gen_b = s.generation().unwrap().to_string();
        assert_ne!(gen_a, gen_b);
        let st = s.status(T0 + 300_500);
        assert_eq!(st.held_bytes, 0, "retired text is not counted");
        assert_eq!(st.retired.generations, 1);
        assert!(st.retired.bytes > 0);
        assert!(
            s.outbox().is_empty(),
            "retired text is never offered for delivery"
        );
        assert!(
            s.delivered(&a_entry).is_err(),
            "an old-generation entry cannot be released"
        );
        // A's batches can't be replayed into B.
        let r = s.offer(
            &BatchInput {
                generation: &gen_a,
                inst: "inst-1",
                batch_id: &id(3),
                queued_at_ms: T0 + 300_500,
                records: &[snapshot("c", "late A")],
            },
            T0 + 300_500,
        );
        assert_eq!(
            r,
            BatchResult::Rejected {
                reason: "stale_generation"
            }
        );
        // B takes new text.
        offer(
            &mut s,
            &id(4),
            &[snapshot("c1", "MARKER_FOR_B")],
            T0 + 300_600,
        );
        s.tick(T0 + 600_000);
        let b_out = s.outbox();
        assert_eq!(b_out.len(), 1);
        let b_body = fs::read_to_string(b_out[0].pages_path()).unwrap();
        assert!(b_body.contains("MARKER_FOR_B") && !b_body.contains("MARKER_FOR_A"));
        // The retired text is still on disk until discarded, then gone.
        let retired_root = dir.path().join("retired");
        assert!(all_text(&retired_root).contains("MARKER_FOR_A_HELD"));
        assert_eq!(s.discard_retired(), RetiredSummary::default());
        assert!(!all_text(dir.path()).contains("MARKER_FOR_A"));
        // A restart does not resurrect it.
        let s2 = open(dir.path(), T0 + 700_000);
        assert_eq!(s2.status(T0 + 700_000).retired.generations, 0);
        assert_eq!(s2.generation(), Some(gen_b.as_str()));
    }

    #[test]
    fn going_back_to_the_first_journal_does_not_revive_its_retired_text() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = open(dir.path(), T0);
        s.ensure_generation(G_A, T0);
        offer(&mut s, &id(1), &[snapshot("c1", "OLD_A")], T0);
        s.ensure_generation(G_B, T0 + 10);
        s.ensure_generation(G_A, T0 + 20);
        s.tick(T0 + 300_000);
        assert!(s.outbox().is_empty());
        assert_eq!(s.status(T0 + 300_000).retired.generations, 2);
    }
}
