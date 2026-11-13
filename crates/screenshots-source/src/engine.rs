// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The source's state machine over the ledger: listing the folder, the count
//! before the confirm, what each look takes in, pause windows, what goes next
//! and what each answer leaves behind. Synchronous and host-testable; the
//! folder, the clock and the image converter come in through [`Platform`].

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use observer_model::ScreenshotsState;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::ledger::{Item, Ledger, Outcome, PauseWindow, Shot, FILE_NAME};
use crate::rules::{
    content_type, folder_line, folder_name, has_game_bar_marker, has_name_stamp, header_pixels,
    image_kind, is_scratch_name, local_to_instant, same_folder, snipping_tool_time, span_start,
    split_name, stamp, upload_name, within_limits, Answer, ImageKind, Span, TakenFrom, STABLE_MS,
};

/// One entry of a folder listing, read without opening the file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listed {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub modified_ms: i64,
    /// The file system's creation time, when it has one.
    pub created_ms: Option<i64>,
    /// A regular file, not a folder or a link.
    pub regular: bool,
    pub hidden: bool,
    pub system: bool,
    /// Only in the cloud: reading it would download it.
    pub cloud_only: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListError {
    /// The folder isn't there.
    Missing,
    /// It is there and couldn't be listed this time.
    Unreadable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConvertError {
    /// Not an image this computer can decode. Final.
    Undecodable,
    /// The converter couldn't run this time.
    Unavailable,
}

/// The computer, as far as the source needs it.
pub trait Platform: Send + Sync + 'static {
    fn now_ms(&self) -> i64;
    /// The zone's offset from UTC at an instant, in seconds.
    fn offset_at(&self, secs: i64) -> Option<i32>;
    /// Windows' own Screenshots folder, whether or not it exists yet.
    fn own_folder(&self) -> Option<PathBuf>;
    /// The owner's home folder, for the folder line.
    fn home(&self) -> Option<PathBuf>;
    /// The folder's top level.
    fn list(&self, dir: &Path) -> Result<Vec<Listed>, ListError>;
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    /// Convert an image to PNG, keeping its colour profile and what metadata
    /// the converter can carry.
    fn to_png(&self, bytes: &[u8]) -> Result<Vec<u8>, ConvertError>;
}

/// What the app says about the source to its window.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Status {
    pub state: ScreenshotsState,
    /// The folder's full path.
    pub folder: Option<String>,
    /// The folder as the tile says it (S4).
    pub folder_line: Option<String>,
    /// The folder's own name (D3, D9, D16).
    pub folder_name: Option<String>,
    /// The folder is the one Windows resolves, so every image in it comes in.
    pub windows_folder: bool,
    /// Counting before the confirm.
    pub counting: bool,
    pub count_failed: bool,
    /// What each span would bring in, once counted.
    pub counts: Option<BTreeMap<Span, u32>>,
    pub span: Span,
    /// Turned back on after it was on: what's in already doesn't come again.
    pub turned_back_on: bool,
    /// What the owner chose from before, while it's still going in.
    pub backlog: Option<Backlog>,
    /// The paired journal doesn't take screenshots yet.
    pub waiting_for_journal: bool,
    /// Waiting to go to the journal.
    pub waiting: u32,
    /// Final: they stay in the folder.
    pub cant_come_in: u32,
    /// Couldn't be read; read again on every look.
    pub unreadable: u32,
    pub last_checked_ms: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct Backlog {
    pub done: u32,
    pub total: u32,
}

/// The pause the computer is under, when it is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Pause {
    /// When it ends on its own, if it does.
    pub until_secs: Option<i64>,
}

/// One screenshot to send.
#[derive(Clone, Debug, PartialEq)]
pub struct SendJob {
    pub id: String,
    pub path: PathBuf,
    pub ext: String,
    pub captured_at: i64,
    pub taken_from: TakenFrom,
    pub generation: u64,
}

