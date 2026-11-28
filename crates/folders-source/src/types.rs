// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Data types shared by the folder source engine, worker, IPC surface, and UI.

use std::collections::BTreeSet;

use screenshots_source::Span;
use serde::{Deserialize, Serialize};

/// Maximum bytes of a file whose content is eligible for device ingest.
pub const MAX_CONTENT_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum bytes of a note eligible for edit tracking.
pub const MAX_NOTE_BYTES: u64 = 4 * 1024 * 1024;
/// Quiet wait (1 hour) before a saved revision of an already-sent document or
/// transcript is sent again.
pub const DOCUMENT_REVISION_WAIT_MS: i64 = 60 * 60 * 1000;

/// Owner-chosen file kinds to bring in from a folder. All six default on.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FolderKinds {
    #[serde(default = "default_true")]
    pub notes: bool,
    #[serde(default = "default_true")]
    pub documents: bool,
    #[serde(default = "default_true")]
    pub transcripts: bool,
    #[serde(default = "default_true")]
    pub screenshots: bool,
    #[serde(default = "default_true")]
    pub photos: bool,
    #[serde(default = "default_true")]
    pub other: bool,
}

fn default_true() -> bool {
    true
}

impl Default for FolderKinds {
    fn default() -> Self {
        Self {
            notes: true,
            documents: true,
            transcripts: true,
            screenshots: true,
            photos: true,
            other: true,
        }
    }
}

impl FolderKinds {
    #[must_use]
    pub fn any_selected(self) -> bool {
        self.notes
            || self.documents
            || self.transcripts
            || self.screenshots
            || self.photos
            || self.other
    }

    #[must_use]
    pub fn enabled_for(self, kind: FileKind) -> bool {
        match kind {
            FileKind::Note => self.notes,
            FileKind::Document => self.documents,
            FileKind::Transcript => self.transcripts,
            FileKind::Screenshot => self.screenshots,
            FileKind::Photo => self.photos,
            FileKind::Other => self.other,
        }
    }

    /// Names of enabled kinds in stable UI/wire order.
    #[must_use]
    pub fn enabled_names(self) -> Vec<String> {
        let mut out = Vec::with_capacity(6);
        if self.notes {
            out.push("notes".to_owned());
        }
        if self.documents {
            out.push("documents".to_owned());
        }
        if self.transcripts {
            out.push("transcripts".to_owned());
        }
        if self.screenshots {
            out.push("screenshots".to_owned());
        }
        if self.photos {
            out.push("photos".to_owned());
        }
        if self.other {
            out.push("other".to_owned());
        }
        out
    }
}

/// Deterministic kind assigned to a file on the device.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Note,
    Document,
    Transcript,
    Screenshot,
    Photo,
    Other,
}

impl FileKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::Document => "document",
            Self::Transcript => "transcript",
            Self::Screenshot => "screenshot",
            Self::Photo => "photo",
            Self::Other => "other",
        }
    }

    /// Wire `kind_hint` sent on protocol-v3 folder activity lines and estimate items.
    #[must_use]
    pub fn kind_hint(self) -> Option<&'static str> {
        match self {
            Self::Note => Some("notes"),
            Self::Document => Some("documents"),
            Self::Transcript => Some("transcripts"),
            Self::Screenshot => Some("screenshots"),
            Self::Photo => Some("photos"),
            Self::Other => None,
        }
    }
}

/// Whether a file's bytes leave the device, only its metadata goes, or it is skipped.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMode {
    Content,
    MetadataOnly,
    Ignore,
}

/// Activity verb recorded for a file event inside a five-minute window.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityVerb {
    Added,
    Edited,
    Revised,
    Renamed,
    Removed,
}

impl ActivityVerb {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Edited => "edited",
            Self::Revised => "revised",
            Self::Renamed => "renamed",
            Self::Removed => "removed",
        }
    }

    /// Canonical wire `event` string for protocol-v3 folder activity JSONL.
    #[must_use]
    pub fn wire_event(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Edited | Self::Revised => "changed",
            Self::Renamed => "renamed",
            Self::Removed => "removed",
        }
    }
}

/// Lifecycle mode of one configured folder.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderMode {
    /// Picked, not yet confirmed. Nothing leaves the device.
    #[default]
    SettingUp,
    /// Active: watches and sends five-minute segments.
    On,
    /// Paused individually: keeps tracking changes locally without skipping,
    /// and holds delivery until resumed.
    Paused,
    /// Turned off: stops watching; already-delivered journal material stays.
    Off,
}

/// Fatal folder-level condition that puts a folder into `needs attention`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderAttention {
    Missing,
    Unreadable,
}

/// Declared state of one folder for `PUT /app/network/api/clients/self/sources`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum FolderDeclaredState {
    NeedsAttention,
    Paused,
    On,
}

impl FolderDeclaredState {
    #[must_use]
    pub fn as_wire_str(self) -> &'static str {
        match self {
            Self::NeedsAttention => "needs_attention",
            Self::Paused => "paused",
            Self::On => "on",
        }
    }
}

/// Per-folder declaration entry (`source = "folder"`, `detail = folder_id`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FolderDeclaration {
    pub id: String,
    pub state: FolderDeclaredState,
}

/// Existing source that overlaps a candidate folder path.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OverlapTarget {
    Obsidian { name: String },
    Screenshots { name: String },
    Folder { id: String, name: String },
}

