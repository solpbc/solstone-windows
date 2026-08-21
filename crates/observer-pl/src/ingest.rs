// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Protocol-v3 linked-device ingest wire types and custody proof.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// One file submitted in a protocol-v3 multipart upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePart {
    pub filename: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

/// The local multipart-construction failure detectable before sending.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum IngestMultipartError {
    #[error("ingest upload requires at least one file")]
    EmptyFiles,
    #[error("duplicate ingest filename: {0}")]
    DuplicateFilename(String),
}

#[derive(Debug, Serialize, Clone)]
struct IngestEnvelope {
    day: String,
    segment: String,
    files: Vec<EnvelopeFile>,
}

#[derive(Debug, Serialize, Clone)]
struct EnvelopeFile {
    submitted: String,
}

/// A protocol-v3 multipart upload whose envelope is derived from its file
/// parts. The envelope file list cannot be supplied independently.
#[derive(Debug, Clone)]
pub struct IngestMultipart {
    boundary: String,
    envelope: IngestEnvelope,
    files: Vec<FilePart>,
}

impl IngestMultipart {
    /// Construct an upload for `day` and `segment`, rejecting duplicate submitted
    /// names before they can reach the server.
    pub fn new(
        boundary: impl Into<String>,
        day: impl Into<String>,
        segment: impl Into<String>,
        files: Vec<FilePart>,
    ) -> Result<Self, IngestMultipartError> {
        if files.is_empty() {
            return Err(IngestMultipartError::EmptyFiles);
        }
        let mut names = HashSet::with_capacity(files.len());
        for file in &files {
            if !names.insert(file.filename.as_str()) {
                return Err(IngestMultipartError::DuplicateFilename(
                    file.filename.clone(),
                ));
            }
        }
        let envelope = IngestEnvelope {
            day: day.into(),
            segment: segment.into(),
            files: files
                .iter()
                .map(|file| EnvelopeFile {
                    submitted: file.filename.clone(),
                })
                .collect(),
        };
        Ok(Self {
            boundary: boundary.into(),
            envelope,
            files,
        })
    }

    /// Multipart content type, including this request's boundary.
    pub fn content_type(&self) -> String {
        format!("multipart/form-data; boundary={}", self.boundary)
    }

    /// Serialize the multipart body. The envelope is a text part and therefore
    /// intentionally has no `filename` attribute.
    pub fn serialize(&self) -> Result<Vec<u8>, serde_json::Error> {
        let envelope = serde_json::to_vec(&self.envelope)?;
        let mut out = Vec::new();
        write_text_part(
            &mut out,
            &self.boundary,
            "envelope",
            "application/json",
            &envelope,
        );
        for file in &self.files {
            out.extend_from_slice(format!("--{}\r\n", self.boundary).as_bytes());
            out.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"files\"; filename=\"{}\"\r\n",
                    file.filename
                )
                .as_bytes(),
            );
            out.extend_from_slice(
                format!("Content-Type: {}\r\n\r\n", file.content_type).as_bytes(),
            );
            out.extend_from_slice(&file.bytes);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(format!("--{}--\r\n", self.boundary).as_bytes());
        Ok(out)
    }
}

fn write_text_part(
    out: &mut Vec<u8>,
    boundary: &str,
    name: &str,
    content_type: &str,
    value: &[u8],
) {
    out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    out.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"{name}\"\r\n").as_bytes(),
    );
    out.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
    out.extend_from_slice(value);
    out.extend_from_slice(b"\r\n");
}

/// Response status from `/app/devices/ingest`.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IngestStatus {
    Ok,
    Duplicate,
    Collision,
    Conflict,
    Failed,
}

impl IngestStatus {
    /// Whether the response lets reconciliation prove custody.
    pub fn is_accepted(self) -> bool {
        matches!(self, Self::Ok | Self::Duplicate | Self::Collision)
    }
}

/// The consumed fields of an ingest response.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct IngestResponse {
    pub status: IngestStatus,
    #[serde(default)]
    pub segment: Option<String>,
    #[serde(default)]
    pub existing_segment: Option<String>,
}

