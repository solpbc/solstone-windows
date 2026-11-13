// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Totals of what this computer has taken in, one file beside the other local
//! records. Nothing here is written into health, an about snapshot, or a log.
//!
//! Time totals are stored in seconds, the unit a later reader should treat as
//! seconds. A sub-second tail sits in `remainder_us` and is not part of that
//! count. `started` is the local date the file was first created. `last` is the
//! intake time of the newest thing that actually increased a total.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const FILE_NAME: &str = "counters.json";

const VERSION: u32 = 1;

/// Which total `last` belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Audio,
    Screen,
    Browsers,
    Obsidian,
    Screenshots,
}

/// One accepted browser batch, in record order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageBatch {
    pub browser_id: String,
    pub batch_id: String,
    pub records: Vec<PageRecord>,
}

/// One record inside a batch. A page is counted only for a snapshot that has
/// both a tab id and a URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageRecord {
    pub snapshot: bool,
    pub ctx: Option<String>,
    pub url: Option<String>,
    /// `url` is already `url_digest`, not the address itself.
    pub url_is_digest: bool,
    /// Unix seconds of the record itself, when it has one.
    pub at_secs: Option<i64>,
}

/// A note the vault engine has already made durable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoteIntake {
    pub id: u64,
    pub path: String,
    pub previous_path: Option<String>,
    pub replaced: Vec<String>,
    pub first_sight: bool,
    pub removed: bool,
    /// The walk that first listed a vault that was already on this computer.
    pub inventory: bool,
    pub at_secs: Option<i64>,
}

/// The shared file. Audio, screen, pages, and notes all write this one record.
#[derive(Clone)]
pub struct SharedCounters {
    inner: Arc<Mutex<Counters>>,
}

impl SharedCounters {
    pub fn open(dir: &Path, today: &str) -> io::Result<Self> {
        Ok(Self {
            inner: Arc::new(Mutex::new(Counters::open(dir, today)?)),
        })
    }

    fn with_mut<R>(&self, f: impl FnOnce(&mut Counters) -> R) -> R {
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut guard)
    }

    pub fn add_audio_span(&self, segment: u64, start_us: u64, end_us: u64, intake: Option<i64>) {
        self.with_mut(|c| c.add_audio_span(segment, start_us, end_us, intake));
    }

    /// `spans == None` means the timelines are already gone. A sealed segment
    /// is left alone, including when a later pass sees no timelines.
    pub fn seal_audio(
        &self,
        segment: u64,
        spans: Option<&[(u64, u64)]>,
        intake: Option<i64>,
        seal: bool,
    ) -> bool {
        self.with_mut(|c| c.seal_audio(segment, spans, intake, seal))
    }

    pub fn note_screen_kept(&self, segment: u64, at_us: u64, display: u64, intake: Option<i64>) {
        self.with_mut(|c| c.note_screen_kept(segment, at_us, display, intake));
    }

    pub fn note_screen_held(&self, segment: u64, at_us: u64) {
        self.with_mut(|c| c.note_screen_held(segment, at_us));
    }

    pub fn pulse_screen(&self, segment: u64, at_us: u64, intake: Option<i64>) {
        self.with_mut(|c| c.pulse_screen(segment, at_us, intake));
    }

    pub fn quarantine(&self, segment: u64) -> bool {
        self.with_mut(|c| c.quarantine(segment))
    }

    pub fn reconcile_pages(&self, batches: &[PageBatch]) -> bool {
        self.with_mut(|c| c.reconcile_pages(batches))
    }

    pub fn reconcile_notes(&self, known_paths: &[String], intakes: &[NoteIntake]) -> bool {
        self.with_mut(|c| c.reconcile_notes(known_paths, intakes))
    }

    pub fn apply_intakes(&self, intakes: &[NoteIntake]) -> bool {
        self.with_mut(|c| c.apply_intakes(intakes))
    }

    /// Screenshots taken in, each by the sha256 of its bytes. One already
    /// counted adds nothing, whatever happened in between.
    pub fn add_screenshots(&self, hashes: &[String], at_secs: i64) -> bool {
        self.with_mut(|c| c.add_screenshots(hashes, at_secs))
    }

    pub fn screenshot_count(&self) -> u64 {
        self.with_mut(|c| c.disk.screenshots.total)
    }

    pub fn last_intake_id(&self) -> u64 {
        self.with_mut(|c| c.disk.obsidian.intake)
    }

    pub fn fail_next_saves(&self, n: u32) {
        self.with_mut(|c| c.fail_next_saves(n));
    }

    pub fn audio_seconds(&self) -> u64 {
        self.with_mut(|c| c.audio_seconds())
    }

    pub fn screen_seconds(&self) -> u64 {
        self.with_mut(|c| c.screen_seconds())
    }

    pub fn page_count(&self) -> u64 {
        self.with_mut(|c| c.page_count())
    }

    pub fn note_count(&self) -> u64 {
        self.with_mut(|c| c.note_count())
    }
}

impl std::fmt::Debug for SharedCounters {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SharedCounters")
    }
}

/// PCM bytes per second. Zero when the format cannot time a buffer.
/// Identity of an address. The counter file and the pending-visit file store
/// this, not the address.
pub fn url_digest(url: &str) -> String {
    digest(url.as_bytes())
}

pub fn pcm_byte_rate(sample_rate_hz: u32, channels: u16, bits_per_sample: u16) -> u64 {
    u64::from(sample_rate_hz)
        .saturating_mul(u64::from(channels))
        .saturating_mul(u64::from(bits_per_sample / 8))
}

/// Timeline rows are `start_us byte_len`. Bytes the timeline does not list are
/// not part of the union.
pub fn spans_from_timeline(text: &str, byte_rate: u64) -> Vec<(u64, u64)> {
    if byte_rate == 0 {
        return Vec::new();
    }
    let mut spans = Vec::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(start_text), Some(len_text)) = (parts.next(), parts.next()) else {
            continue;
        };
        let (Ok(start), Ok(bytes)) = (start_text.parse::<u64>(), len_text.parse::<u64>()) else {
            continue;
        };
        let dur = bytes.saturating_mul(1_000_000) / byte_rate;
        if dur > 0 {
            spans.push((start, start.saturating_add(dur)));
        }
    }
    spans
}

