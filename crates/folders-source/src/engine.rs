// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Stateful multi-folder engine: recursive complete-listing walks, sorting,
//! five-minute windows, note net-edit tracking, one-hour document revision
//! cool-down, rename matching (NTFS file ID + content hash), delete-before-send,
//! pause-without-skipping, protocol-v3 staging, and pairing-safe durable state
//! (`folders/ledger.json`).
//!
//! Zero journal identity is stored in `ledger.json`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use observer_pl::ingest::{FilePart, FolderMeta};
use screenshots_source::Span;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use solstone_folder_sync::files::{
    ListingError, is_placeholder, settled, sha256_hex, visit_directory,
};

use crate::classify::{
    OverlapOutcome, classify_file, detect_overlap, folder_label, folder_name, is_junk_entry,
    is_partial_download, join_rel_path, normalize_path, paths_equal, strip_verbatim_prefix,
    symlink_stays_inside,
};
use crate::note_diff::net_line_diff;
use crate::platform::FolderPlatform;
use crate::stamp::{Stamp, Window};
use crate::types::{
    ActivityVerb, BackfillEstimate, DeliveryMode, EstimateItem, FacetOption, FacetsAvailability,
    FileKind, FolderAttention, FolderDeclaration, FolderDeclaredState, FolderKinds, FolderMode,
    FolderSummary, FoldersDocument, MAX_CONTENT_BYTES, OverlapTarget, PathCheckResult,
    PlatformMetadata, PreviewSummary, SupportedList,
};
use crate::zone_identifier::{extension_lower, guess_mime, origin_domain, system_time_ms};

const LEDGER_FILE: &str = "ledger.json";
const LEDGER_VERSION: u32 = 1;
const SETTLE_MS: i64 = 2_000;
const TEXT_SAMPLE_LIMIT: u64 = 32 * 1024 * 1024;

const RETRY_MS: [i64; 4] = [30_000, 120_000, 600_000, 1_800_000];
/// Maximum companion content files per protocol-v3 folder segment upload
/// (64 files total including the activity JSONL).
pub const MAX_UPLOAD_CONTENT_PARTS: usize = 63;
/// Maximum preview items sent to `POST /app/devices/ingest/folder/estimate`.
pub const MAX_ESTIMATE_ITEMS: usize = 50_000;

/// Callback notified with `(event_id, event_unix_secs)` when a folder item is taken in.
pub type IntakeHook = Arc<dyn Fn(&str, i64) + Send + Sync>;

/// One event inside a ready five-minute folder segment.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PendingEvent {
    pub id: String,
    pub rel_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_path: Option<String>,
    pub verb: ActivityVerb,
    pub kind: FileKind,
    pub delivery: DeliveryMode,
    pub event_ms: i64,
    #[serde(default)]
    pub observed_ms: i64,
    pub offset_s: i32,
    pub window: Window,
    pub window_end_ms: i64,
    pub metadata: PlatformMetadata,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// Note text when `kind == FileKind::Note` and `delivery == DeliveryMode::Content`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_payload: Option<String>,
    #[serde(default)]
    pub note_is_diff: bool,
    #[serde(default)]
    pub is_revision: bool,
    #[serde(default)]
    pub backfill: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at_ms: Option<i64>,
    #[serde(default)]
    pub content_refused: bool,
    #[serde(default)]
    pub tries: u32,
}

/// Durable per-file record held per configured folder.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TrackedFile {
    pub rel_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
    pub size_bytes: u64,
    pub mtime_ns: i128,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    pub kind: FileKind,
    pub delivery: DeliveryMode,
    /// True once the journal acknowledged this file (so a later local deletion
    /// emits `removed` rather than silently dropping).
    #[serde(default)]
    pub delivered_to_journal: bool,
    /// True when the file existed before `span_from_ms` and was intentionally
    /// excluded by the owner's range choice.
    #[serde(default)]
    pub excluded_before_span: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sent_hash: Option<String>,
    /// Baseline note text last sent/acknowledged for computing net window diffs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_note_text: Option<String>,
    /// When an already-delivered document or transcript was last saved with a
    /// new hash, waiting for the one-hour cool-down before emitting a revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_revision_save_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_revision_hash: Option<String>,
}

/// Durable record for one configured folder.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FolderRecord {
    pub id: String,
    pub path: PathBuf,
    pub name: String,
    pub label: String,
    pub mode: FolderMode,
    #[serde(default)]
    pub returning: bool,
    #[serde(default)]
    held_paused: bool,
    #[serde(default)]
    pub kinds: FolderKinds,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facet: Option<FacetOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlap: Option<OverlapTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_from_ms: Option<i64>,
    #[serde(default)]
    pub folder_seen: bool,
    #[serde(default)]
    pub files: BTreeMap<String, TrackedFile>,
    #[serde(default)]
    pub pending: Vec<PendingEvent>,
    #[serde(default)]
    pub backfill_total: u64,
    #[serde(default)]
    pub backfill_done: u64,
    #[serde(default)]
    pub items_taken_in: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_taken_in_ms: Option<i64>,
    #[serde(default)]
    pub fallback_to_details: u64,
    #[serde(skip)]
    pub attention: Option<FolderAttention>,
    #[serde(skip)]
    pub last_checked_ms: Option<i64>,
    #[serde(skip)]
    pub cant_read: u64,
    #[serde(skip)]
    pub preview: Option<PreviewSummary>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
struct Ledger {
    v: u32,
    #[serde(default)]
    folders: Vec<FolderRecord>,
}

/// One staged five-minute segment ready to send to the journal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SegmentDelivery {
    pub folder_id: String,
    pub folder_name: String,
    pub folder_root: PathBuf,
    pub facet: Option<String>,
    pub kinds: FolderKinds,
    pub window: Window,
    pub events: Vec<PendingEvent>,
    pub content_files: Vec<StagedContentFile>,
}

/// One content file included in a `SegmentDelivery`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedContentFile {
    pub event_id: String,
    pub rel_path: String,
    pub disk_path: PathBuf,
    pub kind: FileKind,
    pub content_hash: Option<String>,
    pub note_payload: Option<String>,
    pub note_is_diff: bool,
    pub is_revision: bool,
    pub needs_image_convert: bool,
}

/// Outcome of delivering one five-minute segment to the journal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeliveryOutcome {
    /// Segment accepted (`ok`, `duplicate`, `collision`, or `segment_removed`).
    Delivered,
    /// Segment accepted (`200 ok`), but the journal refused content for specific
    /// relative paths. Re-queues those paths as details-only (`part: null`) and
    /// increments `fallback_to_details`.
    DeliveredWithRefusedContent(Vec<String>),
    /// Every item in the upload failed per-file validation (`400 folder_file_invalid`).
    /// Re-queues refused paths as details-only (`part: null`) and increments
    /// `fallback_to_details`.
    AllFilesRefused(Vec<String>),
    /// Permanent contract rejection (`400 folder_invalid`): drop malformed batch.
    InvalidPermanent,
    /// Transient failure, offline, `503 journal_read_failed`, or `400 folder_ahead`:
    /// retry with backoff.
    Hold,
}

/// Context of other configured sources used for overlap detection.
#[derive(Clone, Debug, Default)]
pub struct OverlapContext {
    pub obsidian: Option<(PathBuf, String)>,
    pub screenshots: Option<(PathBuf, String)>,
}

struct WalkedFile {
    rel_path: String,
    abs_path: PathBuf,
    file_id: Option<String>,
    size_bytes: u64,
    mtime_ns: i128,
    metadata: PlatformMetadata,
    is_screenshot: bool,
}

struct WalkOutcome {
    files: Vec<WalkedFile>,
    unreadable: u64,
    complete: bool,
}

pub struct Engine {
    state_dir: PathBuf,
    platform: Arc<dyn FolderPlatform>,
    ledger: Ledger,
    supported: SupportedList,
    intake_hook: Option<IntakeHook>,
}

impl Engine {
    pub fn open(state_dir: PathBuf, platform: Arc<dyn FolderPlatform>) -> Self {
        let ledger = load_ledger(&state_dir).unwrap_or_else(|| Ledger {
            v: LEDGER_VERSION,
            folders: Vec::new(),
        });
        Self {
            state_dir,
            platform,
            ledger,
            supported: SupportedList::default(),
            intake_hook: None,
        }
    }

    pub fn set_intake_hook(&mut self, hook: IntakeHook) {
        self.intake_hook = Some(hook);
    }

    pub fn set_supported_list(&mut self, supported: SupportedList) {
        if self.supported == supported {
            return;
        }
        // Entries confirmed offline must obey the negotiated kinds before upload.
        for folder in &mut self.ledger.folders {
            folder.pending.retain_mut(|ev| {
                if ev.verb == ActivityVerb::Removed || ev.content_refused {
                    return true;
                }
                let ext = ev.metadata.extension.as_deref();
                let sample = if needs_text_sample(ext, &supported)
                    && ev.metadata.size_bytes <= supported.max_content_bytes.min(TEXT_SAMPLE_LIMIT)
                {
                    let path = join_rel_path(&folder.path, &ev.rel_path);
                    self.platform
                        .read_file(
                            &folder.path,
                            &path,
                            supported.max_content_bytes.min(TEXT_SAMPLE_LIMIT),
                        )
                        .ok()
                } else {
                    None
                };
                let (kind, delivery) = classify_file(
                    ext,
                    ev.metadata.size_bytes,
                    ev.kind == FileKind::Screenshot,
                    sample.as_deref(),
                    folder.kinds,
                    &supported,
                );
                if kind != ev.kind || delivery != DeliveryMode::Content {
                    ev.note_payload = None;
                    ev.note_is_diff = false;
                }
                ev.kind = kind;
                ev.delivery = delivery;
                if let Some(tracked) = folder.files.get_mut(&ev.rel_path) {
                    tracked.kind = kind;
                    tracked.delivery = delivery;
                }
                delivery != DeliveryMode::Ignore
            });
        }
        self.supported = supported;
    }

