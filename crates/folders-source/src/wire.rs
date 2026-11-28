// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Wire transport seam and protocol-v3 response parsing for the folder source:
//! capability gate (`"folder"`), `/supported`, `/facets`, `/estimate`, and
//! five-minute segment delivery answer routing.

use std::collections::BTreeSet;

use solstone_folder_sync::worker::{BoxFuture, Journal as FolderJournal};

use crate::engine::{DeliveryOutcome, SegmentDelivery};
use crate::types::{
    BackfillEstimate, EstimateItem, FacetOption, FacetsAvailability, SupportedList,
};

/// Source identifier sent on device ingest (protocol v3).
pub const SOURCE: &str = "folder";
/// Capability name advertised by `/api/system/status`.
pub const CAPABILITY: &str = "folder";

/// Raw reply returned by the app's transport for one folder segment upload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FolderDeliveryReply {
    pub http_status: u16,
    pub status: Option<String>,
    pub reason_code: Option<String>,
    pub refused_paths: Vec<String>,
}

/// Map a transport reply from `POST /app/devices/ingest` (`source = "folder"`)
/// onto a `DeliveryOutcome` per the protocol-v3 folder answers table.
#[must_use]
pub fn outcome_for(reply: &FolderDeliveryReply) -> DeliveryOutcome {
    if (200..300).contains(&reply.http_status)
        && matches!(
            reply.status.as_deref(),
            Some("ok" | "duplicate" | "collision")
        )
    {
        if !reply.refused_paths.is_empty() {
            return DeliveryOutcome::DeliveredWithRefusedContent(reply.refused_paths.clone());
        }
        return DeliveryOutcome::Delivered;
    }
    match reply.reason_code.as_deref() {
        Some("segment_removed" | "duplicate" | "collision") => DeliveryOutcome::Delivered,
        Some("folder_file_invalid") => {
            DeliveryOutcome::AllFilesRefused(reply.refused_paths.clone())
        }
        Some("folder_invalid") => DeliveryOutcome::InvalidPermanent,
        Some("folder_ahead" | "journal_read_failed") => DeliveryOutcome::Hold,
        _ => DeliveryOutcome::Hold,
    }
}

/// Journal operations needed by the folder worker beyond `FolderJournal`.
pub trait FolderTransport:
    FolderJournal<Upload = SegmentDelivery, Reply = Option<DeliveryOutcome>> + Send + Sync + 'static
{
    fn is_paired(&self) -> bool;
    fn is_offline(&self) -> bool;
    fn fetch_facets(&self) -> BoxFuture<'_, (FacetsAvailability, Vec<FacetOption>)>;
    fn fetch_supported(&self) -> BoxFuture<'_, Option<SupportedList>>;
    fn fetch_estimate(
        &self,
        folder_id: String,
        items: Vec<EstimateItem>,
    ) -> BoxFuture<'_, Option<BackfillEstimate>>;
}

/// Parse `GET /app/devices/ingest/folder/facets` JSON response.
#[must_use]
pub fn parse_facets(body: &[u8]) -> Option<Vec<FacetOption>> {
    #[derive(serde::Deserialize)]
    struct WireFacet {
        name: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        color: Option<String>,
        #[serde(default)]
        muted: bool,
    }
    #[derive(serde::Deserialize)]
    struct WireList {
        facets: Vec<WireFacet>,
    }
    let parsed: WireList = serde_json::from_slice(body).ok()?;
    let mut out: Vec<FacetOption> = parsed
        .facets
        .into_iter()
        .filter(|f| !f.muted && !f.name.trim().is_empty())
        .map(|f| FacetOption {
            title: if f.title.trim().is_empty() {
                f.name.clone()
            } else {
                f.title
            },
            name: f.name,
            color: f.color,
        })
        .collect();
    out.sort_by_key(|a| a.title.to_lowercase());
    Some(out)
}