/// One upload, as the journal's contract shapes it.
#[derive(Clone, Debug, PartialEq)]
pub struct Upload {
    pub day: String,
    pub segment: String,
    pub captured_at: String,
    pub taken_from: TakenFrom,
    pub filename: String,
    pub content_type: &'static str,
    pub bytes: Vec<u8>,
    /// The sha256 of the file as it is in the folder.
    pub file_hash: String,
}

/// What preparing a send found.
#[derive(Clone, Debug, PartialEq)]
pub enum Prepared {
    Upload(Upload),
    /// The journal would not keep it: a type that won't convert, or too big.
    Refused,
    /// The file is gone.
    Missing,
    /// Couldn't be read or converted this time.
    Retry,
}

/// What a send came to.
#[derive(Clone, Debug, PartialEq)]
pub enum Sent {
    Answer(Answer, Option<String>),
    Refused,
    Missing,
    Retry,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Count {
    Unknown,
    Ready,
    Failed,
}

#[derive(Clone, Debug)]
struct Read {
    hash: String,
    pixels: Option<u64>,
    game_bar: bool,
}

type CacheKey = (PathBuf, u64, i64);

/// Read a file, prepare it for the journal, and stamp it. Off the engine, so the
/// read and any conversion never hold the ledger.
pub fn prepare(platform: &dyn Platform, job: &SendJob) -> Prepared {
    let bytes = match platform.read(&job.path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Prepared::Missing,
        Err(_) => return Prepared::Retry,
    };
    let file_hash = sha256_hex(&bytes);
    let (ext, bytes) = match image_kind(&job.ext) {
        Some(ImageKind::Accepted) => {
            if !within_limits(bytes.len() as u64, header_pixels(&bytes)) {
                return Prepared::Refused;
            }
            (job.ext.clone(), bytes)
        }
        Some(ImageKind::Convertible) => match platform.to_png(&bytes) {
            Ok(png) => {
                if !within_limits(png.len() as u64, header_pixels(&png)) {
                    return Prepared::Refused;
                }
                ("png".to_string(), png)
            }
            Err(ConvertError::Undecodable) => return Prepared::Refused,
            Err(ConvertError::Unavailable) => return Prepared::Retry,
        },
        None => return Prepared::Refused,
    };
    let Some(offset) = platform.offset_at(job.captured_at) else {
        return Prepared::Retry;
    };
    let stamped = stamp(job.captured_at, offset);
    Prepared::Upload(Upload {
        day: stamped.day,
        segment: stamped.segment,
        captured_at: stamped.captured_at,
        taken_from: job.taken_from,
        content_type: content_type(&ext),
        filename: upload_name(&ext),
        bytes,
        file_hash,
    })
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push(HEX[usize::from(byte >> 4)] as char);
        out.push(HEX[usize::from(byte & 0xf)] as char);
    }
    out
}

pub struct Engine {
    dir: PathBuf,
    platform: Arc<dyn Platform>,
    ledger: Ledger,
    cache: HashMap<CacheKey, Read>,
    count: Count,
    missing: bool,
    generation: Arc<AtomicU64>,
    counters: Option<source_counters::SharedCounters>,
}