    #[must_use]
    pub fn supported_list(&self) -> &SupportedList {
        &self.supported
    }

    #[must_use]
    pub fn folders(&self) -> &[FolderRecord] {
        &self.ledger.folders
    }

    #[must_use]
    pub fn platform(&self) -> &Arc<dyn FolderPlatform> {
        &self.platform
    }

    fn save(&self) -> io::Result<()> {
        fs::create_dir_all(&self.state_dir)?;
        let path = self.state_dir.join(LEDGER_FILE);
        let tmp = self.state_dir.join(format!("{LEDGER_FILE}.tmp"));
        let bak = self.state_dir.join(format!("{LEDGER_FILE}.bak"));
        let bytes = serde_json::to_vec_pretty(&self.ledger)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        {
            let mut file = File::create(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        if path.exists() {
            if bak.exists() {
                let _ = fs::remove_file(&bak);
            }
            let _ = fs::rename(&path, &bak);
        }
        fs::rename(&tmp, &path)
    }

    /// Check whether a candidate folder can be opened, already exists, or
    /// overlaps an Obsidian vault, screenshots folder, or another folder source.
    #[must_use]
    pub fn check_path(&self, raw_path: &Path, overlap_ctx: &OverlapContext) -> PathCheckResult {
        if !raw_path.is_dir() || fs::read_dir(raw_path).is_err() {
            return PathCheckResult::Refused {
                path: raw_path.display().to_string(),
            };
        }
        let canonical = normalize_path(raw_path);
        let name = folder_name(&canonical);
        let home = self.platform.home_dir();
        let label = folder_label(&canonical, &home);
        let existing: Vec<(&str, &Path, &str)> = self
            .ledger
            .folders
            .iter()
            .map(|f| (f.id.as_str(), f.path.as_path(), f.name.as_str()))
            .collect();
        let obs = overlap_ctx
            .obsidian
            .as_ref()
            .map(|(p, n)| (p.as_path(), n.as_str()));
        let shots = overlap_ctx
            .screenshots
            .as_ref()
            .map(|(p, n)| (p.as_path(), n.as_str()));
        match detect_overlap(&canonical, obs, shots, &existing) {
            OverlapOutcome::AlreadyAdded { id, name } => PathCheckResult::AlreadyAdded {
                folder_id: id,
                name,
            },
            OverlapOutcome::Overlap(target) => PathCheckResult::Overlaps {
                path: canonical.display().to_string(),
                name,
                label,
                target,
            },
            OverlapOutcome::None => PathCheckResult::Ready {
                path: canonical.display().to_string(),
                name,
                label,
            },
        }
    }

    /// Add a new folder in `SettingUp` mode (nothing leaves the device until
    /// `confirm_folder` is called). Returns the folder's id, or `Err` if the
    /// folder cannot be opened.
    pub fn begin_setup(
        &mut self,
        raw_path: &Path,
        overlap: Option<OverlapTarget>,
    ) -> Result<String, PathCheckResult> {
        if !raw_path.is_dir() || fs::read_dir(raw_path).is_err() {
            return Err(PathCheckResult::Refused {
                path: raw_path.display().to_string(),
            });
        }
        let canonical = normalize_path(raw_path);
        if let Some(existing) = self
            .ledger
            .folders
            .iter()
            .find(|f| paths_equal(&f.path, &canonical))
        {
            return Ok(existing.id.clone());
        }
        let name = folder_name(&canonical);
        let id = make_folder_id(&name, &canonical);
        let label = folder_label(&canonical, &self.platform.home_dir());
        let record = FolderRecord {
            id: id.clone(),
            path: canonical,
            name,
            label,
            mode: FolderMode::SettingUp,
            returning: false,
            held_paused: false,
            kinds: FolderKinds::default(),
            facet: None,
            overlap,
            span_from_ms: None,
            folder_seen: true,
            files: BTreeMap::new(),
            pending: Vec::new(),
            backfill_total: 0,
            backfill_done: 0,
            items_taken_in: 0,
            last_taken_in_ms: None,
            fallback_to_details: 0,
            attention: None,
            last_checked_ms: None,
            cant_read: 0,
            preview: None,
        };
        self.ledger.folders.push(record);
        let _ = self.save();
        Ok(id)
    }

    /// Repoint an existing folder whose directory moved or was replaced.
    pub fn repoint_folder(&mut self, folder_id: &str, new_path: &Path) -> Result<(), String> {
        if !new_path.is_dir() || fs::read_dir(new_path).is_err() {
            return Err("refused".into());
        }
        let canonical = normalize_path(new_path);
        let name = folder_name(&canonical);
        let label = folder_label(&canonical, &self.platform.home_dir());
        let Some(folder) = self.ledger.folders.iter_mut().find(|f| f.id == folder_id) else {
            return Err("not_found".into());
        };
        folder.path = canonical;
        folder.name = name;
        folder.label = label;
        folder.folder_seen = true;
        folder.attention = None;
        folder.preview = None;
        let _ = self.save();
        Ok(())
    }

    /// Update enabled kinds on a folder (during setup or while active; changes
    /// apply going forward and never remove what is already in the journal).
    pub fn set_kinds(&mut self, folder_id: &str, kinds: FolderKinds) -> bool {
        let Some(folder) = self.ledger.folders.iter_mut().find(|f| f.id == folder_id) else {
            return false;
        };
        folder.pending.retain_mut(|event| {
            if kinds.enabled_for(event.kind) {
                return true;
            }
            if !kinds.other {
                return false;
            }
            event.delivery = DeliveryMode::MetadataOnly;
            event.content_hash = None;
            event.note_payload = None;
            event.note_is_diff = false;
            true
        });
        for tracked in folder.files.values_mut() {
            if !kinds.enabled_for(tracked.kind) {
                tracked.delivery = if kinds.other {
                    DeliveryMode::MetadataOnly
                } else {
                    DeliveryMode::Ignore
                };
                tracked.pending_revision_save_ms = None;
                tracked.pending_revision_hash = None;
            }
        }
        folder.kinds = kinds;
        folder.preview = None;
        let _ = self.save();
        true
    }

    /// Set or clear the optional facet on a folder.
    pub fn set_facet(&mut self, folder_id: &str, facet: Option<FacetOption>) -> bool {
        let Some(folder) = self.ledger.folders.iter_mut().find(|f| f.id == folder_id) else {
            return false;
        };
        folder.facet = facet;
        let _ = self.save();
        true
    }

    /// Scan a folder (recursively) and compute preview counts, total bytes,
    /// date span, and local fallback work estimate without sending anything.
    pub fn preview_folder(&mut self, folder_id: &str, span: Span) -> Option<PreviewSummary> {
        self.preview_folder_with_items(folder_id, span)
            .map(|(s, _)| s)
    }

    /// Like `preview_folder`, also returning the capped `EstimateItem` list for
    /// `POST /app/devices/ingest/folder/estimate`.
    pub fn preview_folder_with_items(
        &mut self,
        folder_id: &str,
        span: Span,
    ) -> Option<(PreviewSummary, Vec<EstimateItem>)> {
        let idx = self.ledger.folders.iter().position(|f| f.id == folder_id)?;
        let root = self.ledger.folders[idx].path.clone();
        let kinds = self.ledger.folders[idx].kinds;
        let already_delivered: BTreeSet<String> = self.ledger.folders[idx]
            .files
            .iter()
            .filter(|(_, file)| file.delivered_to_journal)
            .map(|(path, _)| path.clone())
            .collect();

        let stamp = self.platform.now_stamp();
        let from_ms = span_start_ms(span, stamp, |seconds| self.platform.offset_s_at(seconds));
        let walk = match walk_folder(&root, &*self.platform) {
            Ok(w) => w,
            Err(_) => {
                let summary = PreviewSummary {
                    span: Some(span),
                    failed: true,
                    ..PreviewSummary::default()
                };
                self.ledger.folders[idx].preview = Some(summary.clone());
                return Some((summary, Vec::new()));
            }
        };

        let mut summary = PreviewSummary {
            span: Some(span),
            unreadable: walk.unreadable,
            first_date: (span != Span::All)
                .then(|| self.platform.stamp_at(from_ms).rfc3339()[..10].to_owned()),
            ..PreviewSummary::default()
        };
        let mut windows: BTreeSet<Window> = BTreeSet::new();
        let mut days: BTreeSet<String> = BTreeSet::new();
        let mut content_thoughts: u64 = 0;
        let mut estimate_items: Vec<EstimateItem> = Vec::new();

        for file in walk.files {
            if file.metadata.modified_ms < from_ms {
                continue;
            }
            if already_delivered.contains(&file.rel_path) {
                continue;
            }
            let note_bytes =
                if needs_text_sample(file.metadata.extension.as_deref(), &self.supported)
                    && file.size_bytes <= self.supported.max_content_bytes.min(TEXT_SAMPLE_LIMIT)
                {
                    self.platform
                        .read_file(
                            &root,
                            &file.abs_path,
                            self.supported.max_content_bytes.min(TEXT_SAMPLE_LIMIT),
                        )
                        .ok()
                } else {
                    None
                };
            let (kind, delivery) = classify_file(
                file.metadata.extension.as_deref(),
                file.size_bytes,
                file.is_screenshot,
                note_bytes.as_deref(),
                kinds,
                &self.supported,
            );
            if delivery == DeliveryMode::Ignore {
                continue;
            }
            summary.total_items += 1;
            match delivery {
                DeliveryMode::Content => {
                    summary.content_items += 1;
                    summary.content_bytes = summary.content_bytes.saturating_add(file.size_bytes);
                    content_thoughts += 1;
                    match kind {
                        FileKind::Note => summary.notes += 1,
                        FileKind::Document => summary.documents += 1,
                        FileKind::Transcript => summary.transcripts += 1,
                        FileKind::Screenshot => summary.screenshots += 1,
                        FileKind::Photo => summary.photos += 1,
                        FileKind::Other => summary.other += 1,
                    }
                }
                DeliveryMode::MetadataOnly => {
                    summary.metadata_only_items += 1;
                    summary.other += 1;
                }
                DeliveryMode::Ignore => {}
            }
            let clamped_ms = file.metadata.modified_ms.min(stamp.ms);
            summary.oldest_ms = Some(summary.oldest_ms.map_or(clamped_ms, |o| o.min(clamped_ms)));
            summary.newest_ms = Some(summary.newest_ms.map_or(clamped_ms, |n| n.max(clamped_ms)));
            let ev_stamp = self.platform.stamp_at(clamped_ms);
            let w = ev_stamp.window();
            days.insert(w.day.clone());
            windows.insert(w);

            if estimate_items.len() < MAX_ESTIMATE_ITEMS {
                let has_content = delivery == DeliveryMode::Content;
                let content_hash = if has_content {
                    note_bytes.as_deref().map(sha256_hex)
                } else {
                    None
                };
                estimate_items.push(EstimateItem {
                    path: file.rel_path,
                    at: ev_stamp.rfc3339(),
                    file_type: file.metadata.extension.filter(|ft| {
                        !ft.is_empty() && ft.len() <= 64 && !ft.chars().any(char::is_control)
                    }),
                    kind_hint: if has_content {
                        kind.kind_hint().map(str::to_owned)
                    } else {
                        None
                    },
                    content_hash,
                    has_content,
                });
            }
        }

        summary.estimate = Some(BackfillEstimate {
            segments: windows.len() as u64,
            days: days.len() as u64,
            thoughts: content_thoughts,
            already_in_journal: 0,
            from_journal: false,
        });
        self.ledger.folders[idx].preview = Some(summary.clone());
        Some((summary, estimate_items))
    }

    /// Replace the cached preview estimate with the journal's own estimate.
    pub fn set_journal_estimate(&mut self, folder_id: &str, estimate: BackfillEstimate) {
        if let Some(folder) = self.ledger.folders.iter_mut().find(|f| f.id == folder_id)
            && let Some(preview) = folder.preview.as_mut()
        {
            preview.estimate = Some(estimate);
        }
    }

    /// Confirm setup (or "bring in older files") for one folder. Transitions
    /// the folder to `On` and enqueues eligible existing files within `span`
    /// without re-sending items already delivered to this journal.
    pub fn confirm_folder(&mut self, folder_id: &str, span: Span) -> Result<(), String> {
        let idx = self
            .ledger
            .folders
            .iter()
            .position(|f| f.id == folder_id)
            .ok_or_else(|| "not_found".to_owned())?;
        if !self.ledger.folders[idx].kinds.any_selected() {
            return Err("no_kinds_selected".into());
        }
        let stamp = self.platform.now_stamp();
        let from_ms = span_start_ms(span, stamp, |seconds| self.platform.offset_s_at(seconds));
        let root = self.ledger.folders[idx].path.clone();
        let kinds = self.ledger.folders[idx].kinds;
        let walk = walk_folder(&root, &*self.platform).map_err(|_| "unreadable".to_owned())?;

        let folder = &mut self.ledger.folders[idx];
        folder.mode = if folder.held_paused {
            FolderMode::Paused
        } else {
            FolderMode::On
        };
        folder.held_paused = false;
        folder.returning = true;
        folder.folder_seen = true;
        folder.attention = None;
        folder.last_checked_ms = Some(stamp.ms);
        folder.cant_read = walk.unreadable;
        folder.span_from_ms = Some(match folder.span_from_ms {
            Some(prev) => prev.min(from_ms),
            None => from_ms,
        });

        let held: BTreeSet<String> = folder
            .pending
            .iter()
            .map(|ev| ev.rel_path.clone())
            .collect();
        let mut queued_now = 0u64;
        for file in walk.files {
            if held.contains(&file.rel_path) {
                continue;
            }
            if folder.files.get(&file.rel_path).is_some_and(|existing| {
                existing.delivered_to_journal || existing.pending_revision_save_ms.is_some()
            }) {
                continue;
            }
            if file.metadata.modified_ms < from_ms {
                folder.files.insert(
                    file.rel_path.clone(),
                    TrackedFile {
                        rel_path: file.rel_path,
                        file_id: file.file_id,
                        size_bytes: file.size_bytes,
                        mtime_ns: file.mtime_ns,
                        content_hash: None,
                        kind: FileKind::Other,
                        delivery: DeliveryMode::Ignore,
                        delivered_to_journal: false,
                        excluded_before_span: true,
                        last_sent_hash: None,
                        last_note_text: None,
                        pending_revision_save_ms: None,
                        pending_revision_hash: None,
                    },
                );
                continue;
            }

            let note_bytes =
                if needs_text_sample(file.metadata.extension.as_deref(), &self.supported)
                    && file.size_bytes <= self.supported.max_content_bytes.min(TEXT_SAMPLE_LIMIT)
                {
                    self.platform
                        .read_file(
                            &root,
                            &file.abs_path,
                            self.supported.max_content_bytes.min(TEXT_SAMPLE_LIMIT),
                        )
                        .ok()
                } else {
                    None
                };
            let (kind, delivery) = classify_file(
                file.metadata.extension.as_deref(),
                file.size_bytes,
                file.is_screenshot,
                note_bytes.as_deref(),
                kinds,
                &self.supported,
            );
            if delivery == DeliveryMode::Ignore {
                continue;
            }

            let content_hash = if delivery == DeliveryMode::Content {
                if let Some(ref nb) = note_bytes {
                    Some(sha256_hex(nb))
                } else {
                    self.platform
                        .read_file(&root, &file.abs_path, self.supported.max_content_bytes)
                        .map(|b| sha256_hex(&b))
                        .ok()
                }
            } else {
                None
            };
            let note_text = if kind == FileKind::Note && delivery == DeliveryMode::Content {
                note_bytes
                    .as_deref()
                    .and_then(|b| std::str::from_utf8(b).ok())
                    .map(str::to_owned)
            } else {
                None
            };

            let ev_ms = file.metadata.modified_ms.min(stamp.ms);
            let ev_stamp = self.platform.stamp_at(ev_ms);
            let window = ev_stamp.window();
            let window_end_ms = ev_stamp.window_end_ms();
            let is_backfill = window_end_ms <= stamp.ms;
            let event_id = event_id_for(&folder.id, &file.rel_path, ActivityVerb::Added, ev_ms);

            folder.pending.retain(|p| p.rel_path != file.rel_path);
            folder.pending.push(PendingEvent {
                id: event_id,
                rel_path: file.rel_path.clone(),
                previous_path: None,
                verb: ActivityVerb::Added,
                kind,
                delivery,
                event_ms: ev_ms,
                observed_ms: stamp.ms,
                offset_s: ev_stamp.offset_s,
                window,
                window_end_ms,
                metadata: file.metadata,
                content_hash: content_hash.clone(),
                note_payload: note_text,
                note_is_diff: false,
                is_revision: false,
                backfill: is_backfill,
                retry_at_ms: None,
                content_refused: false,
                tries: 0,
            });
            folder.files.insert(
                file.rel_path.clone(),
                TrackedFile {
                    rel_path: file.rel_path,
                    file_id: file.file_id,
                    size_bytes: file.size_bytes,
                    mtime_ns: file.mtime_ns,
                    content_hash,
                    kind,
                    delivery,
                    delivered_to_journal: false,
                    excluded_before_span: false,
                    last_sent_hash: None,
                    last_note_text: None,
                    pending_revision_save_ms: None,
                    pending_revision_hash: None,
                },
            );
            queued_now += 1;
        }

        if queued_now > 0 {
            folder.backfill_total = folder
                .backfill_done
                .saturating_add(folder.pending.iter().filter(|ev| ev.backfill).count() as u64);
        }
        let _ = self.save();
        Ok(())
    }

    /// Pause syncing for one folder. Scanning continues so no changes are skipped;
    /// delivery waits until `resume_folder` is called.
    pub fn pause_folder(&mut self, folder_id: &str) -> bool {
        let Some(folder) = self.ledger.folders.iter_mut().find(|f| f.id == folder_id) else {
            return false;
        };
        if folder.mode == FolderMode::On {
            folder.mode = FolderMode::Paused;
            let _ = self.save();
            return true;
        }
        false
    }

    /// Resume syncing for one paused folder. Queued changes deliver immediately.
    pub fn resume_folder(&mut self, folder_id: &str) -> bool {
        let Some(folder) = self.ledger.folders.iter_mut().find(|f| f.id == folder_id) else {
            return false;
        };
        if folder.mode == FolderMode::Paused {
            folder.mode = FolderMode::On;
            let _ = self.save();
            return true;
        }
        false
    }

    /// Turn a folder off. Stops watching and drops unsent queue entries;
    /// already-delivered journal records stay intact so re-enabling won't duplicate.
    pub fn turn_off_folder(&mut self, folder_id: &str) -> bool {
        let Some(folder) = self.ledger.folders.iter_mut().find(|f| f.id == folder_id) else {
            return false;
        };
        folder.mode = FolderMode::Off;
        folder.held_paused = false;
        folder.pending.clear();
        for tracked in folder.files.values_mut() {
            tracked.pending_revision_save_ms = None;
            tracked.pending_revision_hash = None;
        }
        folder.backfill_total = 0;
        folder.backfill_done = 0;
        folder.attention = None;
        let _ = self.save();
        true
    }

    /// Start turning a previously off folder back on (enters `SettingUp` with
    /// `returning = true` so the owner picks how far back to catch up).
    pub fn turn_on_folder(&mut self, folder_id: &str) -> bool {
        let Some(folder) = self.ledger.folders.iter_mut().find(|f| f.id == folder_id) else {
            return false;
        };
        folder.mode = FolderMode::SettingUp;
        folder.returning = true;
        let _ = self.save();
        true
    }

    /// Remove a folder from the app completely (nothing changes on disk or in
    /// the journal).
    pub fn remove_folder(&mut self, folder_id: &str) -> bool {
        let before = self.ledger.folders.len();
        self.ledger.folders.retain(|f| f.id != folder_id);
        let removed = self.ledger.folders.len() != before;
        if removed {
            let _ = self.save();
        }
        removed
    }

    /// Pairing principle 9: a new journal pairing restarts first setup on all
    /// active/paused folders while keeping the owner's granted folders, kinds,
    /// facet choice and previously confirmed work for the next marked journal.
    pub fn restart_setup(&mut self) {
        for folder in &mut self.ledger.folders {
            if matches!(folder.mode, FolderMode::On | FolderMode::Paused) {
                folder.held_paused = folder.mode == FolderMode::Paused;
                folder.mode = FolderMode::SettingUp;
                folder.returning = true;
            }
            for event in &mut folder.pending {
                event.tries = 0;
                event.retry_at_ms = None;
            }
            folder.span_from_ms = None;
            folder.preview = None;
            for tracked in folder.files.values_mut() {
                tracked.delivered_to_journal = false;
                tracked.last_sent_hash = None;
            }
        }
        let _ = self.save();
    }

    /// Scan every `On` and `Paused` folder on disk, reconciling additions,
    /// edits, renames, one-hour document revisions, and delete-before-send.
    /// `Paused` folders STILL scan so changes are never skipped — only delivery
    /// waits until resume.
    pub fn scan_all(&mut self) {
        let now_ms = self.platform.now_ms();
        let mut dirty = false;

        for idx in 0..self.ledger.folders.len() {
            let mode = self.ledger.folders[idx].mode;
            if matches!(mode, FolderMode::On | FolderMode::Paused) {
                dirty |= self.scan_one(idx, now_ms);
            }
            if matches!(
                mode,
                FolderMode::On | FolderMode::Paused | FolderMode::SettingUp
            ) {
                dirty |= self.enqueue_due_revisions(idx, now_ms);
            }
        }
        if dirty {
            let _ = self.save();
        }
    }

    fn scan_one(&mut self, idx: usize, now_ms: i64) -> bool {
        let root = self.ledger.folders[idx].path.clone();
        let kinds = self.ledger.folders[idx].kinds;
        let walk = match walk_folder(&root, &*self.platform) {
            Ok(w) => {
                self.ledger.folders[idx].attention = None;
                self.ledger.folders[idx].folder_seen = true;
                self.ledger.folders[idx].last_checked_ms = Some(now_ms);
                self.ledger.folders[idx].cant_read = w.unreadable;
                w
            }
            Err(ListingError::Missing) => {
                self.ledger.folders[idx].attention = Some(FolderAttention::Missing);
                self.ledger.folders[idx].last_checked_ms = Some(now_ms);
                return false;
            }
            Err(ListingError::Incomplete(_)) => {
                self.ledger.folders[idx].attention = Some(FolderAttention::Unreadable);
                self.ledger.folders[idx].last_checked_ms = Some(now_ms);
                return false;
            }
        };

        let mut changed = false;
        let mut seen_paths: BTreeSet<String> = BTreeSet::new();
        let mut new_candidates: Vec<WalkedFile> = Vec::new();
        let mut modified_candidates: Vec<WalkedFile> = Vec::new();

        for file in walk.files {
            seen_paths.insert(file.rel_path.clone());
            if !settled(now_ms, file.metadata.modified_ms, SETTLE_MS) {
                continue;
            }
            match self.ledger.folders[idx].files.get(&file.rel_path) {
                None => new_candidates.push(file),
                Some(tracked) => {
                    if tracked.size_bytes != file.size_bytes || tracked.mtime_ns != file.mtime_ns {
                        modified_candidates.push(file);
                    }
                }
            }
        }

        // Only a complete listing may treat disappeared paths as renames or
        // deletions. Unrelated partial downloads (e.g. a leftover `.crdownload`
        // in Downloads) are skipped individually and must never suppress rename
        // or delete-before-send reconciliation for the rest of the folder.
        let disappeared_paths: Vec<String> = if walk.complete {
            self.ledger.folders[idx]
                .files
                .keys()
                .filter(|rel| !seen_paths.contains(*rel))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };

        // Match renames first: same stable file_id OR same (size, sha256).
        let mut matched_disappeared: BTreeSet<String> = BTreeSet::new();
        let mut remaining_new: Vec<WalkedFile> = Vec::new();

        let disappeared_sizes: BTreeSet<u64> = disappeared_paths
            .iter()
            .filter_map(|p| self.ledger.folders[idx].files.get(p).map(|t| t.size_bytes))
            .collect();

        for cand in new_candidates {
            let mut matched_old: Option<String> = None;
            // 1. Stable file ID match (NTFS/ReFS file_id:creation_time)
            if let Some(ref cand_id) = cand.file_id {
                for old_rel in &disappeared_paths {
                    if matched_disappeared.contains(old_rel) {
                        continue;
                    }
                    if self.ledger.folders[idx]
                        .files
                        .get(old_rel)
                        .and_then(|old| old.file_id.as_deref())
                        == Some(cand_id.as_str())
                    {
                        matched_old = Some(old_rel.clone());
                        break;
                    }
                }
            }
            // 2. Content hash match (when size matches and old had a content hash)
            if matched_old.is_none()
                && disappeared_sizes.contains(&cand.size_bytes)
                && let Ok(cand_hash) = self
                    .platform
                    .read_file(&root, &cand.abs_path, self.supported.max_content_bytes)
                    .map(|b| sha256_hex(&b))
            {
                for old_rel in &disappeared_paths {
                    if matched_disappeared.contains(old_rel) {
                        continue;
                    }
                    if self.ledger.folders[idx]
                        .files
                        .get(old_rel)
                        .is_some_and(|old| {
                            old.size_bytes == cand.size_bytes
                                && old.content_hash.as_deref() == Some(cand_hash.as_str())
                        })
                    {
                        matched_old = Some(old_rel.clone());
                        break;
                    }
                }
            }

            if let Some(old_rel) = matched_old {
                matched_disappeared.insert(old_rel.clone());
                if let Some(mut old_tracked) = self.ledger.folders[idx].files.remove(&old_rel) {
                    let was_delivered = old_tracked.delivered_to_journal;
                    let folder_id = self.ledger.folders[idx].id.clone();
                    old_tracked.rel_path = cand.rel_path.clone();
                    old_tracked.file_id = cand.file_id;
                    old_tracked.size_bytes = cand.size_bytes;
                    old_tracked.mtime_ns = cand.mtime_ns;

                    if !was_delivered {
                        for ev in &mut self.ledger.folders[idx].pending {
                            if ev.rel_path == old_rel {
                                ev.rel_path = cand.rel_path.clone();
                            }
                        }
                    } else {
                        let ev_stamp = self.platform.stamp_at(now_ms);
                        let event_id =
                            event_id_for(&folder_id, &cand.rel_path, ActivityVerb::Renamed, now_ms);
                        self.ledger.folders[idx].pending.push(PendingEvent {
                            id: event_id,
                            rel_path: cand.rel_path.clone(),
                            previous_path: Some(old_rel),
                            verb: ActivityVerb::Renamed,
                            kind: old_tracked.kind,
                            delivery: DeliveryMode::MetadataOnly,
                            event_ms: now_ms,
                            observed_ms: now_ms,
                            offset_s: ev_stamp.offset_s,
                            window: ev_stamp.window(),
                            window_end_ms: ev_stamp.window_end_ms(),
                            metadata: cand.metadata,
                            content_hash: old_tracked.content_hash.clone(),
                            note_payload: None,
                            note_is_diff: false,
                            is_revision: false,
                            backfill: false,
                            retry_at_ms: None,
                            content_refused: false,
                            tries: 0,
                        });
                    }
                    self.ledger.folders[idx]
                        .files
                        .insert(cand.rel_path, old_tracked);
                    changed = true;
                }
            } else {
                remaining_new.push(cand);
            }
        }

        // Delete-before-send & removed events on unmatched disappeared paths
        // (runs ONLY when `walk.complete && walk.deferred_partials == 0`).
        if walk.complete {
            let folder_id = self.ledger.folders[idx].id.clone();
            for old_rel in disappeared_paths {
                if matched_disappeared.contains(&old_rel) {
                    continue;
                }
                let Some(old) = self.ledger.folders[idx].files.remove(&old_rel) else {
                    continue;
                };
                self.ledger.folders[idx]
                    .pending
                    .retain(|ev| ev.rel_path != old_rel);
                if !old.delivered_to_journal {
                    changed = true;
                } else if !old.excluded_before_span {
                    let ev_stamp = self.platform.stamp_at(now_ms);
                    let event_id =
                        event_id_for(&folder_id, &old_rel, ActivityVerb::Removed, now_ms);
                    self.ledger.folders[idx].pending.push(PendingEvent {
                        id: event_id,
                        rel_path: old_rel.clone(),
                        previous_path: None,
                        verb: ActivityVerb::Removed,
                        kind: old.kind,
                        delivery: DeliveryMode::MetadataOnly,
                        event_ms: now_ms,
                        observed_ms: now_ms,
                        offset_s: ev_stamp.offset_s,
                        window: ev_stamp.window(),
                        window_end_ms: ev_stamp.window_end_ms(),
                        metadata: PlatformMetadata {
                            size_bytes: old.size_bytes,
                            modified_ms: now_ms,
                            created_ms: None,
                            extension: extension_lower(Path::new(&old_rel)),
                            mime: None,
                            origin_url: None,
                            referrer_url: None,
                            origin_domain: None,
                            zone_id: None,
                        },
                        content_hash: None,
                        note_payload: None,
                        note_is_diff: false,
                        is_revision: false,
                        backfill: false,
                        retry_at_ms: None,
                        content_refused: false,
                        tries: 0,
                    });
                    changed = true;
                }
            }
        }

        // Newly appeared files.
        let folder_id = self.ledger.folders[idx].id.clone();
        for file in remaining_new {
            let note_bytes =
                if needs_text_sample(file.metadata.extension.as_deref(), &self.supported)
                    && file.size_bytes <= self.supported.max_content_bytes.min(TEXT_SAMPLE_LIMIT)
                {
                    self.platform
                        .read_file(
                            &root,
                            &file.abs_path,
                            self.supported.max_content_bytes.min(TEXT_SAMPLE_LIMIT),
                        )
                        .ok()
                } else {
                    None
                };
            let (kind, delivery) = classify_file(
                file.metadata.extension.as_deref(),
                file.size_bytes,
                file.is_screenshot,
                note_bytes.as_deref(),
                kinds,
                &self.supported,
            );
            if delivery == DeliveryMode::Ignore {
                continue;
            }
            let content_hash = if delivery == DeliveryMode::Content {
                if let Some(ref nb) = note_bytes {
                    Some(sha256_hex(nb))
                } else {
                    self.platform
                        .read_file(&root, &file.abs_path, self.supported.max_content_bytes)
                        .map(|b| sha256_hex(&b))
                        .ok()
                }
            } else {
                None
            };
            let note_text = if kind == FileKind::Note && delivery == DeliveryMode::Content {
                note_bytes
                    .as_deref()
                    .and_then(|b| std::str::from_utf8(b).ok())
                    .map(str::to_owned)
            } else {
                None
            };
            let ev_ms = file.metadata.modified_ms.min(now_ms);
            let ev_stamp = self.platform.stamp_at(ev_ms);
            let event_id = event_id_for(&folder_id, &file.rel_path, ActivityVerb::Added, ev_ms);

            self.ledger.folders[idx].pending.push(PendingEvent {
                id: event_id,
                rel_path: file.rel_path.clone(),
                previous_path: None,
                verb: ActivityVerb::Added,
                kind,
                delivery,
                event_ms: ev_ms,
                observed_ms: now_ms,
                offset_s: ev_stamp.offset_s,
                window: ev_stamp.window(),
                window_end_ms: ev_stamp.window_end_ms(),
                metadata: file.metadata,
                content_hash: content_hash.clone(),
                note_payload: note_text,
                note_is_diff: false,
                is_revision: false,
                backfill: false,
                retry_at_ms: None,
                content_refused: false,
                tries: 0,
            });
            self.ledger.folders[idx].files.insert(
                file.rel_path.clone(),
                TrackedFile {
                    rel_path: file.rel_path,
                    file_id: file.file_id,
                    size_bytes: file.size_bytes,
                    mtime_ns: file.mtime_ns,
                    content_hash,
                    kind,
                    delivery,
                    delivered_to_journal: false,
                    excluded_before_span: false,
                    last_sent_hash: None,
                    last_note_text: None,
                    pending_revision_save_ms: None,
                    pending_revision_hash: None,
                },
            );
            changed = true;
        }

        // Modified files: note edits coalesce in the 5-minute window; already-delivered
        // documents/transcripts enter the one-hour revision cool-down.
        for file in modified_candidates {
            let note_bytes =
                if needs_text_sample(file.metadata.extension.as_deref(), &self.supported)
                    && file.size_bytes <= self.supported.max_content_bytes.min(TEXT_SAMPLE_LIMIT)
                {
                    self.platform
                        .read_file(
                            &root,
                            &file.abs_path,
                            self.supported.max_content_bytes.min(TEXT_SAMPLE_LIMIT),
                        )
                        .ok()
                } else {
                    None
                };
            let (kind, delivery) = classify_file(
                file.metadata.extension.as_deref(),
                file.size_bytes,
                file.is_screenshot,
                note_bytes.as_deref(),
                kinds,
                &self.supported,
            );
            if delivery == DeliveryMode::Ignore {
                continue;
            }
            let new_hash = if delivery == DeliveryMode::Content {
                if let Some(ref nb) = note_bytes {
                    Some(sha256_hex(nb))
                } else {
                    self.platform
                        .read_file(&root, &file.abs_path, self.supported.max_content_bytes)
                        .map(|b| sha256_hex(&b))
                        .ok()
                }
            } else {
                None
            };

            let Some(tracked) = self.ledger.folders[idx].files.get_mut(&file.rel_path) else {
                continue;
            };
            tracked.file_id = file.file_id;
            tracked.size_bytes = file.size_bytes;
            tracked.mtime_ns = file.mtime_ns;
            tracked.kind = kind;
            tracked.delivery = delivery;
            tracked.excluded_before_span = false;

            if new_hash.is_some() && new_hash == tracked.content_hash {
                continue;
            }
            tracked.content_hash = new_hash.clone();

            let ev_ms = file.metadata.modified_ms.min(now_ms);
            let ev_stamp = self.platform.stamp_at(ev_ms);
            let ev_window = ev_stamp.window();
            let ev_window_end = ev_stamp.window_end_ms();

            match (kind, delivery, tracked.delivered_to_journal) {
                (FileKind::Note, DeliveryMode::Content, _) => {
                    let current_text = note_bytes
                        .as_deref()
                        .and_then(|b| std::str::from_utf8(b).ok())
                        .map(str::to_owned);
                    let (verb, note_payload, note_is_diff) =
                        match (tracked.last_note_text.as_deref(), current_text.as_deref()) {
                            (Some(baseline), Some(after)) => {
                                if net_line_diff(baseline, after).is_none() {
                                    self.ledger.folders[idx].pending.retain(|ev| {
                                        !(ev.rel_path == file.rel_path && ev.window == ev_window)
                                    });
                                    changed = true;
                                    continue;
                                }
                                (ActivityVerb::Edited, Some(after.to_owned()), false)
                            }
                            (_, Some(whole)) => {
                                (ActivityVerb::Added, Some(whole.to_owned()), false)
                            }
                            _ => (ActivityVerb::Edited, None, false),
                        };
                    self.ledger.folders[idx]
                        .pending
                        .retain(|ev| !(ev.rel_path == file.rel_path && ev.window == ev_window));
                    self.ledger.folders[idx].pending.push(PendingEvent {
                        id: event_id_for(&folder_id, &file.rel_path, verb, ev_ms),
                        rel_path: file.rel_path.clone(),
                        previous_path: None,
                        verb,
                        kind: FileKind::Note,
                        delivery: DeliveryMode::Content,
                        event_ms: ev_ms,
                        observed_ms: now_ms,
                        offset_s: ev_stamp.offset_s,
                        window: ev_window,
                        window_end_ms: ev_window_end,
                        metadata: file.metadata,
                        content_hash: new_hash,
                        note_payload,
                        note_is_diff,
                        is_revision: false,
                        backfill: false,
                        retry_at_ms: None,
                        content_refused: false,
                        tries: 0,
                    });
                    changed = true;
                }
                (FileKind::Document | FileKind::Transcript, DeliveryMode::Content, true) => {
                    if new_hash.as_deref() == tracked.last_sent_hash.as_deref() {
                        tracked.pending_revision_save_ms = None;
                        tracked.pending_revision_hash = None;
                    } else {
                        tracked.pending_revision_save_ms = Some(ev_ms);
                        tracked.pending_revision_hash = new_hash;
                    }
                    changed = true;
                }
                (FileKind::Document | FileKind::Transcript, DeliveryMode::Content, false) => {
                    self.ledger.folders[idx]
                        .pending
                        .retain(|ev| ev.rel_path != file.rel_path);
                    self.ledger.folders[idx].pending.push(PendingEvent {
                        id: event_id_for(&folder_id, &file.rel_path, ActivityVerb::Added, ev_ms),
                        rel_path: file.rel_path.clone(),
                        previous_path: None,
                        verb: ActivityVerb::Added,
                        kind,
                        delivery: DeliveryMode::Content,
                        event_ms: ev_ms,
                        observed_ms: now_ms,
                        offset_s: ev_stamp.offset_s,
                        window: ev_window,
                        window_end_ms: ev_window_end,
                        metadata: file.metadata,
                        content_hash: new_hash,
                        note_payload: None,
                        note_is_diff: false,
                        is_revision: false,
                        backfill: false,
                        retry_at_ms: None,
                        content_refused: false,
                        tries: 0,
                    });
                    changed = true;
                }
                (_, DeliveryMode::MetadataOnly, _) => {
                    self.ledger.folders[idx]
                        .pending
                        .retain(|ev| !(ev.rel_path == file.rel_path && ev.window == ev_window));
                    self.ledger.folders[idx].pending.push(PendingEvent {
                        id: event_id_for(&folder_id, &file.rel_path, ActivityVerb::Edited, ev_ms),
                        rel_path: file.rel_path.clone(),
                        previous_path: None,
                        verb: ActivityVerb::Edited,
                        kind,
                        delivery: DeliveryMode::MetadataOnly,
                        event_ms: ev_ms,
                        observed_ms: now_ms,
                        offset_s: ev_stamp.offset_s,
                        window: ev_window,
                        window_end_ms: ev_window_end,
                        metadata: file.metadata,
                        content_hash: None,
                        note_payload: None,
                        note_is_diff: false,
                        is_revision: false,
                        backfill: false,
                        retry_at_ms: None,
                        content_refused: false,
                        tries: 0,
                    });
                    changed = true;
                }
                _ => {}
            }
        }

        changed
    }

    fn enqueue_due_revisions(&mut self, idx: usize, now_ms: i64) -> bool {
        let mut changed = false;
        let root = self.ledger.folders[idx].path.clone();
        let folder_id = self.ledger.folders[idx].id.clone();
        let rev_wait_ms = self.supported.document_revision_cooldown_ms;
        let due_revisions: Vec<(String, i64, FileKind, Option<String>)> = self.ledger.folders[idx]
            .files
            .values()
            .filter_map(|t| {
                let save_ms = t.pending_revision_save_ms?;
                if now_ms.saturating_sub(save_ms) >= rev_wait_ms {
                    Some((
                        t.rel_path.clone(),
                        save_ms,
                        t.kind,
                        t.pending_revision_hash.clone(),
                    ))
                } else {
                    None
                }
            })
            .collect();

        for (rel_path, save_ms, kind, rev_hash) in due_revisions {
            let abs_path = join_rel_path(&root, &rel_path);
            let Ok(handle) = self.platform.open_file(&root, &abs_path) else {
                continue;
            };
            let Ok(fs_meta) = handle.metadata() else {
                continue;
            };
            let metadata = collect_metadata(&root, &abs_path, &fs_meta, &*self.platform);
            let ev_stamp = self.platform.stamp_at(save_ms);
            let event_id = event_id_for(&folder_id, &rel_path, ActivityVerb::Revised, save_ms);
            self.ledger.folders[idx].pending.push(PendingEvent {
                id: event_id,
                rel_path: rel_path.clone(),
                previous_path: None,
                verb: ActivityVerb::Revised,
                kind,
                delivery: DeliveryMode::Content,
                event_ms: save_ms,
                observed_ms: now_ms,
                offset_s: ev_stamp.offset_s,
                window: ev_stamp.window(),
                window_end_ms: ev_stamp.window_end_ms(),
                metadata,
                content_hash: rev_hash,
                note_payload: None,
                note_is_diff: false,
                is_revision: true,
                backfill: false,
                retry_at_ms: None,
                content_refused: false,
                tries: 0,
            });
            if let Some(tracked) = self.ledger.folders[idx].files.get_mut(&rel_path) {
                tracked.pending_revision_save_ms = None;
                tracked.pending_revision_hash = None;
            }
            changed = true;
        }

        changed
    }

    /// Pick the next ready five-minute segment across all `On` folders
    /// (newest window first, honoring Band 4 newest-first backfill ordering).
    /// Returns `None` while paused by the computer or when no window is closed/due.
    pub fn next_delivery(&mut self, computer_paused: bool) -> Option<SegmentDelivery> {
        if computer_paused {
            return None;
        }
        let now_ms = self.platform.now_ms();
        let mut windows: BTreeMap<(Window, usize), Vec<&PendingEvent>> = BTreeMap::new();
        for (idx, folder) in self.ledger.folders.iter().enumerate() {
            let can_deliver = folder.mode == FolderMode::On
                || (folder.mode == FolderMode::SettingUp && !folder.held_paused);
            if !can_deliver || folder.attention.is_some() {
                continue;
            }
            for event in &folder.pending {
                if event.window_end_ms <= now_ms && event.retry_at_ms.is_none_or(|at| at <= now_ms)
                {
                    windows
                        .entry((event.window.clone(), idx))
                        .or_default()
                        .push(event);
                }
            }
        }
        windows
            .into_iter()
            .rev()
            .find_map(|((window, idx), events)| {
                self.select_window(&self.ledger.folders[idx], window, events)
            })
    }

    fn select_window(
        &self,
        folder: &FolderRecord,
        window: Window,
        mut events: Vec<&PendingEvent>,
    ) -> Option<SegmentDelivery> {
        events.sort_by(|a, b| {
            a.event_ms
                .cmp(&b.event_ms)
                .then_with(|| a.rel_path.cmp(&b.rel_path))
        });

        let mut selected_events: Vec<PendingEvent> = Vec::with_capacity(events.len().min(256));
        let mut staged_files = Vec::new();
        let mut staged_bytes = 0u64;
        for ev in events {
            // Bounded well below the wire line/byte caps, including two long origin URLs per row.
            if selected_events.len() >= 256 {
                break;
            }
            let disk_path = join_rel_path(&folder.path, &ev.rel_path);
            // Only a complete scan may infer a removal. Skip a stale event here
            // when its path can no longer be opened, including during first setup.
            if ev.verb != ActivityVerb::Removed
                && self.platform.open_file(&folder.path, &disk_path).is_err()
            {
                continue;
            }
            if ev.delivery != DeliveryMode::Content {
                selected_events.push(ev.clone());
                continue;
            }
            if staged_files.len() >= MAX_UPLOAD_CONTENT_PARTS
                || staged_bytes.saturating_add(ev.metadata.size_bytes) > MAX_CONTENT_BYTES
            {
                continue;
            }
            let ext = ev.metadata.extension.as_deref().unwrap_or("");
            let needs_image_convert = matches!(ev.kind, FileKind::Screenshot | FileKind::Photo)
                && self.supported.convertible_image_extensions.contains(ext);
            // Compressed images can expand up to the entire content allowance.
            // Reserve that space until the converter has measured its output.
            if needs_image_convert && !staged_files.is_empty() {
                continue;
            }
            staged_bytes = if needs_image_convert {
                MAX_CONTENT_BYTES
            } else {
                staged_bytes.saturating_add(ev.metadata.size_bytes)
            };
            staged_files.push(StagedContentFile {
                event_id: ev.id.clone(),
                rel_path: ev.rel_path.clone(),
                disk_path,
                kind: ev.kind,
                content_hash: ev.content_hash.clone(),
                note_payload: ev.note_payload.clone(),
                note_is_diff: ev.note_is_diff,
                is_revision: ev.is_revision,
                needs_image_convert,
            });
            selected_events.push(ev.clone());
        }

        let folder_id = folder.id.clone();
        let folder_name = folder.name.clone();
        let folder_root = folder.path.clone();
        let facet = folder.facet.as_ref().map(|f| f.name.clone());
        let kinds = folder.kinds;

        if selected_events.is_empty() {
            return None;
        }

        Some(SegmentDelivery {
            folder_id,
            folder_name,
            folder_root,
            facet,
            kinds,
            window,
            events: selected_events,
            content_files: staged_files,
        })
    }

    /// Record the journal's reply for a delivered segment.
    pub fn record_outcome(&mut self, delivery: &SegmentDelivery, outcome: DeliveryOutcome) {
        let now_ms = self.platform.now_ms();
        let Some(folder) = self
            .ledger
            .folders
            .iter_mut()
            .find(|f| f.id == delivery.folder_id)
        else {
            return;
        };
        let ids: BTreeSet<&str> = delivery.events.iter().map(|e| e.id.as_str()).collect();

        match outcome {
            DeliveryOutcome::Delivered | DeliveryOutcome::DeliveredWithRefusedContent(_) => {
                let refused_set: BTreeSet<&str> = match &outcome {
                    DeliveryOutcome::DeliveredWithRefusedContent(paths) => {
                        folder.fallback_to_details = folder
                            .fallback_to_details
                            .saturating_add(paths.len() as u64);
                        paths.iter().map(String::as_str).collect()
                    }
                    _ => BTreeSet::new(),
                };
                let mut completed_backfill = 0u64;
                let mut fallback_events: Vec<PendingEvent> = Vec::new();
                for ev in &delivery.events {
                    if refused_set.contains(ev.rel_path.as_str())
                        && ev.delivery == DeliveryMode::Content
                    {
                        let mut meta_only = ev.clone();
                        meta_only.delivery = DeliveryMode::MetadataOnly;
                        meta_only.content_refused = true;
                        meta_only.content_hash = None;
                        meta_only.note_payload = None;
                        meta_only.retry_at_ms = None;
                        fallback_events.push(meta_only);
                        continue;
                    }
                    if ev.backfill {
                        completed_backfill += 1;
                    }
                    folder.items_taken_in = folder.items_taken_in.saturating_add(1);
                    folder.last_taken_in_ms = Some(now_ms);
                    if let Some(ref hook) = self.intake_hook {
                        hook(&ev.id, now_ms.div_euclid(1_000));
                    }
                    if let Some(tracked) = folder.files.get_mut(&ev.rel_path) {
                        tracked.delivered_to_journal = true;
                        if ev.content_hash.is_some() {
                            tracked.last_sent_hash = ev.content_hash.clone();
                        }
                        if ev.kind == FileKind::Note
                            && ev.delivery == DeliveryMode::Content
                            && let Some(ref text) = ev.note_payload
                        {
                            tracked.last_note_text = Some(text.clone());
                        }
                    }
                }
                folder.pending.retain(|ev| !ids.contains(ev.id.as_str()));
                folder.pending.extend(fallback_events);
                if completed_backfill > 0 {
                    folder.backfill_done = folder.backfill_done.saturating_add(completed_backfill);
                    let remaining_backfill = folder.pending.iter().any(|ev| ev.backfill);
                    if !remaining_backfill {
                        folder.backfill_total = 0;
                        folder.backfill_done = 0;
                    }
                }
            }
            DeliveryOutcome::AllFilesRefused(refused) => {
                let refused_set: BTreeSet<&str> = refused.iter().map(String::as_str).collect();
                let mut converted = 0u64;
                for ev in &mut folder.pending {
                    if !ids.contains(ev.id.as_str()) {
                        continue;
                    }
                    if ev.delivery == DeliveryMode::Content
                        && (refused_set.is_empty() || refused_set.contains(ev.rel_path.as_str()))
                    {
                        ev.delivery = DeliveryMode::MetadataOnly;
                        ev.content_refused = true;
                        ev.content_hash = None;
                        ev.note_payload = None;
                        ev.retry_at_ms = None;
                        converted += 1;
                    }
                }
                if converted > 0 {
                    folder.fallback_to_details =
                        folder.fallback_to_details.saturating_add(converted);
                } else {
                    folder.pending.retain(|ev| !ids.contains(ev.id.as_str()));
                }
            }
            DeliveryOutcome::InvalidPermanent => {
                folder.pending.retain(|ev| !ids.contains(ev.id.as_str()));
            }
            DeliveryOutcome::Hold => {
                for ev in &mut folder.pending {
                    if ids.contains(ev.id.as_str()) {
                        let step = (ev.tries as usize).min(RETRY_MS.len() - 1);
                        ev.tries = ev.tries.saturating_add(1);
                        ev.retry_at_ms = Some(now_ms.saturating_add(RETRY_MS[step]));
                    }
                }
            }
        }
        let _ = self.save();
    }

    /// Build the IPC / UI status snapshot.
    #[must_use]
    pub fn document(
        &self,
        facets: Vec<FacetOption>,
        facets_state: FacetsAvailability,
        computer_paused: bool,
        paired: bool,
        offline: bool,
        journal_lists: Option<bool>,
    ) -> FoldersDocument {
        let folders = self
            .ledger
            .folders
            .iter()
            .map(|f| {
                let progress = if f.backfill_total > 0 && f.backfill_done < f.backfill_total {
                    Some([f.backfill_done, f.backfill_total])
                } else {
                    None
                };
                FolderSummary {
                    id: f.id.clone(),
                    name: f.name.clone(),
                    path: f.path.display().to_string(),
                    label: f.label.clone(),
                    mode: f.mode,
                    returning: f.returning,
                    kinds: f.kinds,
                    facet: f.facet.clone(),
                    overlap: f.overlap.clone(),
                    attention: f.attention,
                    preview: f.preview.clone(),
                    progress,
                    waiting: f.pending.len() as u64,
                    items_taken_in: f.items_taken_in,
                    last_taken_in_ms: f.last_taken_in_ms,
                    last_checked_ms: f.last_checked_ms,
                    cant_read: f.cant_read,
                    fallback_to_details: f.fallback_to_details,
                    journal_lists,
                    paused_by_computer: computer_paused && f.mode == FolderMode::On,
                }
            })
            .collect();
        FoldersDocument {
            v: 1,
            folders,
            facets,
            facets_state,
            computer_paused,
            paired,
            offline,
            journal_lists,
        }
    }

    /// Per-folder declarations for `PUT /app/network/api/clients/self/sources`.
    /// Ordered by severity (`needs_attention` -> `paused` -> `on`) then `folder_id`
    /// ascending, omitting `off` and `setting_up` folders.
    #[must_use]
    pub fn declared_folders(&self, computer_paused: bool) -> Vec<FolderDeclaration> {
        let mut out = Vec::new();
        for folder in &self.ledger.folders {
            let state = match folder.mode {
                FolderMode::Off | FolderMode::SettingUp => continue,
                FolderMode::On | FolderMode::Paused if folder.attention.is_some() => {
                    FolderDeclaredState::NeedsAttention
                }
                FolderMode::Paused => FolderDeclaredState::Paused,
                FolderMode::On if computer_paused => FolderDeclaredState::Paused,
                FolderMode::On => FolderDeclaredState::On,
            };
            out.push(FolderDeclaration {
                id: folder.id.clone(),
                state,
            });
        }
        out.sort_by(|a, b| a.state.cmp(&b.state).then_with(|| a.id.cmp(&b.id)));
        out.dedup_by(|a, b| a.id == b.id);
        out
    }
}

/// Stage protocol-v3 `FolderMeta` and multipart `FilePart`s (`folder_<sha256>.jsonl`
/// + `c_01.<ext>` companion parts) in memory without hydrating cloud placeholders.
pub fn stage_delivery(
    delivery: &SegmentDelivery,
    platform: &dyn FolderPlatform,
) -> io::Result<(FolderMeta, Vec<FilePart>)> {
    let mut staged_parts: HashMap<&str, (String, String)> = HashMap::new();
    let mut companion_parts: Vec<FilePart> = Vec::new();
    let mut next_index = 1usize;
    let mut remaining = MAX_CONTENT_BYTES;

    for content in &delivery.content_files {
        let bytes = platform.read_file(&delivery.folder_root, &content.disk_path, remaining)?;
        if content.note_payload.is_none()
            && content
                .content_hash
                .as_ref()
                .is_some_and(|hash| *hash != sha256_hex(&bytes))
        {
            return Err(io::Error::other("file changed since scan"));
        }
        let orig_ext = Path::new(&content.rel_path)
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .filter(|e| !e.is_empty() && e.bytes().all(|b| b.is_ascii_alphanumeric()));

        if let Some(ref note_text) = content.note_payload {
            let ext = orig_ext.as_deref().unwrap_or("md");
            let part_name = format!("c_{next_index:02}.{ext}");
            if note_text.len() as u64 > remaining {
                return Err(io::Error::other("segment size changed"));
            }
            let bytes = note_text.as_bytes().to_vec();
            remaining -= bytes.len() as u64;
            let hash = sha256_hex(&bytes);
            let content_type = guess_mime(Some(ext)).unwrap_or_else(|| "text/plain".to_owned());
            staged_parts.insert(content.event_id.as_str(), (part_name.clone(), hash));
            companion_parts.push(FilePart {
                filename: part_name,
                content_type,
                bytes,
            });
            next_index += 1;
        } else if content.needs_image_convert {
            if let Some(converted) = platform.convert_image(&bytes, content.kind)
                && converted.bytes.len() as u64 <= remaining
            {
                remaining -= converted.bytes.len() as u64;
                let part_name = format!("c_{next_index:02}.{}", converted.extension);
                let hash = sha256_hex(&converted.bytes);
                staged_parts.insert(content.event_id.as_str(), (part_name.clone(), hash));
                companion_parts.push(FilePart {
                    filename: part_name,
                    content_type: converted.content_type.to_owned(),
                    bytes: converted.bytes,
                });
                next_index += 1;
            }
        } else {
            remaining -= bytes.len() as u64;
            let part_name = match orig_ext.as_deref() {
                Some(ext) => format!("c_{next_index:02}.{ext}"),
                None => format!("c_{next_index:02}.bin"),
            };
            let hash = sha256_hex(&bytes);
            let content_type = guess_mime(orig_ext.as_deref())
                .unwrap_or_else(|| "application/octet-stream".to_owned());
            staged_parts.insert(content.event_id.as_str(), (part_name.clone(), hash));
            companion_parts.push(FilePart {
                filename: part_name,
                content_type,
                bytes,
            });
            next_index += 1;
        }
    }

    let mut activity_bytes = Vec::new();
    for ev in &delivery.events {
        if ev.delivery != DeliveryMode::Content && ev.verb != ActivityVerb::Removed {
            let path = join_rel_path(&delivery.folder_root, &ev.rel_path);
            platform.open_file(&delivery.folder_root, &path)?;
        }
        let observed_ms = if ev.observed_ms != 0 {
            ev.observed_ms
        } else {
            ev.event_ms
        };
        let at = Stamp::new(ev.event_ms, ev.offset_s).rfc3339();
        let observed_at = Stamp::new(observed_ms, ev.offset_s).rfc3339();
        let created_at = ev
            .metadata
            .created_ms
            .map(|ms| Stamp::new(ms, ev.offset_s).rfc3339());
        let modified_at = Stamp::new(ev.metadata.modified_ms, ev.offset_s).rfc3339();
        let file_type = ev
            .metadata
            .extension
            .as_deref()
            .filter(|ft| !ft.is_empty() && ft.len() <= 64 && !ft.chars().any(char::is_control));

        let (part, content_hash) = if ev.verb != ActivityVerb::Removed {
            match staged_parts.get(ev.id.as_str()) {
                Some((p, h)) => (Some(p.clone()), Some(h.clone())),
                None => (None, None),
            }
        } else {
            (None, None)
        };
        let kind_hint = part.as_ref().and_then(|_| ev.kind.kind_hint());

        let origin_url = clean_platform_str(ev.metadata.origin_url.as_deref());
        let referrer_url = clean_platform_str(ev.metadata.referrer_url.as_deref());
        let zone_id = ev.metadata.zone_id;

        let platform_val = if origin_url.is_some() || referrer_url.is_some() || zone_id.is_some() {
            let mut obj = serde_json::Map::new();
            if let Some(origin) = origin_url {
                obj.insert("origin_url".into(), Value::String(origin));
            }
            if let Some(referrer) = referrer_url {
                obj.insert("referrer_url".into(), Value::String(referrer));
            }
            if let Some(zid) = zone_id {
                obj.insert("zone_id".into(), Value::from(zid));
            }
            Some(Value::Object(obj))
        } else {
            None
        };

        let mut line = serde_json::Map::new();
        line.insert("v".into(), Value::from(1u32));
        line.insert("event".into(), Value::from(ev.verb.wire_event()));
        line.insert("path".into(), Value::from(ev.rel_path.as_str()));
        if ev.verb == ActivityVerb::Renamed
            && let Some(ref prev) = ev.previous_path
        {
            line.insert("previous_path".into(), Value::from(prev.as_str()));
        }
        line.insert("at".into(), Value::from(at));
        line.insert("observed_at".into(), Value::from(observed_at));
        if let Some(ft) = file_type {
            line.insert("file_type".into(), Value::from(ft));
        }
        line.insert("size".into(), Value::from(ev.metadata.size_bytes));
        if let Some(c) = created_at {
            line.insert("created_at".into(), Value::from(c));
        }
        line.insert("modified_at".into(), Value::from(modified_at));
        if let Some(h) = content_hash {
            line.insert("content_hash".into(), Value::from(h));
        }
        match part {
            Some(p) => line.insert("part".into(), Value::from(p)),
            None => line.insert("part".into(), Value::Null),
        };
        if let Some(kh) = kind_hint {
            line.insert("kind_hint".into(), Value::from(kh));
        }
        if let Some(plat) = platform_val {
            line.insert("platform".into(), plat);
        }

        let mut raw = serde_json::to_vec(&Value::Object(line))
            .map_err(|err| io::Error::other(err.to_string()))?;
        raw.push(b'\n');
        activity_bytes.extend_from_slice(&raw);
    }

    let activity_hash = sha256_hex(&activity_bytes);
    let activity_part = FilePart {
        filename: format!("folder_{activity_hash}.jsonl"),
        content_type: "application/x-ndjson".to_owned(),
        bytes: activity_bytes,
    };

    let meta = FolderMeta {
        v: 1,
        id: delivery.folder_id.clone(),
        name: wire_folder_name(&delivery.folder_name, &delivery.folder_id),
        facet: delivery.facet.clone(),
        kinds: delivery.kinds.enabled_names(),
    };

    let mut parts = Vec::with_capacity(1 + companion_parts.len());
    parts.push(activity_part);
    parts.extend(companion_parts);
    Ok((meta, parts))
}

fn wire_folder_name(name: &str, id: &str) -> String {
    let mut label = String::new();
    for c in name.trim().chars().filter(|c| !c.is_control()) {
        if label.len() + c.len_utf8() > 128 {
            break;
        }
        label.push(c);
    }
    if label.trim().is_empty() {
        id.to_owned()
    } else {
        label
    }
}

fn clean_platform_str(raw: Option<&str>) -> Option<String> {
    let trimmed = raw?.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_control) {
        return None;
    }
    if trimmed.len() <= 2048 {
        return Some(trimmed.to_owned());
    }
    let mut end = 2048;
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    Some(trimmed[..end].to_owned())
}

fn load_ledger(state_dir: &Path) -> Option<Ledger> {
    let path = state_dir.join(LEDGER_FILE);
    let bytes = match fs::read(&path) {
        Ok(b) => b,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            fs::read(state_dir.join(format!("{LEDGER_FILE}.bak"))).ok()?
        }
        Err(_) => return None,
    };
    serde_json::from_slice::<Ledger>(&bytes)
        .ok()
        .filter(|l| l.v == LEDGER_VERSION)
}