/// Root ingest manifest returned by `/app/devices/ingest/manifest`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct IngestManifest {
    pub days: BTreeMap<String, ManifestDay>,
}

/// One root-manifest day entry.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ManifestDay {
    pub segments: u64,
}

/// A per-day ingest manifest returned by `/app/devices/ingest/manifest/{day}`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct DayManifest {
    pub version: u64,
    pub day: String,
    pub segments: BTreeMap<String, DayManifestSegment>,
}

/// Files recorded for one segment in a day manifest.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct DayManifestSegment {
    pub files: Vec<SegmentFile>,
}

/// The protocol-v3 segments response.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct SegmentsEnvelope {
    pub items: Vec<SegmentItem>,
    pub total: u64,
    pub protocol_version: u64,
}

/// One segment returned by `/app/devices/ingest/segments/{day}`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct SegmentItem {
    pub key: String,
    pub observed: bool,
    pub files: Vec<SegmentFile>,
    #[serde(default)]
    pub original_key: Option<String>,
}

/// One server-listed file.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct SegmentFile {
    pub name: String,
    pub size: u64,
    pub sha256: String,
    pub status: SegmentFileStatus,
    #[serde(default)]
    pub submitted_name: Option<String>,
}

impl SegmentFile {
    /// The submission name, falling back to the written name when the journal
    /// did not rename the file.
    pub fn submitted_or_name(&self) -> &str {
        self.submitted_name.as_deref().unwrap_or(&self.name)
    }
}

/// Closed custody vocabulary from the protocol-v3 projection.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SegmentFileStatus {
    Present,
    Missing,
    Processed,
}

impl SegmentFileStatus {
    fn is_terminal(self) -> bool {
        matches!(self, Self::Present | Self::Processed)
    }
}

/// Locally attested file data supplied to [`prove_custody`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalFile<'a> {
    pub name: &'a str,
    pub sha256: &'a str,
    pub size: u64,
}

/// A completed server custody witness. The server key can only be obtained from
/// a [`CustodyProof::Confirmed`] result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodyWitness {
    server_segment: String,
    files: Vec<CustodyFileWitness>,
}

impl CustodyWitness {
    pub fn server_segment(&self) -> &str {
        &self.server_segment
    }

    /// Server facts for each locally matched file, available only after the
    /// complete custody proof succeeds.
    pub fn files(&self) -> &[CustodyFileWitness] {
        &self.files
    }
}

/// Server custody facts for one submitted file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodyFileWitness {
    pub submitted_name: String,
    pub sha256: String,
    pub size: u64,
    pub status: SegmentFileStatus,
}

/// Which independently fetched listing supplied a failed file assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustodySource {
    DayManifest,
    Segments,
}

/// A named reason a server response cannot earn local cleanup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustodyFailure {
    DayAbsentFromRootManifest {
        day: String,
    },
    DayManifestDayMismatch {
        expected: String,
        actual: String,
    },
    SegmentAbsentFromDayManifest {
        segment_key: String,
    },
    ProtocolVersionMismatch {
        actual: u64,
    },
    SegmentsTotalMismatch {
        total: u64,
        item_count: usize,
    },
    SegmentAbsentFromSegments {
        segment_key: String,
    },
    FileCountMismatch {
        source: CustodySource,
        expected: usize,
        actual: usize,
    },
    FileMissing {
        source: CustodySource,
        name: String,
    },
    FileRenamed {
        source: CustodySource,
        expected: String,
        actual: String,
    },
    FileSha256Mismatch {
        source: CustodySource,
        name: String,
        expected: String,
        actual: String,
    },
    FileSizeMismatch {
        source: CustodySource,
        name: String,
        expected: u64,
        actual: u64,
    },
    FileNotHeld {
        source: CustodySource,
        name: String,
        status: SegmentFileStatus,
    },
}