/// Local calendar date for the instant, `YYYY-MM-DD`.
pub fn calendar_date(epoch_secs: u64, utc_offset_secs: i64) -> String {
    let local = (epoch_secs as i64).saturating_add(utc_offset_secs).max(0) as u64;
    let z = local / 86_400 + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

#[derive(Clone, Debug, Default)]
struct ScreenCursor {
    open_from: Option<u64>,
    segment: Option<u64>,
    held: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct TimeTotal {
    seconds: u64,
    remainder_us: u64,
    last: Option<i64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct CountTotal {
    total: u64,
    last: Option<i64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct NotesDisk {
    total: u64,
    last: Option<i64>,
    baselined: bool,
    intake: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
struct SegmentDisk {
    audio_us: u64,
    audio_sealed: bool,
    ranges: Vec<(u64, u64)>,
    screen_us: u64,
    voided: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Disk {
    v: u32,
    started: String,
    audio: TimeTotal,
    screen: TimeTotal,
    browsers: CountTotal,
    obsidian: NotesDisk,
    #[serde(default)]
    segments: BTreeMap<u64, SegmentDisk>,
    #[serde(default)]
    tabs: BTreeMap<String, String>,
    #[serde(default)]
    pages_applied: BTreeSet<String>,
    #[serde(default)]
    note_hashes: BTreeSet<String>,
    #[serde(default)]
    screenshots: CountTotal,
    #[serde(default)]
    screenshot_hashes: BTreeSet<String>,
}

#[derive(Debug)]
pub struct Counters {
    path: PathBuf,
    disk: Disk,
    cursor: ScreenCursor,
    dirty: bool,
    fail_writes: u32,
    saves: u64,
}

impl Counters {
    pub fn open(dir: &Path, today: &str) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let path = dir.join(FILE_NAME);
        let disk = match load_disk(&path)? {
            Some(disk) => disk,
            None => {
                if !valid_date(today) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "the first counter file needs a local date",
                    ));
                }
                Disk {
                    v: VERSION,
                    started: today.to_string(),
                    audio: TimeTotal::default(),
                    screen: TimeTotal::default(),
                    browsers: CountTotal::default(),
                    obsidian: NotesDisk::default(),
                    segments: BTreeMap::new(),
                    tabs: BTreeMap::new(),
                    pages_applied: BTreeSet::new(),
                    note_hashes: BTreeSet::new(),
                    screenshots: CountTotal::default(),
                    screenshot_hashes: BTreeSet::new(),
                }
            }
        };
        let mut counters = Self {
            path,
            disk,
            cursor: ScreenCursor::default(),
            dirty: !path_readable(&dir.join(FILE_NAME)),
            fail_writes: 0,
            saves: 0,
        };
        // A file created just now, or one restored from the backup after the
        // destination was missing, is written on the next successful save.
        // The brand-new file is written immediately so `started` is durable
        // before the first intake.
        if !counters.path.exists() {
            counters.dirty = true;
            counters.save()?;
        }
        Ok(counters)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn fail_next_saves(&mut self, n: u32) {
        self.fail_writes = n;
    }

    pub fn audio_seconds(&self) -> u64 {
        self.disk.audio.seconds
    }

    pub fn screen_seconds(&self) -> u64 {
        self.disk.screen.seconds
    }

    pub fn page_count(&self) -> u64 {
        self.disk.browsers.total
    }

    pub fn note_count(&self) -> u64 {
        self.disk.obsidian.total
    }

    pub fn started(&self) -> &str {
        &self.disk.started
    }

    pub fn last(&self, source: Source) -> Option<i64> {
        match source {
            Source::Audio => self.disk.audio.last,
            Source::Screen => self.disk.screen.last,
            Source::Browsers => self.disk.browsers.last,
            Source::Obsidian => self.disk.obsidian.last,
            Source::Screenshots => self.disk.screenshots.last,
        }
    }

    pub fn add_audio_span(
        &mut self,
        segment: u64,
        start_us: u64,
        end_us: u64,
        intake: Option<i64>,
    ) {
        if let Some(book) = self.disk.segments.get(&segment) {
            if book.voided || book.audio_sealed {
                self.persist_quiet();
                return;
            }
        }
        if end_us > start_us {
            let book = self.disk.segments.entry(segment).or_default();
            insert_range(&mut book.ranges, start_us, end_us);
            let new_us = covered(&book.ranges);
            let old = book.audio_us;
            if new_us > old {
                book.audio_us = new_us;
                add_time(&mut self.disk.audio, new_us - old, intake);
                self.dirty = true;
            }
        }
        self.persist_quiet();
    }

    pub fn seal_audio(
        &mut self,
        segment: u64,
        spans: Option<&[(u64, u64)]>,
        intake: Option<i64>,
        seal: bool,
    ) -> bool {
        if self
            .disk
            .segments
            .get(&segment)
            .is_some_and(|book| book.voided || book.audio_sealed)
        {
            return self.persist_ok();
        }
        if let Some(spans) = spans {
            let mut ranges = Vec::new();
            for &(start, end) in spans {
                insert_range(&mut ranges, start, end);
            }
            let new_us = covered(&ranges);
            let old = self
                .disk
                .segments
                .get(&segment)
                .map(|book| book.audio_us)
                .unwrap_or(0);
            let same = self
                .disk
                .segments
                .get(&segment)
                .is_some_and(|book| book.ranges == ranges && book.audio_us == new_us);
            if !same
                && (old != new_us
                    || self.disk.segments.contains_key(&segment)
                    || !ranges.is_empty())
            {
                let book = self.disk.segments.entry(segment).or_default();
                book.ranges = ranges;
                book.audio_us = new_us;
                if new_us > old {
                    add_time(&mut self.disk.audio, new_us - old, intake);
                } else if new_us < old {
                    sub_time(&mut self.disk.audio, old - new_us);
                }
                self.dirty = true;
            }
        }
        if seal
            && !self
                .disk
                .segments
                .get(&segment)
                .is_some_and(|book| book.audio_sealed)
        {
            self.disk.segments.entry(segment).or_default().audio_sealed = true;
            self.dirty = true;
        }
        self.persist_ok()
    }

    /// `display` is ignored. Two captures of the same interval count once.
    pub fn note_screen_kept(
        &mut self,
        segment: u64,
        at_us: u64,
        _display: u64,
        intake: Option<i64>,
    ) {
        if self
            .disk
            .segments
            .get(&segment)
            .is_some_and(|book| book.voided)
        {
            self.persist_quiet();
            return;
        }
        if self.cursor.held || self.cursor.segment != Some(segment) {
            self.cursor.held = false;
            self.cursor.open_from = Some(at_us);
            self.cursor.segment = Some(segment);
            self.persist_quiet();
            return;
        }
        if let Some(from) = self.cursor.open_from {
            if at_us > from {
                self.credit_screen(segment, at_us - from, intake);
                self.cursor.open_from = Some(at_us);
            }
        } else {
            self.cursor.open_from = Some(at_us);
            self.cursor.segment = Some(segment);
        }
        self.persist_quiet();
    }

    /// A hold drops the open interval. The time from the previous kept frame
    /// through the next kept frame is not taken in.
    pub fn note_screen_held(&mut self, _segment: u64, _at_us: u64) {
        self.cursor.open_from = None;
        self.cursor.segment = None;
        self.cursor.held = true;
        self.persist_quiet();
    }

    pub fn pulse_screen(&mut self, segment: u64, at_us: u64, intake: Option<i64>) {
        if self.cursor.held || self.cursor.segment != Some(segment) {
            self.persist_quiet();
            return;
        }
        if let Some(from) = self.cursor.open_from {
            if at_us > from {
                self.credit_screen(segment, at_us - from, intake);
                self.cursor.open_from = Some(at_us);
            }
        }
        self.persist_quiet();
    }

    pub fn quarantine(&mut self, segment: u64) -> bool {
        let already = self
            .disk
            .segments
            .get(&segment)
            .is_some_and(|book| book.voided);
        if !already {
            let audio = self
                .disk
                .segments
                .get(&segment)
                .map(|book| book.audio_us)
                .unwrap_or(0);
            let screen = self
                .disk
                .segments
                .get(&segment)
                .map(|book| book.screen_us)
                .unwrap_or(0);
            sub_time(&mut self.disk.audio, audio);
            sub_time(&mut self.disk.screen, screen);
            let book = self.disk.segments.entry(segment).or_default();
            book.audio_us = 0;
            book.screen_us = 0;
            book.ranges.clear();
            book.audio_sealed = true;
            book.voided = true;
            self.dirty = true;
        }
        self.persist_ok()
    }

    pub fn reconcile_pages(&mut self, batches: &[PageBatch]) -> bool {
        for batch in batches {
            self.apply_batch(batch);
        }
        self.persist_ok()
    }

    pub fn reconcile_notes(&mut self, known_paths: &[String], intakes: &[NoteIntake]) -> bool {
        let mut ordered = intakes.to_vec();
        ordered.sort_by_key(|intake| intake.id);
        if !self.disk.obsidian.baselined {
            let fresh: BTreeSet<&str> = ordered
                .iter()
                .filter(|intake| is_fresh_place(intake))
                .map(|intake| intake.path.as_str())
                .collect();
            for path in known_paths {
                if !fresh.contains(path.as_str()) {
                    self.disk.note_hashes.insert(digest(path.as_bytes()));
                }
            }
            self.disk.obsidian.baselined = true;
            self.dirty = true;
        }
        for intake in &ordered {
            self.apply_intake(intake);
        }
        self.persist_ok()
    }

    pub fn add_screenshots(&mut self, hashes: &[String], at_secs: i64) -> bool {
        for hash in hashes {
            if self.disk.screenshot_hashes.insert(hash.clone()) {
                self.disk.screenshots.total = self.disk.screenshots.total.saturating_add(1);
                self.disk.screenshots.last = Some(
                    self.disk
                        .screenshots
                        .last
                        .map_or(at_secs, |prev| prev.max(at_secs)),
                );
                self.dirty = true;
            }
        }
        self.persist_ok()
    }

    pub fn apply_intakes(&mut self, intakes: &[NoteIntake]) -> bool {
        let mut ordered = intakes.to_vec();
        ordered.sort_by_key(|intake| intake.id);
        for intake in &ordered {
            self.apply_intake(intake);
        }
        self.persist_ok()
    }

    fn apply_batch(&mut self, batch: &PageBatch) {
        let key = format!("{}\u{1}{}", batch.browser_id, batch.batch_id);
        if self.disk.pages_applied.contains(&key) {
            return;
        }
        for record in &batch.records {
            if !record.snapshot {
                continue;
            }
            let Some(ctx) = record.ctx.as_deref().filter(|ctx| !ctx.is_empty()) else {
                continue;
            };
            let Some(url) = record.url.as_deref().filter(|url| !url.is_empty()) else {
                continue;
            };
            let tab = format!("{}\u{1}{ctx}", batch.browser_id);
            let url_hash = if record.url_is_digest {
                url.to_string()
            } else {
                digest(url.as_bytes())
            };
            if self
                .disk
                .tabs
                .get(&tab)
                .is_some_and(|seen| seen == &url_hash)
            {
                continue;
            }
            self.disk.tabs.insert(tab, url_hash);
            self.disk.browsers.total = self.disk.browsers.total.saturating_add(1);
            if let Some(at) = record.at_secs {
                self.disk.browsers.last =
                    Some(self.disk.browsers.last.map_or(at, |prev| prev.max(at)));
            }
        }
        self.disk.pages_applied.insert(key);
        self.dirty = true;
    }

    fn apply_intake(&mut self, intake: &NoteIntake) {
        // An id of 0 was assigned before ids started at 1. Apply it by the path
        // set and do not store 0 as the cursor: 0 means nothing has been applied.
        if intake.id != 0 && intake.id <= self.disk.obsidian.intake {
            return;
        }
        if intake.id != 0 {
            self.disk.obsidian.intake = intake.id;
            self.dirty = true;
        }
        let path_hash = digest(intake.path.as_bytes());
        if intake.removed {
            if self.disk.note_hashes.remove(&path_hash) {
                self.dirty = true;
            }
            return;
        }
        if intake.inventory {
            if self.disk.note_hashes.insert(path_hash) {
                self.dirty = true;
            }
            return;
        }
        let mut moved = false;
        if let Some(previous) = intake.previous_path.as_deref() {
            if self.disk.note_hashes.remove(&digest(previous.as_bytes())) {
                moved = true;
            }
        }
        for replaced in &intake.replaced {
            if self.disk.note_hashes.remove(&digest(replaced.as_bytes())) {
                moved = true;
            }
        }
        if moved || !intake.first_sight {
            let inserted = self.disk.note_hashes.insert(path_hash);
            if moved || inserted {
                self.dirty = true;
            }
            return;
        }
        if self.disk.note_hashes.insert(path_hash) {
            self.disk.obsidian.total = self.disk.obsidian.total.saturating_add(1);
            if let Some(at) = intake.at_secs {
                self.disk.obsidian.last =
                    Some(self.disk.obsidian.last.map_or(at, |prev| prev.max(at)));
            }
            self.dirty = true;
        }
    }

    fn credit_screen(&mut self, segment: u64, delta_us: u64, intake: Option<i64>) {
        if delta_us == 0 {
            return;
        }
        let book = self.disk.segments.entry(segment).or_default();
        if book.voided {
            return;
        }
        book.screen_us = book.screen_us.saturating_add(delta_us);
        add_time(&mut self.disk.screen, delta_us, intake);
        self.dirty = true;
    }

    fn persist_quiet(&mut self) {
        if self.dirty {
            let _ = self.save();
        }
    }

    fn persist_ok(&mut self) -> bool {
        if !self.dirty {
            return true;
        }
        self.save().is_ok()
    }

    fn save(&mut self) -> io::Result<()> {
        if self.fail_writes > 0 {
            self.fail_writes -= 1;
            self.dirty = true;
            return Err(io::Error::other("counter save held back"));
        }
        if self.disk.v != VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "counter file version is not one this build writes",
            ));
        }
        let bytes = serde_json::to_vec(&self.disk)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        let tmp = tmp_path(&self.path);
        {
            let mut file = File::create(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        let bak = bak_path(&self.path);
        if self.path.exists() {
            if bak.exists() {
                fs::remove_file(&bak)?;
            }
            fs::rename(&self.path, &bak)?;
        }
        fs::rename(&tmp, &self.path)?;
        sync_dir(self.path.parent());
        self.dirty = false;
        self.saves = self.saves.saturating_add(1);
        Ok(())
    }
}

fn is_fresh_place(intake: &NoteIntake) -> bool {
    intake.first_sight
        && !intake.inventory
        && !intake.removed
        && intake.previous_path.is_none()
        && intake.replaced.is_empty()
}

fn add_time(total: &mut TimeTotal, delta_us: u64, intake: Option<i64>) {
    let whole = total
        .seconds
        .saturating_mul(1_000_000)
        .saturating_add(total.remainder_us)
        .saturating_add(delta_us);
    total.seconds = whole / 1_000_000;
    total.remainder_us = whole % 1_000_000;
    if delta_us > 0 {
        if let Some(at) = intake {
            total.last = Some(total.last.map_or(at, |prev| prev.max(at)));
        }
    }
}

fn sub_time(total: &mut TimeTotal, delta_us: u64) {
    let whole = total
        .seconds
        .saturating_mul(1_000_000)
        .saturating_add(total.remainder_us)
        .saturating_sub(delta_us);
    total.seconds = whole / 1_000_000;
    total.remainder_us = whole % 1_000_000;
}

fn insert_range(ranges: &mut Vec<(u64, u64)>, start: u64, end: u64) {
    if end <= start {
        return;
    }
    ranges.push((start, end));
    ranges.sort_unstable();
    let mut merged = Vec::new();
    for (start, end) in ranges.drain(..) {
        if let Some((_, last_end)) = merged.last_mut() {
            if start <= *last_end {
                if end > *last_end {
                    *last_end = end;
                }
                continue;
            }
        }
        merged.push((start, end));
    }
    *ranges = merged;
}

fn covered(ranges: &[(u64, u64)]) -> u64 {
    ranges.iter().fold(0u64, |sum, (start, end)| {
        sum.saturating_add(end.saturating_sub(*start))
    })
}

fn digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let hashed = Sha256::digest(bytes);
    let mut out = String::with_capacity(hashed.len() * 2);
    for byte in hashed {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0xf) as usize] as char);
    }
    out
}