fn collect_metadata(
    root: &Path,
    path: &Path,
    fs_meta: &fs::Metadata,
    platform: &dyn FolderPlatform,
) -> PlatformMetadata {
    let modified_ms = fs_meta.modified().map(system_time_ms).unwrap_or(0);
    let created_ms = fs_meta.created().ok().map(system_time_ms);
    let extension = extension_lower(path);
    let mime = guess_mime(extension.as_deref());
    let zone = platform
        .read_zone_identifier(root, path)
        .unwrap_or_default();
    let origin_dom = zone.origin_url.as_deref().and_then(origin_domain);
    PlatformMetadata {
        size_bytes: fs_meta.len(),
        modified_ms,
        created_ms,
        extension,
        mime,
        origin_url: zone.origin_url,
        referrer_url: zone.referrer_url,
        origin_domain: origin_dom,
        zone_id: zone.zone_id,
    }
}

fn needs_text_sample(ext: Option<&str>, supported: &SupportedList) -> bool {
    ext.is_some_and(|e| {
        supported.note_extensions.contains(e)
            || (supported.transcript_extensions.contains(e) && !matches!(e, "vtt" | "srt"))
    })
}

fn span_start_ms(span: Span, now: Stamp, offset_at: impl Fn(i64) -> i32) -> i64 {
    use jiff::{Span as CalendarSpan, Timestamp, tz::Offset};
    let duration = match span {
        Span::All => return i64::MIN,
        Span::ThirtyDays => CalendarSpan::new().days(30),
        Span::SixMonths => CalendarSpan::new().months(6),
        Span::Year => CalendarSpan::new().years(1),
        Span::Nothing => CalendarSpan::new(),
    };
    let start = Timestamp::from_millisecond(now.ms).ok().and_then(|ts| {
        let today = Offset::from_seconds(now.offset_s)
            .ok()?
            .to_datetime(ts)
            .date();
        let first = today.checked_sub(duration).ok()?;
        Offset::UTC.to_timestamp(first.at(0, 0, 0, 0)).ok()
    });
    let Some(start) = start else {
        return now.local_midnight_ms();
    };
    let wall = start.as_second();
    // Locate the first instant of this civil day in the device's zone, including
    // offset changes between now and the target day and skipped local midnight.
    let (mut left, mut right) = (wall - 86_400, wall + 86_400);
    while left < right {
        let mid = left + (right - left) / 2;
        if mid + i64::from(offset_at(mid)) < wall {
            left = mid + 1;
        } else {
            right = mid;
        }
    }
    left * 1_000
}