/// Parse `GET /app/devices/ingest/folder/supported` JSON response.
#[must_use]
pub fn parse_supported_list(body: &[u8]) -> Option<SupportedList> {
    #[derive(serde::Deserialize, Default)]
    struct WireThresholds {
        #[serde(default)]
        structured_prose_min_words: Option<usize>,
        #[serde(default)]
        structured_prose_min_heading_levels: Option<usize>,
        #[serde(default)]
        long_prose_min_words: Option<usize>,
    }
    #[derive(serde::Deserialize, Default)]
    struct WireKind {
        #[serde(default)]
        extensions: Vec<String>,
        #[serde(default)]
        convert_to_png_on_device: Vec<String>,
        #[serde(default)]
        thresholds: Option<WireThresholds>,
    }
    #[derive(serde::Deserialize, Default)]
    struct WireKinds {
        #[serde(default)]
        notes: WireKind,
        #[serde(default)]
        documents: WireKind,
        #[serde(default)]
        transcripts: WireKind,
        #[serde(default)]
        screenshots: WireKind,
        #[serde(default)]
        photos: WireKind,
    }
    #[derive(serde::Deserialize)]
    struct WireSupported {
        #[serde(default)]
        ruleset_version: Option<u32>,
        #[serde(default)]
        max_file_bytes: Option<u64>,
        #[serde(default)]
        max_note_bytes: Option<u64>,
        #[serde(default)]
        max_image_pixels: Option<u64>,
        #[serde(default)]
        document_revision_cooldown_seconds: Option<i64>,
        #[serde(default)]
        kinds: WireKinds,
    }

    let wire: WireSupported = serde_json::from_slice(body).ok()?;
    let mut list = SupportedList::default();
    let norm = |items: Vec<String>| -> BTreeSet<String> {
        items
            .into_iter()
            .map(|s| s.trim().trim_start_matches('.').to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect()
    };

    let notes = norm(wire.kinds.notes.extensions);
    if !notes.is_empty() {
        list.note_extensions = notes;
    }

    let docs: BTreeSet<String> = norm(wire.kinds.documents.extensions)
        .into_iter()
        .filter(|ext| !list.note_extensions.contains(ext))
        .collect();
    if !docs.is_empty() {
        list.document_extensions = docs;
    }
    if let Some(th) = wire.kinds.documents.thresholds {
        if let Some(w) = th.structured_prose_min_words {
            list.note_structured_words = w;
        }
        if let Some(h) = th.structured_prose_min_heading_levels {
            list.note_heading_levels = h;
        }
        if let Some(w) = th.long_prose_min_words {
            list.note_long_words = w;
        }
    }

    let transcripts: BTreeSet<String> = norm(wire.kinds.transcripts.extensions)
        .into_iter()
        .filter(|ext| !list.document_extensions.contains(ext))
        .collect();
    if !transcripts.is_empty() {
        list.transcript_extensions = transcripts;
    }

    let mut accepted_images = norm(wire.kinds.screenshots.extensions);
    accepted_images.extend(norm(wire.kinds.photos.extensions));
    if !accepted_images.is_empty() {
        list.accepted_image_extensions = accepted_images;
    }

    let mut convertible = norm(wire.kinds.screenshots.convert_to_png_on_device);
    convertible.extend(norm(wire.kinds.photos.convert_to_png_on_device));
    if !convertible.is_empty() {
        list.convertible_image_extensions = convertible;
    }

    if let Some(v) = wire.ruleset_version {
        list.ruleset_version = v;
    }
    if let Some(max) = wire.max_file_bytes {
        list.max_content_bytes = max;
    }
    if let Some(max) = wire.max_note_bytes {
        list.max_note_bytes = max;
    }
    if let Some(px) = wire.max_image_pixels {
        list.max_image_pixels = px;
    }
    if let Some(secs) = wire.document_revision_cooldown_seconds
        && secs > 0
    {
        list.document_revision_cooldown_ms = secs.saturating_mul(1_000);
    }
    list.from_journal = true;
    Some(list)
}

/// Parse `POST /app/devices/ingest/folder/estimate` JSON response.
#[must_use]
pub fn parse_estimate(body: &[u8]) -> Option<BackfillEstimate> {
    #[derive(serde::Deserialize)]
    struct WireEstimate {
        segments: u64,
        days: u64,
        thoughts: u64,
        #[serde(default)]
        already_in_journal: u64,
    }
    let wire: WireEstimate = serde_json::from_slice(body).ok()?;
    Some(BackfillEstimate {
        segments: wire.segments,
        days: wire.days,
        thoughts: wire.thoughts,
        already_in_journal: wire.already_in_journal,
        from_journal: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_protocol_v3_supported_list() {
        let raw = br#"{
          "version": 1,
          "ruleset_version": 8,
          "window_seconds": 300,
          "max_file_bytes": 67108864,
          "max_note_bytes": 4194304,
          "max_image_pixels": 67108864,
          "document_revision_cooldown_seconds": 3600,
          "kinds": {
            "notes": { "extensions": ["md", "markdown", "txt", "log"], "mode": "first_sight_and_net_change" },
            "documents": {
              "extensions": ["pdf", "md", "markdown", "txt", "log"],
              "mode": "each_revision",
              "thresholds": {
                "structured_prose_min_words": 1500,
                "structured_prose_min_heading_levels": 2,
                "long_prose_min_words": 5000
              }
            },
            "transcripts": {
              "extensions": ["vtt", "srt", "md", "markdown", "txt", "log", "json", "csv", "tsv", "html"],
              "mode": "each_revision"
            },
            "screenshots": {
              "extensions": ["png", "jpg", "jpeg", "webp"],
              "convert_to_png_on_device": ["heic", "heif", "tif", "tiff", "gif", "bmp"],
              "mode": "once"
            },
            "photos": {
              "extensions": ["png", "jpg", "jpeg", "webp"],
              "convert_to_png_on_device": ["heic", "heif", "tif", "tiff", "gif", "bmp"],
              "mode": "once"
            },
            "other": { "extensions": [], "mode": "metadata_only" }
          },
          "activity_only": { "model_passes": 0 }
        }"#;
        let parsed = parse_supported_list(raw).unwrap();
        assert!(parsed.from_journal);
        assert_eq!(parsed.ruleset_version, 8);
        assert_eq!(parsed.max_content_bytes, 67_108_864);
        assert_eq!(parsed.max_note_bytes, 4_194_304);
        assert_eq!(parsed.document_revision_cooldown_ms, 3_600_000);
        assert!(parsed.note_extensions.contains("log"));
        assert!(parsed.document_extensions.contains("pdf"));
        assert!(!parsed.document_extensions.contains("docx"));
        assert!(parsed.transcript_extensions.contains("vtt"));
        assert!(parsed.convertible_image_extensions.contains("heic"));
    }

    #[test]
    fn parses_protocol_v3_facets_and_estimate() {
        let facets_raw = br##"{
          "facets": [
            { "name": "work", "title": "Work", "color": "#2f6fdd" },
            { "name": "personal", "title": "Personal", "color": "#2f6fdd" }
          ]
        }"##;
        let facets = parse_facets(facets_raw).unwrap();
        assert_eq!(facets.len(), 2);
        assert_eq!(facets[0].name, "personal");
        assert_eq!(facets[1].name, "work");

        let est_raw = br#"{
          "items": 2,
          "content_items": 1,
          "activity_only_items": 1,
          "already_in_journal": 3,
          "segments": 2,
          "content_segments": 1,
          "activity_only_segments": 1,
          "days": 2,
          "thoughts": 1,
          "backfill_paused": false
        }"#;
        let est = parse_estimate(est_raw).unwrap();
        assert!(est.from_journal);
        assert_eq!(est.segments, 2);
        assert_eq!(est.days, 2);
        assert_eq!(est.thoughts, 1);
        assert_eq!(est.already_in_journal, 3);
    }

    #[test]
    fn maps_folder_answers_table_outcomes() {
        assert_eq!(
            outcome_for(&FolderDeliveryReply {
                http_status: 200,
                status: Some("ok".into()),
                reason_code: None,
                refused_paths: vec![],
            }),
            DeliveryOutcome::Delivered
        );
        assert_eq!(
            outcome_for(&FolderDeliveryReply {
                http_status: 200,
                status: Some("ok".into()),
                reason_code: None,
                refused_paths: vec!["bad.pdf".into()],
            }),
            DeliveryOutcome::DeliveredWithRefusedContent(vec!["bad.pdf".into()])
        );
        assert_eq!(
            outcome_for(&FolderDeliveryReply {
                http_status: 400,
                status: Some("rejected".into()),
                reason_code: Some("folder_file_invalid".into()),
                refused_paths: vec!["bad.pdf".into()],
            }),
            DeliveryOutcome::AllFilesRefused(vec!["bad.pdf".into()])
        );
        assert_eq!(
            outcome_for(&FolderDeliveryReply {
                http_status: 400,
                status: Some("rejected".into()),
                reason_code: Some("folder_ahead".into()),
                refused_paths: vec![],
            }),
            DeliveryOutcome::Hold
        );
        assert_eq!(
            outcome_for(&FolderDeliveryReply {
                http_status: 503,
                status: None,
                reason_code: Some("journal_read_failed".into()),
                refused_paths: vec![],
            }),
            DeliveryOutcome::Hold
        );
        assert_eq!(
            outcome_for(&FolderDeliveryReply {
                http_status: 400,
                status: Some("rejected".into()),
                reason_code: Some("folder_invalid".into()),
                refused_paths: vec![],
            }),
            DeliveryOutcome::InvalidPermanent
        );
    }
}