fn valid_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
}

fn tmp_path(path: &Path) -> PathBuf {
    path.with_file_name(format!("{FILE_NAME}.tmp"))
}

fn bak_path(path: &Path) -> PathBuf {
    path.with_file_name(format!("{FILE_NAME}.bak"))
}

fn path_readable(path: &Path) -> bool {
    fs::read(path).is_ok()
}

fn load_disk(path: &Path) -> io::Result<Option<Disk>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(parse_disk(&bytes)?)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => match fs::read(bak_path(path)) {
            Ok(bytes) => Ok(Some(parse_disk(&bytes)?)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        },
        Err(error) => Err(error),
    }
}

fn parse_disk(bytes: &[u8]) -> io::Result<Disk> {
    let disk: Disk = serde_json::from_slice(bytes)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    if disk.v != VERSION || !valid_date(&disk.started) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "counter file is not readable",
        ));
    }
    Ok(disk)
}

/// One source total as stored, for the settings window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowSourceTotal {
    pub total: u64,
    pub last: Option<i64>,
}

/// The counter document the settings window may show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowCounterDocument {
    pub started: String,
    pub audio: WindowSourceTotal,
    pub screen: WindowSourceTotal,
    pub browsers: WindowSourceTotal,
    pub obsidian: WindowSourceTotal,
    pub screenshots: WindowSourceTotal,
}