/// Result of checking a chosen folder path before setup begins.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PathCheckResult {
    Ready {
        path: String,
        name: String,
        label: String,
    },
    AlreadyAdded {
        folder_id: String,
        name: String,
    },
    Overlaps {
        path: String,
        name: String,
        label: String,
        target: OverlapTarget,
    },
    Refused {
        path: String,
    },
}

/// One facet available on the paired journal.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FacetOption {
    pub name: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// Availability of the journal's facet list.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FacetsAvailability {
    Ready,
    #[default]
    Unpaired,
    Offline,
}

/// One item sent to `POST /app/devices/ingest/folder/estimate`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EstimateItem {
    pub path: String,
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    pub has_content: bool,
}

/// Non-money backfill work estimate (segments, days, thoughts).
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BackfillEstimate {
    pub segments: u64,
    pub days: u64,
    pub thoughts: u64,
    #[serde(default)]
    pub already_in_journal: u64,
    pub from_journal: bool,
}

/// Preview counts and range summary shown before confirmation.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PreviewSummary {
    pub span: Option<Span>,
    pub total_items: u64,
    pub content_items: u64,
    pub content_bytes: u64,
    pub metadata_only_items: u64,
    pub notes: u64,
    pub documents: u64,
    pub transcripts: u64,
    pub screenshots: u64,
    pub photos: u64,
    pub other: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newest_ms: Option<i64>,
    pub unreadable: u64,
    pub failed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate: Option<BackfillEstimate>,
}

/// Platform metadata attached to a folder activity event.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlatformMetadata {
    pub size_bytes: u64,
    pub modified_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referrer_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone_id: Option<u64>,
}

/// Supported file-type table published by the journal, with a local fallback
/// used until the paired journal answers.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SupportedList {
    pub note_extensions: BTreeSet<String>,
    pub document_extensions: BTreeSet<String>,
    pub transcript_extensions: BTreeSet<String>,
    pub accepted_image_extensions: BTreeSet<String>,
    pub convertible_image_extensions: BTreeSet<String>,
    pub max_content_bytes: u64,
    pub max_note_bytes: u64,
    pub max_image_pixels: u64,
    pub document_revision_cooldown_ms: i64,
    pub ruleset_version: u32,
    pub note_structured_words: usize,
    pub note_heading_levels: usize,
    pub note_long_words: usize,
    pub from_journal: bool,
}

impl Default for SupportedList {
    fn default() -> Self {
        let set = |items: &[&str]| items.iter().map(|s| (*s).to_owned()).collect();
        Self {
            note_extensions: set(&["md", "markdown", "txt", "log"]),
            document_extensions: set(&["pdf"]),
            transcript_extensions: set(&["vtt", "srt"]),
            accepted_image_extensions: set(&["png", "jpg", "jpeg", "webp"]),
            convertible_image_extensions: set(&["heic", "heif", "tif", "tiff", "gif", "bmp"]),
            max_content_bytes: MAX_CONTENT_BYTES,
            max_note_bytes: MAX_NOTE_BYTES,
            max_image_pixels: 67_108_864,
            document_revision_cooldown_ms: DOCUMENT_REVISION_WAIT_MS,
            ruleset_version: 8,
            note_structured_words: 1_500,
            note_heading_levels: 2,
            note_long_words: 5_000,
            from_journal: false,
        }
    }
}

/// Status snapshot for one configured folder, read by the settings window.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FolderSummary {
    pub id: String,
    pub name: String,
    pub path: String,
    pub label: String,
    pub mode: FolderMode,
    pub returning: bool,
    pub kinds: FolderKinds,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facet: Option<FacetOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlap: Option<OverlapTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<FolderAttention>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<PreviewSummary>,
    /// `[done, total]` while chosen history is still arriving in the journal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<[u64; 2]>,
    pub waiting: u64,
    pub items_taken_in: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_taken_in_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked_ms: Option<i64>,
    pub cant_read: u64,
    pub fallback_to_details: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_lists: Option<bool>,
    pub paused_by_computer: bool,
}

/// Complete folders source document returned over IPC to the settings window.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct FoldersDocument {
    pub v: u32,
    pub folders: Vec<FolderSummary>,
    #[serde(default)]
    pub facets: Vec<FacetOption>,
    #[serde(default)]
    pub facets_state: FacetsAvailability,
    #[serde(default)]
    pub computer_paused: bool,
    #[serde(default)]
    pub paired: bool,
    #[serde(default)]
    pub offline: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_lists: Option<bool>,
}

/// Action request sent from the settings window to the folder worker.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum FolderActionRequest {
    BeginSetup {
        path: String,
        #[serde(default)]
        overlap: Option<OverlapTarget>,
    },
    Repoint {
        id: String,
        path: String,
    },
    SetKinds {
        id: String,
        kinds: FolderKinds,
    },
    SetFacet {
        id: String,
        facet: Option<FacetOption>,
    },
    Preview {
        id: String,
        span: Span,
    },
    Confirm {
        id: String,
        span: Span,
    },
    Pause {
        id: String,
    },
    Resume {
        id: String,
    },
    TurnOff {
        id: String,
    },
    TurnOn {
        id: String,
    },
    Remove {
        id: String,
    },
    Retry {
        id: String,
    },
    RefreshFacets,
}