/// Result of the complete three-read custody check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustodyProof {
    Confirmed(CustodyWitness),
    Unconfirmed(CustodyFailure),
}

/// Prove that the v3 manifest, day manifest, and segments response together
/// witness every local file for `segment_key` on `day`.
pub fn prove_custody(
    manifest: &IngestManifest,
    day_manifest: &DayManifest,
    segments: &SegmentsEnvelope,
    day: &str,
    segment_key: &str,
    local: &[LocalFile<'_>],
) -> CustodyProof {
    if !manifest.days.contains_key(day) {
        return CustodyProof::Unconfirmed(CustodyFailure::DayAbsentFromRootManifest {
            day: day.to_owned(),
        });
    }
    if day_manifest.day != day {
        return CustodyProof::Unconfirmed(CustodyFailure::DayManifestDayMismatch {
            expected: day.to_owned(),
            actual: day_manifest.day.clone(),
        });
    }
    let Some(day_segment) = day_manifest.segments.get(segment_key) else {
        return CustodyProof::Unconfirmed(CustodyFailure::SegmentAbsentFromDayManifest {
            segment_key: segment_key.to_owned(),
        });
    };
    if segments.protocol_version != 3 {
        return CustodyProof::Unconfirmed(CustodyFailure::ProtocolVersionMismatch {
            actual: segments.protocol_version,
        });
    }
    if segments.total != segments.items.len() as u64 {
        return CustodyProof::Unconfirmed(CustodyFailure::SegmentsTotalMismatch {
            total: segments.total,
            item_count: segments.items.len(),
        });
    }
    let Some(segment) = segments.items.iter().find(|item| item.key == segment_key) else {
        return CustodyProof::Unconfirmed(CustodyFailure::SegmentAbsentFromSegments {
            segment_key: segment_key.to_owned(),
        });
    };
    if let Err(failure) = prove_files(CustodySource::DayManifest, &day_segment.files, local) {
        return CustodyProof::Unconfirmed(failure);
    }
    if let Err(failure) = prove_files(CustodySource::Segments, &segment.files, local) {
        return CustodyProof::Unconfirmed(failure);
    }
    CustodyProof::Confirmed(CustodyWitness {
        server_segment: segment.key.clone(),
        files: segment
            .files
            .iter()
            .map(|file| CustodyFileWitness {
                submitted_name: file.submitted_or_name().to_owned(),
                sha256: file.sha256.clone(),
                size: file.size,
                status: file.status,
            })
            .collect(),
    })
}

fn prove_files(
    source: CustodySource,
    files: &[SegmentFile],
    local: &[LocalFile<'_>],
) -> Result<(), CustodyFailure> {
    if files.len() != local.len() {
        return Err(CustodyFailure::FileCountMismatch {
            source,
            expected: local.len(),
            actual: files.len(),
        });
    }
    for local_file in local {
        let Some(file) = files
            .iter()
            .find(|file| file.submitted_or_name() == local_file.name)
        else {
            if let Some(file) = files.iter().find(|file| file.name == local_file.name) {
                return Err(CustodyFailure::FileRenamed {
                    source,
                    expected: local_file.name.to_owned(),
                    actual: file.submitted_or_name().to_owned(),
                });
            }
            return Err(CustodyFailure::FileMissing {
                source,
                name: local_file.name.to_owned(),
            });
        };
        if file.sha256 != local_file.sha256 {
            return Err(CustodyFailure::FileSha256Mismatch {
                source,
                name: local_file.name.to_owned(),
                expected: local_file.sha256.to_owned(),
                actual: file.sha256.clone(),
            });
        }
        if file.size != local_file.size {
            return Err(CustodyFailure::FileSizeMismatch {
                source,
                name: local_file.name.to_owned(),
                expected: local_file.size,
                actual: file.size,
            });
        }
        if !file.status.is_terminal() {
            return Err(CustodyFailure::FileNotHeld {
                source,
                name: local_file.name.to_owned(),
                status: file.status,
            });
        }
    }
    Ok(())
}