/// What the settings window's read found.
///
/// This read does not create a file, does not open the backup, and does not
/// promote one. Absence, a backup alone, and an unreadable file are all
/// [`WindowCounterRead::NoRows`]. An I/O failure is [`Err`]. The settings
/// command turns that error into no rows, which is not `nothing yet`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowCounterRead {
    NoRows,
    Document(WindowCounterDocument),
}

/// A time source as the window shows it: whole seconds, not the two halves.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WindowSecondsCell {
    pub seconds: u64,
    pub last: Option<i64>,
}

/// A counted source as the window shows it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WindowCountCell {
    pub total: u64,
    pub last: Option<i64>,
}

/// The counter document the settings window renders. `None` from
/// [`window_payload`] means no rows.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WindowCounterPayload {
    pub started: String,
    pub audio: WindowSecondsCell,
    pub screen: WindowSecondsCell,
    pub browsers: WindowCountCell,
    pub obsidian: WindowCountCell,
    pub screenshots: WindowCountCell,
}

/// Map a window read onto the document the window renders.
///
/// Time sources keep the stored seconds. Counted sources keep the stored
/// total. A missing read stays missing. This does not open or write a file.
pub fn window_payload(read: &WindowCounterRead) -> Option<WindowCounterPayload> {
    let WindowCounterRead::Document(doc) = read else {
        return None;
    };
    Some(WindowCounterPayload {
        started: doc.started.clone(),
        audio: WindowSecondsCell {
            seconds: doc.audio.total,
            last: doc.audio.last,
        },
        screen: WindowSecondsCell {
            seconds: doc.screen.total,
            last: doc.screen.last,
        },
        browsers: WindowCountCell {
            total: doc.browsers.total,
            last: doc.browsers.last,
        },
        obsidian: WindowCountCell {
            total: doc.obsidian.total,
            last: doc.obsidian.last,
        },
        screenshots: WindowCountCell {
            total: doc.screenshots.total,
            last: doc.screenshots.last,
        },
    })
}