impl Engine {
    /// Open the ledger in `dir`. A ledger that doesn't read is moved aside and
    /// the source starts fresh; the journal answers `duplicate` for anything
    /// it already holds.
    pub fn open(
        dir: PathBuf,
        platform: Arc<dyn Platform>,
        counters: Option<source_counters::SharedCounters>,
    ) -> io::Result<Self> {
        let ledger = match Ledger::load(&dir) {
            Ok(ledger) => ledger,
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                tracing::warn!(target: "screenshots", %error, "screenshots ledger unreadable; starting fresh");
                let _ = std::fs::rename(
                    dir.join(FILE_NAME),
                    dir.join(format!("{FILE_NAME}.unreadable")),
                );
                Ledger::default()
            }
            Err(error) => return Err(error),
        };
        Ok(Self {
            dir,
            platform,
            ledger,
            cache: HashMap::new(),
            count: Count::Unknown,
            missing: false,
            generation: Arc::new(AtomicU64::new(0)),
            counters,
        })
    }

    /// The setup generation, shared with whoever announces a new pairing: it
    /// moves at once, so a send that started before records nothing.
    pub fn share_generation(&mut self, generation: Arc<AtomicU64>) {
        self.generation = generation;
    }

    /// A new pairing, for a caller that drives the engine directly.
    pub fn bump_generation(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    fn save(&self) {
        if let Err(error) = self.ledger.save(&self.dir) {
            tracing::warn!(target: "screenshots", %error, "couldn't save the screenshots ledger");
        }
    }

    fn now_ms(&self) -> i64 {
        self.platform.now_ms()
    }

    fn offset(&self) -> impl Fn(i64) -> i32 + '_ {
        |secs| self.platform.offset_at(secs).unwrap_or(0)
    }

    /// The folder the source reads, and whether it is Windows' own.
    fn folder(&self) -> Option<(PathBuf, bool)> {
        let own = self.platform.own_folder();
        match (&self.ledger.chosen, own) {
            (Some(chosen), own) => {
                let is_own = own
                    .as_ref()
                    .and_then(|own| own.to_str())
                    .is_some_and(|own| same_folder(chosen, own));
                Some((PathBuf::from(chosen), is_own))
            }
            (None, Some(own)) => Some((own, true)),
            (None, None) => None,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.ledger.enabled
    }

    pub fn is_confirmed(&self) -> bool {
        self.ledger.confirmed
    }

    pub fn needs_scan_soon(&self) -> bool {
        self.ledger.enabled && !self.ledger.confirmed && self.count == Count::Unknown
    }

    // ── the owner's choices ────────────────────────────────────────────────

    pub fn turn_on(&mut self) {
        self.ledger.consented = true;
        self.ledger.enabled = true;
        self.ledger.confirmed = false;
        self.count = Count::Unknown;
        self.save();
    }

    /// Off: nothing more comes in. What's in the journal stays, and so does the
    /// record of it, so turning back on never brings it in twice.
    pub fn turn_off(&mut self) {
        self.ledger.enabled = false;
        self.ledger.confirmed = false;
        self.ledger.live_from = None;
        self.ledger
            .items
            .retain(|item| !(item.frozen && item.outcome.waiting()));
        for item in &mut self.ledger.items {
            item.frozen = false;
        }
        self.ledger.pause_windows.clear();
        self.ledger.seen.clear();
        self.count = Count::Unknown;
        self.missing = false;
        self.save();
    }

    /// Read another folder. What was waiting from the old one is dropped; what
    /// reached the journal is remembered, and the count starts again.
    pub fn choose_folder(&mut self, path: String) {
        self.ledger.chosen = Some(path);
        self.ledger.consented = true;
        self.ledger.enabled = true;
        self.ledger.confirmed = false;
        self.ledger.live_from = None;
        self.ledger.folder_seen = false;
        self.ledger
            .items
            .retain(|item| item.outcome == Outcome::Delivered);
        for item in &mut self.ledger.items {
            item.frozen = false;
        }
        self.ledger.seen.clear();
        self.cache.clear();
        self.count = Count::Unknown;
        self.missing = false;
        self.save();
    }

    /// A newly paired journal: back to the chooser, with no S17. What reached
    /// the old journal goes to the new one too; screenshots waiting still go.
    pub fn restart_setup(&mut self) {
        self.ledger.confirmed = false;
        self.ledger.was_on = false;
        self.ledger.live_from = None;
        self.ledger.items.retain(|item| {
            item.outcome != Outcome::Delivered && !(item.frozen && item.outcome.waiting())
        });
        for item in &mut self.ledger.items {
            item.frozen = false;
        }
        self.ledger.seen.clear();
        self.count = Count::Unknown;
        self.save();
    }

    /// Freeze what was counted for `span` and start bringing it in. Refused
    /// until the count is ready.
    pub fn confirm(&mut self, span: Span) -> bool {
        if !self.ledger.enabled || self.ledger.confirmed || self.count != Count::Ready {
            return false;
        }
        let start = span_start(span, self.now_ms().div_euclid(1000), &self.offset());
        // Each confirm's set is the count the owner saw: an earlier backfill
        // stops counting toward this one.
        self.ledger
            .items
            .retain(|item| !(item.frozen && item.outcome.waiting()));
        for item in &mut self.ledger.items {
            item.frozen = false;
        }
        let delivered = self.delivered_ids();
        let mut taken = Vec::new();
        let mut seen_ids = HashSet::new();
        for shot in self.ledger.seen.clone() {
            if !seen_ids.insert(shot.id.clone())
                || delivered.contains(&shot.id)
                || shot.captured_at.is_none_or(|at| at < start)
            {
                continue;
            }
            if let Some(item) = self.ledger.items.iter_mut().find(|i| i.shot.id == shot.id) {
                if item.outcome == Outcome::Deleted {
                    item.outcome = Outcome::Pending;
                }
                item.frozen = true;
                item.shot = shot;
                continue;
            }
            if shot.has_hash() {
                taken.push(shot.id.clone());
            }
            let outcome = if shot.unread() {
                Outcome::Unreadable
            } else {
                Outcome::Pending
            };
            self.ledger.items.push(Item {
                shot,
                frozen: true,
                outcome,
            });
        }
        self.ledger.span = span;
        self.ledger.live_from = Some(start);
        self.ledger.confirmed = true;
        self.ledger.was_on = true;
        self.save();
        self.count_taken(&taken);
        true
    }

    fn count_taken(&self, ids: &[String]) {
        if ids.is_empty() {
            return;
        }
        if let Some(counters) = &self.counters {
            counters.add_screenshots(ids, self.now_ms().div_euclid(1000));
        }
    }

    fn delivered_ids(&self) -> HashSet<String> {
        self.ledger
            .items
            .iter()
            .filter(|item| item.outcome == Outcome::Delivered)
            .map(|item| item.shot.id.clone())
            .collect()
    }

    // ── pause ──────────────────────────────────────────────────────────────

    /// Open a window when the computer pauses, and close it at the moment of
    /// resume (or when the pause was due to end, if that has passed).
    pub fn reconcile_pause(&mut self, pause: Option<Pause>) {
        let now = self.now_ms().div_euclid(1000);
        let open = self
            .ledger
            .pause_windows
            .iter_mut()
            .find(|window| window.end.is_none());
        match (pause, open) {
            (Some(pause), Some(window)) => {
                if window.expiration != pause.until_secs {
                    window.expiration = pause.until_secs;
                    self.save();
                }
            }
            (Some(pause), None) => {
                if self.ledger.confirmed {
                    self.ledger.pause_windows.push(PauseWindow {
                        start: now,
                        end: None,
                        expiration: pause.until_secs,
                    });
                    self.save();
                }
            }
            (None, Some(window)) => {
                window.end = Some(match window.expiration {
                    Some(at) if at <= now => at.max(window.start),
                    _ => now,
                });
                self.save();
            }
            (None, None) => {}
        }
    }

    // ── a look at the folder ───────────────────────────────────────────────

    /// List the folder: the count before the confirm, or what comes in after.
    pub fn scan(&mut self) {
        if !self.ledger.enabled {
            return;
        }
        let Some((dir, own)) = self.folder() else {
            return;
        };
        let now = self.now_ms();
        let listing = match self.platform.list(&dir) {
            Ok(listing) => {
                self.missing = false;
                if !self.ledger.folder_seen {
                    self.ledger.folder_seen = true;
                }
                listing
            }
            Err(ListError::Missing) if self.ledger.folder_seen => {
                self.missing = true;
                self.ledger.last_checked_ms = Some(now);
                self.save();
                return;
            }
            // Windows' folder not made yet: empty until the first screenshot.
            Err(ListError::Missing) => Vec::new(),
            Err(ListError::Unreadable) => {
                if !self.ledger.confirmed {
                    self.count = Count::Failed;
                }
                return;
            }
        };
        let listed_paths: HashSet<String> = listing
            .iter()
            .filter_map(|entry| entry.path.to_str().map(str::to_owned))
            .collect();
        let mut shots: Vec<Shot> = listing
            .iter()
            .filter_map(|entry| self.shot_for(entry, own, now))
            .collect();
        shots.sort_by(|a, b| (a.captured_at, &a.path).cmp(&(b.captured_at, &b.path)));
        self.cache.retain(|(path, _, _), _| {
            path.to_str()
                .is_some_and(|path| listed_paths.contains(path))
        });
        self.ledger.seen = shots;
        self.ledger.last_checked_ms = Some(now);
        if self.ledger.confirmed {
            self.absorb(&listed_paths);
        } else {
            self.count = Count::Ready;
        }
        self.save();
    }

    /// One listed file as a screenshot, or `None` when it isn't one this
    /// folder gives up.
    fn shot_for(&mut self, entry: &Listed, own: bool, now_ms: i64) -> Option<Shot> {
        if !entry.regular || entry.hidden || entry.system || is_scratch_name(&entry.name) {
            return None;
        }
        if now_ms - entry.modified_ms < STABLE_MS {
            return None;
        }
        let path = entry.path.to_str()?.to_owned();
        let (ext, stem) = split_name(&entry.name);
        let kind = image_kind(&ext)?;
        let name_time = snipping_tool_time(stem);
        let named = has_name_stamp(stem);
        let (captured_at, taken_from) = match (name_time, entry.created_ms) {
            (Some(local), _) => (
                local_to_instant(local.as_local_secs(), &self.offset()),
                TakenFrom::Platform,
            ),
            (None, Some(created)) => (created.div_euclid(1000), TakenFrom::Platform),
            (None, None) => (entry.modified_ms.div_euclid(1000), TakenFrom::File),
        };
        let mut shot = Shot {
            id: String::new(),
            path: path.clone(),
            captured_at: Some(captured_at),
            taken_from: Some(taken_from),
            bytes: entry.size,
            ext: ext.clone(),
            pixels: None,
            cloud_only: entry.cloud_only,
        };
        if entry.cloud_only {
            // Counted from its listing; read only when it is sent. A chosen
            // folder needs the name, since the marker is inside the file.
            if !own && !named {
                return None;
            }
            shot.id = format!("cloud:{path}");
            return Some(shot);
        }
        // A chosen folder gives up only what Windows named or Game Bar marked;
        // only a PNG can carry the marker, so nothing else is read to look.
        if !own && !named && ext != "png" {
            return None;
        }
        match self.read_cached(entry, kind) {
            Some(read) => {
                if !own && !named && !read.game_bar {
                    return None;
                }
                shot.id = read.hash;
                shot.pixels = read.pixels;
            }
            None => {
                if !own && !named {
                    return None;
                }
                shot.id = format!("path:{path}");
            }
        }
        Some(shot)
    }

    fn read_cached(&mut self, entry: &Listed, kind: ImageKind) -> Option<Read> {
        let key = (entry.path.clone(), entry.size, entry.modified_ms);
        if let Some(read) = self.cache.get(&key) {
            return Some(read.clone());
        }
        let bytes = self.platform.read(&entry.path).ok()?;
        let read = Read {
            hash: sha256_hex(&bytes),
            pixels: (kind == ImageKind::Accepted)
                .then(|| header_pixels(&bytes))
                .flatten(),
            game_bar: has_game_bar_marker(&bytes),
        };
        self.cache.insert(key, read.clone());
        Some(read)
    }

    /// After the confirm: what's gone, what's read now, and what's new.
    fn absorb(&mut self, listed_paths: &HashSet<String>) {
        let live_from = self.ledger.live_from.unwrap_or(i64::MAX);
        let windows = self.ledger.pause_windows.clone();
        let paused_at =
            |at: Option<i64>| at.is_some_and(|at| windows.iter().any(|w| w.excludes(at)));
        // Taken while paused: never comes in, unless it was part of the confirm.
        self.ledger.items.retain(|item| {
            item.frozen || !item.outcome.waiting() || !paused_at(item.shot.captured_at)
        });

        let mut taken = Vec::new();
        let seen = self.ledger.seen.clone();
        let seen_ids: HashSet<&str> = seen.iter().map(|shot| shot.id.as_str()).collect();
        // A file read now that couldn't be before, or a cloud file brought down.
        for shot in &seen {
            if !shot.has_hash() || self.ledger.items.iter().any(|i| i.shot.id == shot.id) {
                continue;
            }
            if let Some(item) = self.ledger.items.iter_mut().find(|item| {
                item.shot.path == shot.path
                    && !item.shot.has_hash()
                    && matches!(
                        item.outcome,
                        Outcome::Unreadable | Outcome::Pending | Outcome::Held
                    )
            }) {
                item.shot = shot.clone();
                item.outcome = Outcome::Pending;
                taken.push(shot.id.clone());
            }
        }
        // Gone before it was sent. A file still at its path is mid-write.
        for item in &mut self.ledger.items {
            let open = item.outcome.waiting() || item.outcome == Outcome::Unreadable;
            if open
                && !seen_ids.contains(item.shot.id.as_str())
                && !listed_paths.contains(&item.shot.path)
            {
                item.outcome = Outcome::Deleted;
            }
        }
        let known: HashSet<String> = self
            .ledger
            .items
            .iter()
            .map(|item| item.shot.id.clone())
            .collect();
        for shot in seen {
            if known.contains(&shot.id)
                || shot.captured_at.is_none_or(|at| at < live_from)
                || paused_at(shot.captured_at)
            {
                continue;
            }
            if self.ledger.items.iter().any(|item| {
                item.shot.path == shot.path && !item.shot.has_hash() && !shot.has_hash()
            }) {
                continue;
            }
            if shot.has_hash() {
                taken.push(shot.id.clone());
            }
            let outcome = if shot.unread() {
                Outcome::Unreadable
            } else {
                Outcome::Pending
            };
            self.ledger.items.push(Item {
                shot,
                frozen: false,
                outcome,
            });
        }
        self.count_taken(&taken);
    }

    // ── delivery ───────────────────────────────────────────────────────────

    /// The next screenshot to send, in the order they were taken in.
    pub fn next_send(&self) -> Option<SendJob> {
        if !self.ledger.enabled || !self.ledger.confirmed || self.missing {
            return None;
        }
        self.ledger
            .items
            .iter()
            .find(|item| {
                item.outcome.waiting()
                    && !item.shot.unread()
                    && item.shot.captured_at.is_some()
                    && (item.frozen
                        || !item
                            .shot
                            .captured_at
                            .is_some_and(|at| self.ledger.in_pause_window(at)))
            })
            .map(|item| SendJob {
                id: item.shot.id.clone(),
                path: PathBuf::from(&item.shot.path),
                ext: item.shot.ext.clone(),
                captured_at: item.shot.captured_at.unwrap_or_default(),
                taken_from: item.shot.taken_from.unwrap_or(TakenFrom::File),
                generation: self.generation(),
            })
    }

    pub fn platform(&self) -> Arc<dyn Platform> {
        Arc::clone(&self.platform)
    }

    /// Record what a send came to. Nothing is recorded for a send that began
    /// before a new pairing, or for a screenshot no longer in the ledger.
    pub fn record(&mut self, job: &SendJob, sent: Sent) {
        if job.generation != self.generation() {
            return;
        }
        let Some(item) = self
            .ledger
            .items
            .iter_mut()
            .find(|item| item.shot.id == job.id)
        else {
            return;
        };
        let (outcome, file_hash) = match sent {
            Sent::Answer(Answer::Delivered, hash) => (Outcome::Delivered, hash),
            Sent::Answer(Answer::Refused, hash) => (Outcome::Refused, hash),
            Sent::Answer(Answer::Held, hash) => (Outcome::Held, hash),
            Sent::Refused => (Outcome::Refused, None),
            Sent::Missing => (Outcome::Deleted, None),
            Sent::Retry => (Outcome::Held, None),
        };
        item.outcome = outcome;
        let mut taken = Vec::new();
        // A cloud file is named by its bytes once it has been read.
        if item.shot.cloud_only && !item.shot.has_hash() {
            if let Some(hash) = file_hash {
                item.shot.id = hash.clone();
                item.shot.cloud_only = false;
                taken.push(hash);
            }
        }
        self.save();
        self.count_taken(&taken);
    }

    // ── what the window shows ──────────────────────────────────────────────

    pub fn status(&self, paused: bool, waiting_for_journal: bool) -> Status {
        let folder = self.folder();
        let home = self.platform.home();
        let home = home.as_ref().and_then(|home| home.to_str());
        let path = folder
            .as_ref()
            .and_then(|(path, _)| path.to_str().map(str::to_owned));
        let ledger = &self.ledger;
        let frozen: Vec<&Item> = ledger.items.iter().filter(|item| item.frozen).collect();
        let open = frozen.iter().any(|item| item.outcome.waiting());
        let backlog = (ledger.confirmed && open).then(|| Backlog {
            done: frozen.iter().filter(|item| !item.outcome.waiting()).count() as u32,
            total: frozen.len() as u32,
        });
        let state = if folder.is_none() {
            ScreenshotsState::NoFolder
        } else if !ledger.enabled {
            ScreenshotsState::Off
        } else if self.missing {
            ScreenshotsState::FolderMissing
        } else if !ledger.confirmed {
            ScreenshotsState::Choosing
        } else if paused {
            ScreenshotsState::Paused
        } else if backlog.is_some() {
            ScreenshotsState::BringingIn
        } else {
            ScreenshotsState::On
        };
        let counts = (state == ScreenshotsState::Choosing && self.count == Count::Ready)
            .then(|| self.counts());
        let count_items = |outcome: Outcome| {
            ledger
                .items
                .iter()
                .filter(|item| item.outcome == outcome)
                .count() as u32
        };
        Status {
            state,
            folder_line: path.as_deref().map(|path| folder_line(path, home)),
            folder_name: path.as_deref().map(folder_name),
            folder: path,
            windows_folder: folder.is_some_and(|(_, own)| own),
            counting: state == ScreenshotsState::Choosing && self.count == Count::Unknown,
            count_failed: state == ScreenshotsState::Choosing && self.count == Count::Failed,
            counts,
            span: ledger.span,
            turned_back_on: ledger.was_on && !ledger.confirmed,
            backlog,
            waiting_for_journal: waiting_for_journal
                && matches!(state, ScreenshotsState::On | ScreenshotsState::BringingIn),
            waiting: ledger
                .items
                .iter()
                .filter(|item| item.outcome.waiting())
                .count() as u32,
            cant_come_in: count_items(Outcome::Refused),
            unreadable: count_items(Outcome::Unreadable),
            last_checked_ms: ledger.last_checked_ms,
        }
    }

    /// What each span would bring in: listed, not already in the journal, not
    /// before the span, each screenshot once.
    fn counts(&self) -> BTreeMap<Span, u32> {
        let now = self.now_ms().div_euclid(1000);
        let delivered = self.delivered_ids();
        Span::ALL
            .iter()
            .map(|&span| {
                let start = span_start(span, now, &self.offset());
                let ids: HashSet<&str> = self
                    .ledger
                    .seen
                    .iter()
                    .filter(|shot| {
                        !delivered.contains(&shot.id)
                            && shot.captured_at.is_some_and(|at| at >= start)
                    })
                    .map(|shot| shot.id.as_str())
                    .collect();
                (span, ids.len() as u32)
            })
            .collect()
    }
}