/// Build a deterministic client-assigned folder ID matching
/// `^[a-z0-9][a-z0-9_-]{0,63}$` (protocol v3 contract).
fn make_folder_id(name: &str, canonical: &Path) -> String {
    let key = canonical.to_string_lossy().to_lowercase();
    let hash = &sha256_hex(key.as_bytes())[..8];
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if (ch == '-' || ch == '_' || ch == ' ') && !slug.is_empty() && !last_dash {
            slug.push('-');
            last_dash = true;
        }
        if slug.len() >= 40 {
            break;
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        format!("folder-{hash}")
    } else {
        format!("{slug}-{hash}")
    }
}

fn event_id_for(folder_id: &str, rel_path: &str, verb: ActivityVerb, event_ms: i64) -> String {
    let raw = format!("{folder_id}:{rel_path}:{}:{event_ms}", verb.as_str());
    sha256_hex(raw.as_bytes())
}

/// Recursively walk `root`, skipping cloud placeholders BEFORE opening files or
/// reading ADS metadata, skipping partial downloads / junk / hidden-system
/// entries, and confining symlinks/junctions inside `canonical_root`.
fn walk_folder(root: &Path, platform: &dyn FolderPlatform) -> Result<WalkOutcome, ListingError> {
    if !root.exists() {
        return Err(ListingError::Missing);
    }
    let canonical_raw = fs::canonicalize(root).map_err(|err| {
        if err.kind() == io::ErrorKind::NotFound {
            ListingError::Missing
        } else {
            ListingError::Incomplete(err)
        }
    })?;
    let canonical_root = strip_verbatim_prefix(&canonical_raw);
    if canonical_root != strip_verbatim_prefix(root) {
        return Err(ListingError::Incomplete(io::Error::other(
            "selected folder moved",
        )));
    }

    let mut out = WalkOutcome {
        files: Vec::new(),
        unreadable: 0,
        complete: true,
    };
    let mut stack: Vec<(PathBuf, String)> = vec![(canonical_raw.clone(), String::new())];
    let mut visited_dirs: BTreeSet<PathBuf> = BTreeSet::new();
    visited_dirs.insert(canonical_root.clone());

    while let Some((dir, prefix)) = stack.pop() {
        let dir_ids = platform.file_ids_in_dir(&dir);
        let result = visit_directory(&dir, |entry| {
            let name_os = entry.file_name();
            let Some(name) = name_os.to_str() else {
                return Ok(());
            };
            if is_junk_entry(name) {
                return Ok(());
            }
            if is_partial_download(name) {
                return Ok(());
            }
            let abs_path = entry.path();
            let Ok(sym_meta) = fs::symlink_metadata(&abs_path) else {
                out.unreadable += 1;
                out.complete = false;
                return Ok(());
            };
            // Check OneDrive / cloud-files placeholders BEFORE following links,
            // calling fs::metadata, or reading Zone.Identifier ADS so a
            // cloud-only file is never hydrated.
            if is_placeholder(&sym_meta) {
                return Ok(());
            }
            if platform.is_hidden_or_system(&sym_meta) {
                return Ok(());
            }
            if (sym_meta.file_type().is_symlink() || platform.is_reparse_point(&sym_meta))
                && !symlink_stays_inside(&canonical_root, &abs_path)
            {
                return Ok(());
            }
            let Ok(meta) = fs::metadata(&abs_path) else {
                out.unreadable += 1;
                out.complete = false;
                return Ok(());
            };
            if is_placeholder(&meta) {
                return Ok(());
            }
            let rel_path = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}/{name}")
            };
            if rel_path.len() > 1024
                || rel_path.contains('\\')
                || rel_path.chars().any(char::is_control)
            {
                return Ok(());
            }
            if meta.is_dir() {
                if let Ok(canon_raw) = fs::canonicalize(&abs_path) {
                    let canon_clean = strip_verbatim_prefix(&canon_raw);
                    if crate::classify::path_starts_with_ci(&canon_clean, &canonical_root)
                        && visited_dirs.insert(canon_clean)
                    {
                        stack.push((canon_raw, rel_path));
                    }
                }
                return Ok(());
            }
            if !meta.is_file() {
                return Ok(());
            }
            let platform_meta = collect_metadata(root, &abs_path, &meta, platform);
            let is_screenshot =
                platform.is_screenshot(root, &abs_path, platform_meta.extension.as_deref());
            let mtime_ns = meta
                .modified()
                .ok()
                .map(|t| match t.duration_since(std::time::UNIX_EPOCH) {
                    Ok(d) => d.as_nanos() as i128,
                    Err(e) => -(e.duration().as_nanos() as i128),
                })
                .unwrap_or(0);
            let file_id = dir_ids.get(&name_os).cloned();
            out.files.push(WalkedFile {
                rel_path,
                abs_path,
                file_id,
                size_bytes: meta.len(),
                mtime_ns,
                metadata: platform_meta,
                is_screenshot,
            });
            Ok(())
        });

        match result {
            Ok(()) => {}
            Err(ListingError::Missing) if dir == canonical_raw => {
                return Err(ListingError::Missing);
            }
            Err(ListingError::Incomplete(err)) if dir == canonical_raw => {
                return Err(ListingError::Incomplete(err));
            }
            Err(_) => {
                out.unreadable += 1;
                out.complete = false;
            }
        }
    }
    Ok(out)
}
#[cfg(test)]
mod range_tests {
    use super::*;
    fn stamp(utc: &str) -> i64 {
        utc.parse::<jiff::Timestamp>().unwrap().as_millisecond()
    }

    #[test]
    fn local_midnight_uses_calendar_days_clamped_months_and_the_offset_on_that_day() {
        let spring = stamp("2024-03-10T07:00:00Z") / 1000;
        let offset = |seconds| {
            if seconds < spring {
                -5 * 3600
            } else {
                -4 * 3600
            }
        };
        for (span, today, first) in [
            (
                Span::ThirtyDays,
                "2024-04-09T18:00:00Z",
                "2024-03-10T05:00:00Z",
            ),
            (
                Span::Nothing,
                "2024-03-10T17:00:00Z",
                "2024-03-10T05:00:00Z",
            ),
            (
                Span::SixMonths,
                "2024-08-31T16:00:00Z",
                "2024-02-29T05:00:00Z",
            ),
            (Span::Year, "2024-02-29T16:00:00Z", "2023-02-28T05:00:00Z"),
        ] {
            let ms = stamp(today);
            assert_eq!(
                span_start_ms(span, Stamp::new(ms, offset(ms / 1000)), offset),
                stamp(first)
            );
        }
    }
}