/// Read `counters.json` for the settings window.
pub fn read_window_counters(dir: &Path) -> io::Result<WindowCounterRead> {
    let path = dir.join(FILE_NAME);
    match fs::read(&path) {
        Ok(bytes) => match parse_disk(&bytes) {
            Ok(disk) => Ok(WindowCounterRead::Document(WindowCounterDocument {
                started: disk.started,
                audio: WindowSourceTotal {
                    total: disk.audio.seconds,
                    last: disk.audio.last,
                },
                screen: WindowSourceTotal {
                    total: disk.screen.seconds,
                    last: disk.screen.last,
                },
                browsers: WindowSourceTotal {
                    total: disk.browsers.total,
                    last: disk.browsers.last,
                },
                obsidian: WindowSourceTotal {
                    total: disk.obsidian.total,
                    last: disk.obsidian.last,
                },
                screenshots: WindowSourceTotal {
                    total: disk.screenshots.total,
                    last: disk.screenshots.last,
                },
            })),
            Err(_) => Ok(WindowCounterRead::NoRows),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(WindowCounterRead::NoRows),
        Err(error) => Err(error),
    }
}

fn sync_dir(dir: Option<&Path>) {
    #[cfg(unix)]
    if let Some(dir) = dir {
        if let Ok(handle) = File::open(dir) {
            let _ = handle.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = 1_000_000;

    fn open_at(today: &str) -> (tempfile::TempDir, Counters) {
        let dir = tempfile::tempdir().unwrap();
        let counters = Counters::open(dir.path(), today).unwrap();
        (dir, counters)
    }

    fn audio(counters: &mut Counters, segment: u64, start_s: u64, end_s: u64, at: i64) {
        counters.add_audio_span(segment, start_s * S, end_s * S, Some(at));
    }

    fn page(url: &str, ctx: &str, batch: &str, at: i64) -> PageBatch {
        PageBatch {
            browser_id: "browser-1".to_string(),
            batch_id: batch.to_string(),
            records: vec![PageRecord {
                snapshot: true,
                ctx: Some(ctx.to_string()),
                url: Some(url.to_string()),
                url_is_digest: false,
                at_secs: Some(at),
            }],
        }
    }

    fn intake(id: u64, path: &str) -> NoteIntake {
        NoteIntake {
            id,
            path: path.to_string(),
            previous_path: None,
            replaced: Vec::new(),
            first_sight: true,
            removed: false,
            inventory: false,
            at_secs: Some(50 + id as i64),
        }
    }

    #[test]
    fn the_same_segment_unions_microphone_and_computer_sound() {
        let (_dir, mut counters) = open_at("2026-11-09");
        audio(&mut counters, 1, 0, 10, 100);
        audio(&mut counters, 1, 5, 15, 110);
        assert_eq!(counters.audio_seconds(), 15);
        audio(&mut counters, 1, 5, 15, 120);
        assert_eq!(counters.audio_seconds(), 15);
        assert_eq!(counters.last(Source::Audio), Some(110));
    }

    #[test]
    fn two_segments_add_their_unions() {
        let (_dir, mut counters) = open_at("2026-11-09");
        audio(&mut counters, 1, 0, 10, 1);
        audio(&mut counters, 1, 5, 15, 2);
        audio(&mut counters, 2, 0, 10, 3);
        audio(&mut counters, 2, 5, 15, 4);
        assert_eq!(counters.audio_seconds(), 30);
    }

    #[test]
    fn recovery_sets_the_union_whether_the_saved_credit_was_low_or_high() {
        for prior in [0u64, 4, 20] {
            let (_dir, mut counters) = open_at("2026-11-09");
            if prior > 0 {
                audio(&mut counters, 7, 0, prior, 10);
            }
            counters.seal_audio(7, Some(&[(0, 10 * S), (5 * S, 15 * S)]), Some(10), true);
            assert_eq!(counters.audio_seconds(), 15, "prior {prior}");
            counters.seal_audio(7, None, Some(10 + 86_400), true);
            assert_eq!(counters.audio_seconds(), 15, "second pass {prior}");
            assert_eq!(counters.last(Source::Audio), Some(10));
        }
    }

    #[test]
    fn a_span_missing_from_the_timeline_is_not_in_the_union() {
        let (_dir, mut counters) = open_at("2026-11-09");
        audio(&mut counters, 3, 0, 20, 4);
        counters.seal_audio(3, Some(&[(0, 10 * S)]), Some(4), true);
        assert_eq!(counters.audio_seconds(), 10);
        counters.seal_audio(3, None, Some(90_000), true);
        assert_eq!(counters.audio_seconds(), 10);
        assert_eq!(counters.last(Source::Audio), Some(4));
    }

    #[test]
    fn an_open_segment_unions_a_later_span_with_the_timeline() {
        let (_dir, mut counters) = open_at("2026-11-09");
        counters.seal_audio(4, Some(&[(0, 10 * S)]), Some(1), false);
        audio(&mut counters, 4, 5, 15, 2);
        assert_eq!(counters.audio_seconds(), 15);
        audio(&mut counters, 4, 5, 15, 3);
        assert_eq!(counters.audio_seconds(), 15);
    }

    #[test]
    fn the_on_disk_audio_integer_is_seconds_and_does_not_wrap() {
        let (dir, mut counters) = open_at("2026-11-09");
        audio(&mut counters, 1, 0, 15, 8);
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(counters.path()).unwrap()).unwrap();
        assert_eq!(value["audio"]["seconds"], 15);
        assert_eq!(value["audio"]["remainder_us"], 0);
        counters.add_audio_span(2, 0, 3_000_000_000 * S, Some(9));
        assert_eq!(counters.audio_seconds(), 15 + 3_000_000_000);
        drop(counters);
        let reloaded = Counters::open(dir.path(), "1999-01-01").unwrap();
        assert_eq!(reloaded.audio_seconds(), 15 + 3_000_000_000);
        assert_eq!(reloaded.started(), "2026-11-09");
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(reloaded.path()).unwrap()).unwrap();
        assert_eq!(value["audio"]["seconds"].as_u64(), Some(15 + 3_000_000_000));
    }

    #[test]
    fn a_sub_second_tail_is_not_stored_as_the_seconds_count() {
        let (_dir, mut counters) = open_at("2026-11-09");
        counters.add_audio_span(1, 0, 1_500_000, Some(1));
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(counters.path()).unwrap()).unwrap();
        assert_eq!(value["audio"]["seconds"], 1);
        assert_eq!(value["audio"]["remainder_us"], 500_000);
    }

    #[test]
    fn quarantine_drops_audio_and_screen_once_in_one_write() {
        let (dir, mut counters) = open_at("2026-11-09");
        audio(&mut counters, 3, 0, 4, 20);
        counters.note_screen_kept(3, 0, 1, Some(20));
        counters.note_screen_kept(3, 4 * S, 1, Some(24));
        assert_eq!(counters.screen_seconds(), 4);
        let pre = fs::read(counters.path()).unwrap();
        let saves = counters.saves;
        counters.quarantine(3);
        assert_eq!(counters.saves, saves + 1);
        assert_eq!(counters.audio_seconds(), 0);
        assert_eq!(counters.screen_seconds(), 0);
        assert_eq!(counters.last(Source::Audio), Some(20));
        let post = fs::read(counters.path()).unwrap();

        fs::write(dir.path().join(FILE_NAME), &pre).unwrap();
        let mut again = Counters::open(dir.path(), "2026-11-09").unwrap();
        again.quarantine(3);
        assert_eq!(again.audio_seconds(), 0);
        assert_eq!(again.screen_seconds(), 0);

        fs::write(dir.path().join(FILE_NAME), &post).unwrap();
        let mut committed = Counters::open(dir.path(), "2026-11-09").unwrap();
        let saves = committed.saves;
        committed.quarantine(3);
        assert_eq!(committed.audio_seconds(), 0);
        assert_eq!(committed.screen_seconds(), 0);
        assert_eq!(committed.saves, saves);
    }

    #[test]
    fn screen_time_is_not_taken_from_audio_or_from_a_second_display() {
        let (_dir, mut counters) = open_at("2026-11-09");
        counters.note_screen_kept(1, 0, 1, Some(1));
        counters.note_screen_kept(1, 4 * S, 1, Some(5));
        counters.seal_audio(9, Some(&[(0, 300 * S)]), Some(5), true);
        assert_eq!(counters.screen_seconds(), 4);
        assert_eq!(counters.audio_seconds(), 300);
        counters.seal_audio(8, Some(&[(0, 10 * S)]), Some(6), true);
        assert_eq!(counters.screen_seconds(), 4);

        let (_dir, mut counters) = open_at("2026-11-09");
        counters.note_screen_kept(1, 0, 1, Some(1));
        counters.note_screen_kept(1, 0, 2, Some(1));
        counters.note_screen_kept(1, 2 * S, 1, Some(3));
        counters.note_screen_kept(1, 2 * S, 2, Some(3));
        assert_eq!(counters.screen_seconds(), 2);
    }

    #[test]
    fn held_back_time_is_not_taken_in_and_a_quiet_gap_is() {
        let (_dir, mut counters) = open_at("2026-11-09");
        counters.note_screen_kept(1, 0, 0, Some(1));
        counters.note_screen_held(1, S);
        counters.note_screen_kept(1, 3 * S, 0, Some(4));
        counters.note_screen_kept(1, 5 * S, 0, Some(6));
        assert_eq!(counters.screen_seconds(), 2);
        assert_eq!(counters.last(Source::Screen), Some(6));

        let (_dir, mut counters) = open_at("2026-11-09");
        counters.note_screen_kept(1, 0, 0, Some(1));
        counters.note_screen_kept(1, 2 * S, 0, Some(3));
        assert_eq!(counters.screen_seconds(), 2);

        let (_dir, mut counters) = open_at("2026-11-09");
        counters.note_screen_kept(1, 0, 0, Some(1));
        counters.note_screen_held(1, S);
        counters.pulse_screen(1, 10 * S, Some(99));
        assert_eq!(counters.screen_seconds(), 0);
        assert_eq!(counters.last(Source::Screen), None);
    }

    #[test]
    fn a_static_screen_keeps_counting_until_it_is_held() {
        let (_dir, mut counters) = open_at("2026-11-09");
        counters.note_screen_kept(1, 0, 0, Some(1));
        counters.pulse_screen(1, 2 * S, Some(3));
        assert_eq!(counters.screen_seconds(), 2);
    }

    #[test]
    fn an_accepted_page_survives_a_save_that_never_reached_the_file() {
        let (dir, mut counters) = open_at("2026-11-09");
        counters.fail_next_saves(1);
        let batch = page("https://example.com/a", "tab", "batch-1", 40);
        assert!(!counters.reconcile_pages(std::slice::from_ref(&batch)));
        drop(counters);
        let mut restarted = Counters::open(dir.path(), "2026-11-09").unwrap();
        assert_eq!(restarted.page_count(), 0);
        assert!(restarted.reconcile_pages(std::slice::from_ref(&batch)));
        assert_eq!(restarted.page_count(), 1);
        assert_eq!(restarted.last(Source::Browsers), Some(40));
        drop(restarted);
        let mut again = Counters::open(dir.path(), "2026-11-09").unwrap();
        assert!(again.reconcile_pages(&[batch]));
        assert_eq!(again.page_count(), 1);
        assert_eq!(again.last(Source::Browsers), Some(40));
    }

    #[test]
    fn the_same_tab_and_url_do_not_count_again_after_a_new_process() {
        let (dir, mut counters) = open_at("2026-11-09");
        assert!(counters.reconcile_pages(&[page("https://example.com/a", "tab", "b1", 5)]));
        drop(counters);
        let mut restarted = Counters::open(dir.path(), "2026-11-09").unwrap();
        assert!(restarted.reconcile_pages(&[page("https://example.com/a", "tab", "b2", 80)]));
        assert_eq!(restarted.page_count(), 1);
        assert_eq!(restarted.last(Source::Browsers), Some(5));
        assert!(restarted.reconcile_pages(&[page("https://example.com/b", "tab", "b3", 90)]));
        assert_eq!(restarted.page_count(), 2);
        assert_eq!(restarted.last(Source::Browsers), Some(90));
    }

    #[test]
    fn a_stored_digest_is_the_same_visit_as_the_address() {
        let (_dir, mut counters) = open_at("2026-11-09");
        assert!(counters.reconcile_pages(&[page("https://example.com/a", "tab", "b1", 10)]));
        let same = PageBatch {
            browser_id: "browser-1".to_string(),
            batch_id: "b2".to_string(),
            records: vec![PageRecord {
                snapshot: true,
                ctx: Some("tab".to_string()),
                url: Some(url_digest("https://example.com/a")),
                url_is_digest: true,
                at_secs: Some(80),
            }],
        };
        assert!(counters.reconcile_pages(&[same]));
        assert_eq!(counters.page_count(), 1);
        assert_eq!(counters.last(Source::Browsers), Some(10));
        let other = PageBatch {
            browser_id: "browser-1".to_string(),
            batch_id: "b3".to_string(),
            records: vec![PageRecord {
                snapshot: true,
                ctx: Some("tab".to_string()),
                url: Some(url_digest("https://example.com/b")),
                url_is_digest: true,
                at_secs: Some(90),
            }],
        };
        assert!(counters.reconcile_pages(&[other]));
        assert_eq!(counters.page_count(), 2);
        assert_eq!(counters.last(Source::Browsers), Some(90));
    }

    #[test]
    fn a_snapshot_with_no_tab_counts_nothing_and_a_delta_counts_nothing() {
        let (_dir, mut counters) = open_at("2026-11-09");
        assert!(counters.reconcile_pages(&[page("https://example.com/a", "tab", "b1", 5)]));
        let no_tab = PageBatch {
            browser_id: "browser-1".to_string(),
            batch_id: "b2".to_string(),
            records: vec![PageRecord {
                snapshot: true,
                ctx: None,
                url: Some("https://example.com/b".to_string()),
                url_is_digest: false,
                at_secs: Some(70),
            }],
        };
        let delta = PageBatch {
            browser_id: "browser-1".to_string(),
            batch_id: "b3".to_string(),
            records: vec![PageRecord {
                snapshot: false,
                ctx: Some("tab".to_string()),
                url: Some("https://example.com/c".to_string()),
                url_is_digest: false,
                at_secs: Some(80),
            }],
        };
        assert!(counters.reconcile_pages(&[no_tab, delta]));
        assert_eq!(counters.page_count(), 1);
        assert_eq!(counters.last(Source::Browsers), Some(5));
        assert!(counters.reconcile_pages(&[page("https://example.com/a", "tab", "b4", 90)]));
        assert_eq!(counters.page_count(), 1);
    }

    #[test]
    fn a_counted_page_stays_counted_when_delivery_does_not_happen() {
        let (_dir, mut counters) = open_at("2026-11-09");
        assert!(counters.reconcile_pages(&[page("https://example.com/a", "tab", "b1", 5)]));
        counters.quarantine(99);
        assert_eq!(counters.page_count(), 1);
        assert_eq!(counters.last(Source::Browsers), Some(5));
    }

    #[test]
    fn the_first_run_records_notes_already_in_the_vault_and_counts_none() {
        let (dir, mut counters) = open_at("2026-11-09");
        let known = vec!["a.md".to_string(), "b.md".to_string()];
        assert!(counters.reconcile_notes(&known, &[]));
        assert_eq!(counters.note_count(), 0);
        assert_eq!(counters.last(Source::Obsidian), None);
        let edit = NoteIntake {
            first_sight: false,
            at_secs: Some(90),
            ..intake(1, "a.md")
        };
        let rename = NoteIntake {
            id: 2,
            path: "c.md".to_string(),
            previous_path: Some("b.md".to_string()),
            first_sight: false,
            at_secs: Some(91),
            ..intake(2, "c.md")
        };
        let collapsed = NoteIntake {
            id: 3,
            path: "d.md".to_string(),
            replaced: vec!["a.md".to_string()],
            first_sight: true,
            at_secs: Some(92),
            ..intake(3, "d.md")
        };
        assert!(counters.apply_intakes(&[edit, rename, collapsed]));
        assert_eq!(counters.note_count(), 0);
        assert_eq!(counters.last(Source::Obsidian), None);
        assert!(counters.apply_intakes(&[intake(4, "new.md")]));
        assert_eq!(counters.note_count(), 1);
        drop(counters);
        let mut restarted = Counters::open(dir.path(), "2026-11-09").unwrap();
        assert!(restarted.reconcile_notes(&known, &[intake(4, "new.md")]));
        assert_eq!(restarted.note_count(), 1);
    }

    #[test]
    fn an_intake_whose_id_is_zero_counts_once_and_applying_it_again_stays_at_one() {
        let (dir, mut counters) = open_at("2026-11-09");
        audio(&mut counters, 1, 0, 99, 10);
        assert_eq!(counters.audio_seconds(), 99);
        assert!(counters.apply_intakes(&[intake(0, "a.md")]));
        assert_eq!(counters.note_count(), 1);
        assert_eq!(counters.last(Source::Obsidian), Some(50));
        assert_eq!(counters.audio_seconds(), 99);
        assert!(counters.apply_intakes(&[intake(0, "a.md")]));
        assert_eq!(counters.note_count(), 1);
        assert_eq!(counters.audio_seconds(), 99);
        drop(counters);
        let on_disk: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.path().join(FILE_NAME)).unwrap()).unwrap();
        assert_eq!(on_disk["obsidian"]["intake"], 0);
        assert_eq!(on_disk["obsidian"]["total"], 1);
        assert_eq!(on_disk["audio"]["seconds"], 99);
        let mut restarted = Counters::open(dir.path(), "2026-11-09").unwrap();
        assert!(restarted.apply_intakes(&[intake(0, "a.md")]));
        assert_eq!(restarted.note_count(), 1);
        assert!(restarted.apply_intakes(&[intake(1, "b.md")]));
        assert_eq!(restarted.note_count(), 2);
        assert_eq!(restarted.audio_seconds(), 99);
        drop(restarted);
        let again: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.path().join(FILE_NAME)).unwrap()).unwrap();
        assert_eq!(again["obsidian"]["intake"], 1);
        assert_eq!(again["obsidian"]["total"], 2);
        assert_eq!(again["audio"]["seconds"], 99);
    }

    #[test]
    fn an_intake_whose_id_is_zero_and_is_inventory_records_the_path_and_does_not_count() {
        let (dir, mut counters) = open_at("2026-11-09");
        audio(&mut counters, 1, 0, 99, 10);
        let recorded = NoteIntake {
            inventory: true,
            ..intake(0, "a.md")
        };
        assert!(counters.apply_intakes(std::slice::from_ref(&recorded)));
        assert_eq!(counters.note_count(), 0);
        assert_eq!(counters.last(Source::Obsidian), None);
        assert_eq!(counters.audio_seconds(), 99);
        assert!(counters.apply_intakes(&[recorded]));
        assert_eq!(counters.note_count(), 0);
        drop(counters);
        let on_disk: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.path().join(FILE_NAME)).unwrap()).unwrap();
        assert_eq!(on_disk["obsidian"]["intake"], 0);
        assert_eq!(on_disk["obsidian"]["total"], 0);
        assert_eq!(on_disk["audio"]["seconds"], 99);
        let mut restarted = Counters::open(dir.path(), "2026-11-09").unwrap();
        assert!(restarted.apply_intakes(&[intake(1, "a.md")]));
        assert_eq!(restarted.note_count(), 0);
        assert!(restarted.apply_intakes(&[intake(2, "b.md")]));
        assert_eq!(restarted.note_count(), 1);
        assert_eq!(restarted.audio_seconds(), 99);
        drop(restarted);
        let again: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.path().join(FILE_NAME)).unwrap()).unwrap();
        assert_eq!(again["obsidian"]["intake"], 2);
        assert_eq!(again["obsidian"]["total"], 1);
        assert_eq!(again["audio"]["seconds"], 99);
    }

    #[test]
    fn an_intake_whose_id_is_zero_and_is_a_removal_drops_the_hash_and_does_not_count() {
        let (dir, mut counters) = open_at("2026-11-09");
        audio(&mut counters, 1, 0, 99, 10);
        assert!(counters.apply_intakes(&[NoteIntake {
            inventory: true,
            ..intake(0, "a.md")
        }]));
        let removal = NoteIntake {
            removed: true,
            first_sight: false,
            ..intake(0, "a.md")
        };
        assert!(counters.apply_intakes(std::slice::from_ref(&removal)));
        assert_eq!(counters.note_count(), 0);
        assert_eq!(counters.last(Source::Obsidian), None);
        assert_eq!(counters.audio_seconds(), 99);
        assert!(counters.apply_intakes(&[removal]));
        assert_eq!(counters.note_count(), 0);
        drop(counters);
        let on_disk: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.path().join(FILE_NAME)).unwrap()).unwrap();
        assert_eq!(on_disk["obsidian"]["intake"], 0);
        assert_eq!(on_disk["obsidian"]["total"], 0);
        assert_eq!(on_disk["audio"]["seconds"], 99);
        let mut restarted = Counters::open(dir.path(), "2026-11-09").unwrap();
        assert!(restarted.apply_intakes(&[intake(1, "a.md")]));
        assert_eq!(restarted.note_count(), 1);
        assert_eq!(restarted.audio_seconds(), 99);
        drop(restarted);
        let again: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.path().join(FILE_NAME)).unwrap()).unwrap();
        assert_eq!(again["obsidian"]["intake"], 1);
        assert_eq!(again["obsidian"]["total"], 1);
        assert_eq!(again["audio"]["seconds"], 99);
    }

    #[test]
    fn a_placed_note_whose_save_never_landed_is_counted_once_on_restart() {
        let (dir, mut counters) = open_at("2026-11-09");
        assert!(counters.reconcile_notes(&["old.md".to_string()], &[]));
        counters.fail_next_saves(1);
        let placed = intake(1, "new.md");
        assert!(!counters.apply_intakes(std::slice::from_ref(&placed)));
        drop(counters);
        let mut restarted = Counters::open(dir.path(), "2026-11-09").unwrap();
        assert_eq!(restarted.note_count(), 0);
        assert!(restarted.apply_intakes(std::slice::from_ref(&placed)));
        assert_eq!(restarted.note_count(), 1);
        drop(restarted);
        let mut again = Counters::open(dir.path(), "2026-11-09").unwrap();
        assert!(again.apply_intakes(&[placed]));
        assert_eq!(again.note_count(), 1);
    }

    #[test]
    fn a_failed_place_records_nothing_and_a_later_place_counts() {
        let (_dir, mut counters) = open_at("2026-11-09");
        assert!(counters.reconcile_notes(&["old.md".to_string()], &[]));
        assert_eq!(counters.note_count(), 0);
        assert!(counters.apply_intakes(&[intake(1, "big.md")]));
        assert_eq!(counters.note_count(), 1);
    }

    #[test]
    fn removing_a_note_counts_nothing_and_a_later_note_at_that_path_can_count() {
        let (_dir, mut counters) = open_at("2026-11-09");
        assert!(counters.reconcile_notes(&["a.md".to_string()], &[]));
        let removal = NoteIntake {
            id: 1,
            removed: true,
            first_sight: false,
            at_secs: Some(80),
            ..intake(1, "a.md")
        };
        assert!(counters.apply_intakes(&[removal]));
        assert_eq!(counters.note_count(), 0);
        assert_eq!(counters.last(Source::Obsidian), None);
        assert!(counters.apply_intakes(&[intake(2, "a.md")]));
        assert_eq!(counters.note_count(), 1);
    }

    #[test]
    fn inventory_does_not_count_and_started_does_not_move() {
        let (dir, mut counters) = open_at("2026-11-09");
        let inventory = NoteIntake {
            inventory: true,
            ..intake(1, "a.md")
        };
        assert!(counters.reconcile_notes(&["a.md".to_string()], &[inventory]));
        assert_eq!(counters.note_count(), 0);
        assert_eq!(counters.started(), "2026-11-09");
        assert_eq!(counters.last(Source::Obsidian), None);
        audio(&mut counters, 1, 0, 1, 1_000);
        assert_eq!(counters.started(), "2026-11-09");
        drop(counters);
        let reloaded = Counters::open(dir.path(), "2030-01-01").unwrap();
        assert_eq!(reloaded.started(), "2026-11-09");
        assert_eq!(reloaded.last(Source::Audio), Some(1_000));
    }

    #[test]
    fn a_zero_credit_gap_does_not_move_last() {
        let (_dir, mut counters) = open_at("2026-11-09");
        audio(&mut counters, 1, 0, 10, 5);
        audio(&mut counters, 1, 2, 4, 500);
        assert_eq!(counters.last(Source::Audio), Some(5));
    }

    #[test]
    fn a_failed_save_keeps_both_credits_for_the_next_successful_write() {
        let (dir, mut counters) = open_at("2026-11-09");
        counters.fail_next_saves(1);
        audio(&mut counters, 1, 0, 10, 1);
        let on_disk: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.path().join(FILE_NAME)).unwrap()).unwrap();
        assert_eq!(on_disk["audio"]["seconds"], 0);
        assert!(counters.reconcile_pages(&[page("https://example.com/a", "tab", "b1", 2)]));
        drop(counters);
        let reloaded = Counters::open(dir.path(), "2026-11-09").unwrap();
        assert_eq!(reloaded.audio_seconds(), 10);
        assert_eq!(reloaded.page_count(), 1);
    }

    #[test]
    fn audio_and_a_page_share_one_file() {
        let dir = tempfile::tempdir().unwrap();
        let shared = SharedCounters::open(dir.path(), "2026-11-09").unwrap();
        shared.add_audio_span(1, 0, 10 * S, Some(1));
        assert!(shared.reconcile_pages(&[page("https://example.com/a", "tab", "b1", 2)]));
        drop(shared);
        let reloaded = Counters::open(dir.path(), "2026-11-09").unwrap();
        assert_eq!(reloaded.audio_seconds(), 10);
        assert_eq!(reloaded.page_count(), 1);
        assert_eq!(reloaded.path().file_name().unwrap(), "counters.json");
        assert!(!dir.path().join("setup.json").exists());
    }

    #[test]
    fn a_torn_temp_file_is_not_the_record() {
        let (dir, mut counters) = open_at("2026-11-09");
        audio(&mut counters, 1, 0, 10, 1);
        fs::write(tmp_path(counters.path()), b"{not-the-record").unwrap();
        drop(counters);
        let reloaded = Counters::open(dir.path(), "2026-11-09").unwrap();
        assert_eq!(reloaded.audio_seconds(), 10);
    }

    #[test]
    fn a_replace_interrupted_after_the_destination_is_gone_keeps_the_previous_totals() {
        let (dir, mut counters) = open_at("2026-11-09");
        audio(&mut counters, 1, 0, 10, 1);
        let dest = counters.path().to_path_buf();
        let bak = bak_path(&dest);
        drop(counters);
        fs::remove_file(&bak).unwrap();
        fs::rename(&dest, &bak).unwrap();
        fs::write(tmp_path(&dest), br#"{"v":1,"started":"1999-01-01","audio":{"seconds":99,"remainder_us":0,"last":1},"screen":{"seconds":0,"remainder_us":0,"last":null},"browsers":{"total":0,"last":null},"obsidian":{"total":0,"last":null,"baselined":false,"intake":0}}"#).unwrap();
        let reloaded = Counters::open(dir.path(), "2030-01-01").unwrap();
        assert_eq!(reloaded.audio_seconds(), 10);
        assert_eq!(reloaded.started(), "2026-11-09");
    }

    #[test]
    fn an_unreadable_file_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        fs::write(&path, b"{not json").unwrap();
        let error = Counters::open(dir.path(), "2026-11-09").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read(&path).unwrap(), b"{not json");
    }

    #[test]
    fn timeline_rows_become_spans_from_the_byte_rate() {
        let rate = pcm_byte_rate(16_000, 1, 16);
        let spans = spans_from_timeline("0 320000\n10000000 160000\n", rate);
        assert_eq!(spans, vec![(0, 10 * S), (10 * S, 15 * S)]);
        assert_eq!(calendar_date(0, 0), "1970-01-01");
        assert_eq!(calendar_date(86_400, 0), "1970-01-02");
    }

    #[test]
    fn the_window_read_does_not_create_or_promote_a_counter_file() {
        let dir = tempfile::tempdir().unwrap();
        let read = read_window_counters(dir.path()).unwrap();
        assert_eq!(read, WindowCounterRead::NoRows);
        assert!(!dir.path().join(FILE_NAME).exists());
        assert!(!bak_path(&dir.path().join(FILE_NAME)).exists());

        let bak = bak_path(&dir.path().join(FILE_NAME));
        let bak_bytes = br#"{"v":1,"started":"2026-11-09","audio":{"seconds":4,"remainder_us":0,"last":7},"screen":{"seconds":0,"remainder_us":0,"last":null},"browsers":{"total":0,"last":null},"obsidian":{"total":0,"last":null,"baselined":false,"intake":0}}"#;
        fs::write(&bak, bak_bytes).unwrap();
        assert_eq!(
            read_window_counters(dir.path()).unwrap(),
            WindowCounterRead::NoRows
        );
        assert_eq!(fs::read(&bak).unwrap(), bak_bytes);
        assert!(!dir.path().join(FILE_NAME).exists());

        let path = dir.path().join(FILE_NAME);
        fs::write(&path, b"{not json").unwrap();
        assert_eq!(
            read_window_counters(dir.path()).unwrap(),
            WindowCounterRead::NoRows
        );
        assert_eq!(fs::read(&path).unwrap(), b"{not json");

        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(read_window_counters(dir.path()).is_err());
        assert!(path.is_dir());
        assert_eq!(fs::read(&bak).unwrap(), bak_bytes);
    }

    #[test]
    fn the_window_read_returns_each_sources_stored_total_and_last() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = br#"{"v":1,"started":"2026-11-01","audio":{"seconds":0,"remainder_us":0,"last":20},"screen":{"seconds":3,"remainder_us":0,"last":99},"browsers":{"total":2,"last":5},"obsidian":{"total":0,"last":null,"baselined":false,"intake":0}}"#;
        fs::write(dir.path().join(FILE_NAME), bytes).unwrap();
        let WindowCounterRead::Document(doc) = read_window_counters(dir.path()).unwrap() else {
            panic!("expected the stored document");
        };
        assert_eq!(doc.started, "2026-11-01");
        assert_eq!(doc.audio.total, 0);
        assert_eq!(doc.audio.last, Some(20));
        assert_eq!(doc.screen.total, 3);
        assert_eq!(doc.screen.last, Some(99));
        assert_eq!(doc.browsers.total, 2);
        assert_eq!(doc.browsers.last, Some(5));
        assert_eq!(doc.obsidian.total, 0);
        assert_eq!(doc.obsidian.last, None);
        assert_eq!(fs::read(dir.path().join(FILE_NAME)).unwrap(), bytes);

        let positive = br#"{"v":1,"started":"2026-11-01","audio":{"seconds":15,"remainder_us":0,"last":8},"screen":{"seconds":0,"remainder_us":0,"last":null},"browsers":{"total":0,"last":null},"obsidian":{"total":0,"last":null,"baselined":false,"intake":0}}"#;
        fs::write(dir.path().join(FILE_NAME), positive).unwrap();
        let WindowCounterRead::Document(doc) = read_window_counters(dir.path()).unwrap() else {
            panic!("expected the stored document");
        };
        assert_eq!(doc.audio.total, 15);
        assert_eq!(fs::read(dir.path().join(FILE_NAME)).unwrap(), positive);
    }

    #[test]
    fn the_window_payload_uses_seconds_for_time_and_total_for_counts() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = br#"{"v":1,"started":"2026-11-01","audio":{"seconds":0,"remainder_us":0,"last":20},"screen":{"seconds":3,"remainder_us":0,"last":99},"browsers":{"total":2,"last":5},"obsidian":{"total":0,"last":null,"baselined":false,"intake":0}}"#;
        fs::write(dir.path().join(FILE_NAME), bytes).unwrap();
        let read = read_window_counters(dir.path()).unwrap();
        let value = serde_json::to_value(window_payload(&read)).unwrap();
        let audio = value.get("audio").unwrap();
        let browsers = value.get("browsers").unwrap();
        assert_eq!(audio.get("seconds").and_then(|v| v.as_u64()), Some(0));
        assert_eq!(audio.get("last").and_then(|v| v.as_i64()), Some(20));
        assert!(audio.get("total").is_none());
        assert_eq!(browsers.get("total").and_then(|v| v.as_u64()), Some(2));
        assert!(browsers.get("seconds").is_none());
        assert!(value.get("remainder_us").is_none());
        assert!(value.get("note_hashes").is_none());
        assert!(value.get("pages_applied").is_none());
        assert!(window_payload(&WindowCounterRead::NoRows).is_none());
        assert_eq!(fs::read(dir.path().join(FILE_NAME)).unwrap(), bytes);
    }

    #[test]
    fn a_screenshot_counts_once_by_its_hash_and_an_older_file_still_reads() {
        let (dir, mut counters) = open_at("2026-11-13");
        let hashes = vec!["aa".to_string(), "bb".to_string(), "aa".to_string()];
        assert!(counters.add_screenshots(&hashes, 100));
        assert!(counters.add_screenshots(&["bb".to_string()], 200));
        assert_eq!(counters.disk.screenshots.total, 2);
        assert_eq!(counters.last(Source::Screenshots), Some(100));
        let payload = window_payload(&read_window_counters(dir.path()).unwrap()).unwrap();
        assert_eq!(payload.screenshots.total, 2);

        // A file written before screenshots were counted still reads.
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.path().join(FILE_NAME)).unwrap()).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("screenshots");
        object.remove("screenshot_hashes");
        fs::write(
            dir.path().join(FILE_NAME),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        let payload = window_payload(&read_window_counters(dir.path()).unwrap()).unwrap();
        assert_eq!(payload.screenshots.total, 0);
        let mut reopened = Counters::open(dir.path(), "2026-11-13").unwrap();
        assert!(reopened.add_screenshots(&["aa".to_string()], 300));
        assert_eq!(reopened.disk.screenshots.total, 1);
    }
}
