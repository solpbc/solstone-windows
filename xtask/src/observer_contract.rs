// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Offline verifier and immutable pins for the observer-client authority bundle.

use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

pub const ADOPTION_SCHEMA_VERSION: u64 = 1;
pub const CONSUMER_IDENTIFIER: &str = "solstone-windows";
pub const AUTHORITY_REPOSITORY: &str = "https://github.com/solpbc/solstone-journal";
pub const AUTHORITY_COMMIT: &str = "827d3761e2b515b9bd537ded28b245c8c6d86cc0";
pub const BUNDLE_SEMVER: &str = "1.0.2";
pub const ARCHIVE_SHA256: &str = "3b57fa9fb4736dff1f72ffdd48928834ea33d3925d132874202764cf9f988667";
pub const ARCHIVE_SIZE_BYTES: u64 = 16_950;
/// All authority paths are relative to the explicit bundle directory.
pub const AUTHORITY_MANIFEST_PATH: &str = "manifest.json";
pub const AUTHORITY_MANIFEST_SHA256: &str =
    "9ecf4bbfcd793a8aecc9e2257254e68c74c48cde22282ff07369101b90d97c33";
pub const GENERATOR_IDENTITY: &str = "solstone.convey.contract.observer_bundle.v1";
pub const BUNDLE_SCHEMA_IDENTITY: &str = "solstone.observer-client-contract-bundle.schema.v1";
pub const SCHEMA_DIALECT_URI: &str = "https://json-schema.org/draft/2020-12/schema";
pub const OPENAPI_DOCUMENT_VERSION: &str = "1.0.0";
pub const OPENAPI_SPEC_VERSION: &str = "3.1.0";
pub const PROJECTION_PATH: &str = "projection.openapi.json";
pub const OBSERVER_PROTOCOL_VERSION: u64 = 2;
pub const SUPPORTED_RESPONSE_VARIANTS: &[u64] = &[1, 2];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FilePin {
    pub path: &'static str,
    pub sha256: &'static str,
}

pub const BUNDLE_FILES: &[FilePin] = &[
    FilePin {
        path: "consumer-audit.json",
        sha256: "f3562062aeb971c9dc95ae5d14333566b28431758bcd232c33c093757df7bc18",
    },
    FilePin {
        path: "fixtures/wire-behavior.json",
        sha256: "9749a50daba9b4a270da045d350bc5edb7a42c9723fa0bf420c8fb8a4a0415f8",
    },
    FilePin {
        path: "projection.openapi.json",
        sha256: "8a2b7037552edf710597f2ffa6fdc5aa715311df4ea8cf168e70abe4231c64ca",
    },
    FilePin {
        path: "vectors.json",
        sha256: "7a5132c57b61e2a615a22719abc77e40b708d4a6636c45690cc522dc26c36dec",
    },
];

pub const COMPONENT_CLOSURE: &[&str] = &[
    "CallosumEvent",
    "Error",
    "SegmentFile",
    "SegmentItem",
    "SegmentsEnvelope",
];

pub const CONSUMER_IDENTIFIERS: &[&str] = &[
    "solstone-android",
    "solstone-browser",
    "solstone-linux",
    "solstone-macos",
    "solstone-swift",
    "solstone-tmux",
    "solstone-windows",
];

pub const INITIAL_TARGETS: &[&str] = &["solstone-linux", "solstone-windows"];

pub const OPERATION_IDS: &[&str] = &[
    "callosum.rootEvents",
    "chat.openSolChatRequest",
    "link.pair",
    "observer.callosumStream",
    "observer.ingestEvent",
    "observer.ingestSegments",
    "observer.ingestUpload",
    "observer.register",
];

pub const ADOPTED_OPERATION_IDS: &[&str] = &[
    "callosum.rootEvents",
    "link.pair",
    "observer.ingestEvent",
    "observer.ingestSegments",
    "observer.ingestUpload",
    "observer.register",
];

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct OperationMapping {
    pub operation_id: &'static str,
    pub method: &'static str,
    pub path: &'static str,
}

pub const WINDOWS_OPERATION_MAPPINGS: &[OperationMapping] = &[
    OperationMapping {
        operation_id: "callosum.rootEvents",
        method: "GET",
        path: "/sse/events",
    },
    OperationMapping {
        operation_id: "link.pair",
        method: "POST",
        path: "/app/network/pair",
    },
    OperationMapping {
        operation_id: "observer.ingestEvent",
        method: "POST",
        path: "/app/observer/ingest/event",
    },
    OperationMapping {
        operation_id: "observer.ingestSegments",
        method: "GET",
        path: "/app/observer/ingest/segments/{day}",
    },
    OperationMapping {
        operation_id: "observer.ingestUpload",
        method: "POST",
        path: "/app/observer/ingest",
    },
    OperationMapping {
        operation_id: "observer.register",
        method: "POST",
        path: "/app/observer/register",
    },
];

pub const FULL_FIXTURE_IDS: &[&str] = &[
    "declared.observer.ingestSegments.custody_unknown_rejected",
    "declared.observer.ingestSegments.envelope_total_mismatch",
    "declared.observer.ingestUpload.status_unknown_rejected",
    "example.callosum.rootEvents.response.200.text-event-stream.default",
    "example.chat.openSolChatRequest.request.body.application-json.default",
    "example.chat.openSolChatRequest.response.200.application-json.default",
    "example.link.pair.request.body.application-json.default",
    "example.link.pair.response.200.application-json.default",
    "example.observer.callosumStream.response.200.text-event-stream.default",
    "example.observer.ingestEvent.request.body.application-json.default",
    "example.observer.ingestEvent.response.200.application-json.default",
    "example.observer.ingestSegments.response.200.application-json.legacy",
    "example.observer.ingestSegments.response.200.application-json.v2",
    "example.observer.ingestUpload.request.body.multipart-form-data.default",
    "example.observer.ingestUpload.response.200.application-json.duplicate",
    "example.observer.ingestUpload.response.200.application-json.normal",
    "example.observer.register.request.body.application-json.default",
    "example.observer.register.response.200.application-json.default",
    "recorded.auth.bearer.segments",
    "recorded.auth.handle.segments",
    "recorded.chat.openSolChatRequest.missing",
    "recorded.chat.openSolChatRequest.ok",
    "recorded.ingestUpload.collision",
    "recorded.ingestUpload.conflict",
    "recorded.ingestUpload.duplicate",
    "recorded.ingestUpload.failed",
    "recorded.ingestUpload.ok",
    "recorded.segments.custody_statuses",
    "recorded.segments.legacy.absent_header",
    "recorded.segments.legacy.unparseable_header",
    "recorded.segments.submitted_name_omitted",
    "recorded.segments.v2.envelope",
    "recorded.sse.observer.data",
    "recorded.sse.observer.error",
    "recorded.sse.observer.heartbeat",
    "recorded.sse.root.data_unknown_event",
    "recorded.sse.root.heartbeat",
];

pub const ADOPTED_FIXTURE_IDS: &[&str] = &[
    "declared.observer.ingestSegments.custody_unknown_rejected",
    "declared.observer.ingestSegments.envelope_total_mismatch",
    "declared.observer.ingestUpload.status_unknown_rejected",
    "example.callosum.rootEvents.response.200.text-event-stream.default",
    "example.link.pair.request.body.application-json.default",
    "example.link.pair.response.200.application-json.default",
    "example.observer.ingestEvent.request.body.application-json.default",
    "example.observer.ingestEvent.response.200.application-json.default",
    "example.observer.ingestSegments.response.200.application-json.legacy",
    "example.observer.ingestSegments.response.200.application-json.v2",
    "example.observer.ingestUpload.request.body.multipart-form-data.default",
    "example.observer.ingestUpload.response.200.application-json.duplicate",
    "example.observer.ingestUpload.response.200.application-json.normal",
    "example.observer.register.request.body.application-json.default",
    "example.observer.register.response.200.application-json.default",
    "recorded.auth.bearer.segments",
    "recorded.auth.handle.segments",
    "recorded.ingestUpload.collision",
    "recorded.ingestUpload.conflict",
    "recorded.ingestUpload.duplicate",
    "recorded.ingestUpload.failed",
    "recorded.ingestUpload.ok",
    "recorded.segments.custody_statuses",
    "recorded.segments.legacy.absent_header",
    "recorded.segments.legacy.unparseable_header",
    "recorded.segments.submitted_name_omitted",
    "recorded.segments.v2.envelope",
    "recorded.sse.root.data_unknown_event",
    "recorded.sse.root.heartbeat",
];

pub const FULL_VECTOR_IDS: &[&str] = &[
    "callosum.rootEvents.sse.data_unknown_event",
    "callosum.rootEvents.sse.heartbeat",
    "chat.openSolChatRequest.missing_required_field",
    "chat.openSolChatRequest.ok",
    "observer.auth.bearer",
    "observer.auth.handle",
    "observer.callosumStream.sse.data",
    "observer.callosumStream.sse.error",
    "observer.callosumStream.sse.heartbeat",
    "observer.ingestSegments.custody_statuses",
    "observer.ingestSegments.custody_unknown_rejected",
    "observer.ingestSegments.envelope_total_mismatch",
    "observer.ingestSegments.legacy_array.absent_header",
    "observer.ingestSegments.legacy_array.unparseable_header",
    "observer.ingestSegments.submitted_name_fallback",
    "observer.ingestSegments.v2_envelope",
    "observer.ingestUpload.status.collision",
    "observer.ingestUpload.status.conflict",
    "observer.ingestUpload.status.duplicate",
    "observer.ingestUpload.status.failed",
    "observer.ingestUpload.status.ok",
    "observer.ingestUpload.status_unknown_rejected",
];

pub const ADOPTED_VECTOR_IDS: &[&str] = &[
    "callosum.rootEvents.sse.data_unknown_event",
    "callosum.rootEvents.sse.heartbeat",
    "observer.auth.bearer",
    "observer.auth.handle",
    "observer.ingestSegments.custody_statuses",
    "observer.ingestSegments.custody_unknown_rejected",
    "observer.ingestSegments.envelope_total_mismatch",
    "observer.ingestSegments.legacy_array.absent_header",
    "observer.ingestSegments.legacy_array.unparseable_header",
    "observer.ingestSegments.submitted_name_fallback",
    "observer.ingestSegments.v2_envelope",
    "observer.ingestUpload.status.collision",
    "observer.ingestUpload.status.conflict",
    "observer.ingestUpload.status.duplicate",
    "observer.ingestUpload.status.failed",
    "observer.ingestUpload.status.ok",
    "observer.ingestUpload.status_unknown_rejected",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsafePathReason {
    Absolute,
    Empty,
    EmptyComponent,
    ReservedName,
    TrailingDotOrSpace,
    NonPortableName,
}

#[derive(Debug)]
pub enum VerifyError {
    Io {
        path: String,
        message: String,
    },
    UnsafePath {
        path: String,
        reason: UnsafePathReason,
    },
    Traversal {
        path: String,
    },
    Backslash {
        path: String,
    },
    ControlChar {
        path: String,
    },
    DuplicatePath {
        path: String,
    },
    CaseCollision {
        first: String,
        second: String,
    },
    NonRegularFile {
        path: String,
        kind: &'static str,
    },
    InvalidFileMode {
        path: String,
        mode: u32,
    },
    MissingFile {
        path: String,
    },
    UnlistedFile {
        path: String,
    },
    ExtraFile {
        path: String,
    },
    DigestMismatch {
        path: String,
        expected: String,
        actual: String,
    },
    MalformedJson {
        document: String,
        message: String,
    },
    MalformedManifest {
        message: String,
    },
    ForbiddenAdoptionMetadata {
        field: String,
    },
    AdoptionShapeMismatch {
        field: String,
    },
    AdoptionFieldMismatch {
        field: String,
    },
    AdoptionCoverageDuplicate {
        field: String,
        id: String,
    },
    AdoptionCoverageUnsorted {
        field: String,
    },
    AdoptionCoverageMismatch {
        field: String,
    },
    ManifestFieldMismatch {
        field: String,
    },
    ManifestInventoryMismatch {
        message: String,
    },
    ProjectionMismatch {
        message: String,
    },
    FixtureSetMismatch {
        message: String,
    },
    VectorSetMismatch {
        message: String,
    },
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, message } => write!(f, "I/O error for {path}: {message}"),
            Self::UnsafePath { path, reason } => write!(f, "unsafe path {path:?}: {reason:?}"),
            Self::Traversal { path } => write!(f, "path traversal: {path:?}"),
            Self::Backslash { path } => write!(f, "backslash in path: {path:?}"),
            Self::ControlChar { path } => write!(f, "control character in path: {path:?}"),
            Self::DuplicatePath { path } => write!(f, "duplicate path: {path}"),
            Self::CaseCollision { first, second } => {
                write!(f, "case-colliding paths: {first} and {second}")
            }
            Self::NonRegularFile { path, kind } => write!(f, "non-regular file {path}: {kind}"),
            Self::InvalidFileMode { path, mode } => write!(f, "invalid mode {mode:o} for {path}"),
            Self::MissingFile { path } => write!(f, "missing file: {path}"),
            Self::UnlistedFile { path } => write!(f, "unlisted file: {path}"),
            Self::ExtraFile { path } => write!(f, "extra file: {path}"),
            Self::DigestMismatch {
                path,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "digest mismatch for {path}: expected {expected}, got {actual}"
                )
            }
            Self::MalformedJson { document, message } => {
                write!(f, "malformed JSON in {document}: {message}")
            }
            Self::MalformedManifest { message } => write!(f, "malformed manifest: {message}"),
            Self::ForbiddenAdoptionMetadata { field } => {
                write!(f, "forbidden adoption metadata field: {field}")
            }
            Self::AdoptionShapeMismatch { field } => {
                write!(f, "adoption field has the wrong shape: {field}")
            }
            Self::AdoptionFieldMismatch { field } => write!(f, "adoption field mismatch: {field}"),
            Self::AdoptionCoverageDuplicate { field, id } => {
                write!(f, "duplicate adoption coverage ID in {field}: {id}")
            }
            Self::AdoptionCoverageUnsorted { field } => {
                write!(f, "adoption coverage is not sorted: {field}")
            }
            Self::AdoptionCoverageMismatch { field } => {
                write!(f, "adoption coverage mismatch: {field}")
            }
            Self::ManifestFieldMismatch { field } => write!(f, "manifest field mismatch: {field}"),
            Self::ManifestInventoryMismatch { message } => {
                write!(f, "manifest inventory mismatch: {message}")
            }
            Self::ProjectionMismatch { message } => write!(f, "projection mismatch: {message}"),
            Self::FixtureSetMismatch { message } => write!(f, "fixture set mismatch: {message}"),
            Self::VectorSetMismatch { message } => write!(f, "vector set mismatch: {message}"),
        }
    }
}

impl std::error::Error for VerifyError {}

#[derive(Debug, Eq, PartialEq)]
pub struct VerifyReport {
    pub bundle_semver: &'static str,
    pub operation_count: usize,
    pub fixture_count: usize,
    pub vector_count: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AdoptionRecord {
    adoption_schema_version: u64,
    consumer_identifier: String,
    authority_repository: String,
    authority_commit: String,
    bundle_semver: String,
    archive_sha256: String,
    archive_size_bytes: u64,
    authority_manifest_path: String,
    authority_manifest_sha256: String,
    bundle_files: Vec<AdoptionFile>,
    adopted_operation_ids: Vec<String>,
    adopted_fixture_ids: Vec<String>,
    adopted_vector_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AdoptionFile {
    path: String,
    sha256: String,
}

#[derive(Clone)]
struct ManifestFile {
    path: String,
    sha256: String,
}

/// Verify the exact local authority bundle and its consumer-owned adoption mirror.
pub fn verify(bundle_dir: &Path, adoption_path: &Path) -> Result<VerifyReport, VerifyError> {
    verify_regular_file(adoption_path, "adoption.json")?;
    let adoption_bytes = read_file(adoption_path, "adoption.json")?;
    let adoption_value: Value = parse_json(&adoption_bytes, "adoption.json")?;
    verify_forbidden_adoption_fields(&adoption_value)?;
    let adoption: AdoptionRecord = serde_json::from_value(adoption_value).map_err(|error| {
        VerifyError::AdoptionShapeMismatch {
            field: error.to_string(),
        }
    })?;
    verify_adoption(&adoption)?;

    let bundle_meta =
        fs::symlink_metadata(bundle_dir).map_err(|error| io_error(bundle_dir, error))?;
    if !bundle_meta.file_type().is_dir() {
        return Err(VerifyError::NonRegularFile {
            path: "bundle".to_owned(),
            kind: file_kind(&bundle_meta),
        });
    }
    verify_mode(bundle_dir, &bundle_meta, true)?;

    let (actual_files, actual_dirs) = walk_bundle(bundle_dir)?;
    if !actual_files.contains(AUTHORITY_MANIFEST_PATH) {
        return Err(VerifyError::MissingFile {
            path: AUTHORITY_MANIFEST_PATH.to_owned(),
        });
    }

    let manifest_path = bundle_dir.join(AUTHORITY_MANIFEST_PATH);
    let manifest_bytes = read_file(&manifest_path, AUTHORITY_MANIFEST_PATH)?;
    let manifest: Value = parse_json(&manifest_bytes, AUTHORITY_MANIFEST_PATH)?;
    let manifest_files = parse_manifest_files(&manifest)?;
    verify_declared_paths(&manifest_files)?;
    verify_inventory(&actual_files, &actual_dirs, &manifest_files)?;

    let mut pending_digest = compare_digest(
        AUTHORITY_MANIFEST_PATH,
        &manifest_bytes,
        AUTHORITY_MANIFEST_SHA256,
    );
    for pin in BUNDLE_FILES {
        let bytes = read_file(&bundle_dir.join(pin.path), pin.path)?;
        let declared = manifest_files
            .iter()
            .find(|entry| entry.path == pin.path)
            .expect("inventory equality establishes every pinned file");
        if let Some(error) = compare_digest(pin.path, &bytes, &declared.sha256) {
            pending_digest.get_or_insert(error);
        }
        if let Some(error) = compare_digest(pin.path, &bytes, pin.sha256) {
            pending_digest.get_or_insert(error);
        }
    }

    verify_manifest_fields(&manifest)?;
    verify_projection(&bundle_dir.join(PROJECTION_PATH))?;
    verify_id_document(
        &bundle_dir.join("fixtures/wire-behavior.json"),
        "fixtures/wire-behavior.json",
        "fixtures",
        FULL_FIXTURE_IDS,
        ADOPTED_FIXTURE_IDS,
        true,
    )?;
    verify_id_document(
        &bundle_dir.join("vectors.json"),
        "vectors.json",
        "vectors",
        FULL_VECTOR_IDS,
        ADOPTED_VECTOR_IDS,
        false,
    )?;

    if let Some(error) = pending_digest {
        return Err(error);
    }

    Ok(VerifyReport {
        bundle_semver: BUNDLE_SEMVER,
        operation_count: OPERATION_IDS.len(),
        fixture_count: ADOPTED_FIXTURE_IDS.len(),
        vector_count: ADOPTED_VECTOR_IDS.len(),
    })
}

fn verify_regular_file(path: &Path, label: &str) -> Result<(), VerifyError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| io_error(path, error))?;
    if !metadata.file_type().is_file() {
        return Err(VerifyError::NonRegularFile {
            path: label.to_owned(),
            kind: file_kind(&metadata),
        });
    }
    verify_mode(path, &metadata, false)
}

fn read_file(path: &Path, label: &str) -> Result<Vec<u8>, VerifyError> {
    fs::read(path).map_err(|error| VerifyError::Io {
        path: label.to_owned(),
        message: error.to_string(),
    })
}

fn parse_json(bytes: &[u8], document: &str) -> Result<Value, VerifyError> {
    serde_json::from_slice(bytes).map_err(|error| VerifyError::MalformedJson {
        document: document.to_owned(),
        message: error.to_string(),
    })
}

fn verify_forbidden_adoption_fields(value: &Value) -> Result<(), VerifyError> {
    const FORBIDDEN: &[&str] = &[
        "generated_at",
        "hostname",
        "username",
        "temp_path",
        "internal_job_id",
        "rollout_state",
        "windows_commit",
    ];
    let object = value
        .as_object()
        .ok_or_else(|| VerifyError::AdoptionShapeMismatch {
            field: "top level".to_owned(),
        })?;
    if let Some(field) = FORBIDDEN.iter().find(|field| object.contains_key(**field)) {
        return Err(VerifyError::ForbiddenAdoptionMetadata {
            field: (*field).to_string(),
        });
    }
    Ok(())
}

fn verify_adoption(record: &AdoptionRecord) -> Result<(), VerifyError> {
    macro_rules! exact {
        ($field:ident, $expected:expr) => {
            if record.$field != $expected {
                return Err(VerifyError::AdoptionFieldMismatch {
                    field: stringify!($field).to_owned(),
                });
            }
        };
    }
    exact!(adoption_schema_version, ADOPTION_SCHEMA_VERSION);
    exact!(consumer_identifier, CONSUMER_IDENTIFIER);
    exact!(authority_repository, AUTHORITY_REPOSITORY);
    exact!(authority_commit, AUTHORITY_COMMIT);
    exact!(bundle_semver, BUNDLE_SEMVER);
    exact!(archive_sha256, ARCHIVE_SHA256);
    exact!(archive_size_bytes, ARCHIVE_SIZE_BYTES);
    exact!(authority_manifest_path, AUTHORITY_MANIFEST_PATH);
    exact!(authority_manifest_sha256, AUTHORITY_MANIFEST_SHA256);

    let expected_files: Vec<(&str, &str)> = BUNDLE_FILES
        .iter()
        .map(|entry| (entry.path, entry.sha256))
        .collect();
    let actual_files: Vec<(&str, &str)> = record
        .bundle_files
        .iter()
        .map(|entry| (entry.path.as_str(), entry.sha256.as_str()))
        .collect();
    if actual_files != expected_files {
        return Err(VerifyError::AdoptionFieldMismatch {
            field: "bundle_files".to_owned(),
        });
    }

    verify_coverage(
        "adopted_operation_ids",
        &record.adopted_operation_ids,
        ADOPTED_OPERATION_IDS,
    )?;
    verify_coverage(
        "adopted_fixture_ids",
        &record.adopted_fixture_ids,
        ADOPTED_FIXTURE_IDS,
    )?;
    verify_coverage(
        "adopted_vector_ids",
        &record.adopted_vector_ids,
        ADOPTED_VECTOR_IDS,
    )
}

fn verify_coverage(field: &str, actual: &[String], expected: &[&str]) -> Result<(), VerifyError> {
    let mut seen = BTreeSet::new();
    for id in actual {
        if !seen.insert(id.as_str()) {
            return Err(VerifyError::AdoptionCoverageDuplicate {
                field: field.to_owned(),
                id: id.clone(),
            });
        }
    }
    if actual.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(VerifyError::AdoptionCoverageUnsorted {
            field: field.to_owned(),
        });
    }
    if actual
        .iter()
        .map(String::as_str)
        .ne(expected.iter().copied())
    {
        return Err(VerifyError::AdoptionCoverageMismatch {
            field: field.to_owned(),
        });
    }
    Ok(())
}

fn walk_bundle(bundle_dir: &Path) -> Result<(BTreeSet<String>, BTreeSet<String>), VerifyError> {
    let mut files = BTreeSet::new();
    let mut dirs = BTreeSet::new();
    let mut folded = BTreeMap::<String, String>::new();
    walk_dir(bundle_dir, "", &mut files, &mut dirs, &mut folded)?;
    Ok((files, dirs))
}

fn walk_dir(
    root: &Path,
    relative: &str,
    files: &mut BTreeSet<String>,
    dirs: &mut BTreeSet<String>,
    folded: &mut BTreeMap<String, String>,
) -> Result<(), VerifyError> {
    let current = if relative.is_empty() {
        root.to_path_buf()
    } else {
        root.join(relative)
    };
    let entries = fs::read_dir(&current).map_err(|error| io_error(&current, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| io_error(&current, error))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| VerifyError::UnsafePath {
                path: relative.to_owned(),
                reason: UnsafePathReason::NonPortableName,
            })?;
        let child = if relative.is_empty() {
            name
        } else {
            format!("{relative}/{name}")
        };
        validate_relative_path(&child)?;
        if let Some(first) = folded.insert(child.to_ascii_lowercase(), child.clone()) {
            return Err(VerifyError::CaseCollision {
                first,
                second: child,
            });
        }
        let metadata =
            fs::symlink_metadata(entry.path()).map_err(|error| io_error(&entry.path(), error))?;
        if metadata.file_type().is_dir() {
            verify_mode(&entry.path(), &metadata, true)?;
            dirs.insert(child.clone());
            walk_dir(root, &child, files, dirs, folded)?;
        } else if metadata.file_type().is_file() {
            verify_mode(&entry.path(), &metadata, false)?;
            files.insert(child);
        } else {
            return Err(VerifyError::NonRegularFile {
                path: child,
                kind: file_kind(&metadata),
            });
        }
    }
    Ok(())
}

pub fn validate_relative_path(path: &str) -> Result<(), VerifyError> {
    if path.is_empty() {
        return Err(VerifyError::UnsafePath {
            path: path.to_owned(),
            reason: UnsafePathReason::Empty,
        });
    }
    if path.starts_with('/') || Path::new(path).is_absolute() {
        return Err(VerifyError::UnsafePath {
            path: path.to_owned(),
            reason: UnsafePathReason::Absolute,
        });
    }
    if path.contains('\\') {
        return Err(VerifyError::Backslash {
            path: path.to_owned(),
        });
    }
    if path.chars().any(char::is_control) {
        return Err(VerifyError::ControlChar {
            path: path.to_owned(),
        });
    }
    for component in path.split('/') {
        if component.is_empty() {
            return Err(VerifyError::UnsafePath {
                path: path.to_owned(),
                reason: UnsafePathReason::EmptyComponent,
            });
        }
        if component == "." || component == ".." {
            return Err(VerifyError::Traversal {
                path: path.to_owned(),
            });
        }
        if component.ends_with('.') || component.ends_with(' ') {
            return Err(VerifyError::UnsafePath {
                path: path.to_owned(),
                reason: UnsafePathReason::TrailingDotOrSpace,
            });
        }
        let stem = component
            .split('.')
            .next()
            .unwrap_or(component)
            .to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || stem
                .strip_prefix("COM")
                .or_else(|| stem.strip_prefix("LPT"))
                .is_some_and(|suffix| {
                    matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
                });
        if reserved {
            return Err(VerifyError::UnsafePath {
                path: path.to_owned(),
                reason: UnsafePathReason::ReservedName,
            });
        }
        if !component
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | '{' | '}'))
        {
            return Err(VerifyError::UnsafePath {
                path: path.to_owned(),
                reason: UnsafePathReason::NonPortableName,
            });
        }
    }
    Ok(())
}

fn parse_manifest_files(manifest: &Value) -> Result<Vec<ManifestFile>, VerifyError> {
    let entries = manifest
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| VerifyError::MalformedManifest {
            message: "files must be an array".to_owned(),
        })?;
    entries
        .iter()
        .map(|entry| {
            let object = entry
                .as_object()
                .ok_or_else(|| VerifyError::MalformedManifest {
                    message: "files entries must be objects".to_owned(),
                })?;
            if object.len() != 2 || !object.contains_key("path") || !object.contains_key("sha256") {
                return Err(VerifyError::MalformedManifest {
                    message: "files entries must contain only path and sha256".to_owned(),
                });
            }
            let path = object["path"]
                .as_str()
                .ok_or_else(|| VerifyError::MalformedManifest {
                    message: "file path must be a string".to_owned(),
                })?;
            let sha256 =
                object["sha256"]
                    .as_str()
                    .ok_or_else(|| VerifyError::MalformedManifest {
                        message: "file sha256 must be a string".to_owned(),
                    })?;
            if !is_sha256(sha256) {
                return Err(VerifyError::MalformedManifest {
                    message: format!("invalid sha256 for {path}"),
                });
            }
            Ok(ManifestFile {
                path: path.to_owned(),
                sha256: sha256.to_owned(),
            })
        })
        .collect()
}

fn verify_declared_paths(files: &[ManifestFile]) -> Result<(), VerifyError> {
    let mut exact = BTreeSet::new();
    let mut folded = BTreeMap::<String, String>::new();
    for file in files {
        validate_relative_path(&file.path)?;
        if !exact.insert(file.path.clone()) {
            return Err(VerifyError::DuplicatePath {
                path: file.path.clone(),
            });
        }
        if let Some(first) = folded.insert(file.path.to_ascii_lowercase(), file.path.clone()) {
            return Err(VerifyError::CaseCollision {
                first,
                second: file.path.clone(),
            });
        }
    }
    Ok(())
}

fn verify_inventory(
    actual_files: &BTreeSet<String>,
    actual_dirs: &BTreeSet<String>,
    manifest_files: &[ManifestFile],
) -> Result<(), VerifyError> {
    let declared: BTreeSet<&str> = manifest_files
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    let pinned: BTreeSet<&str> = BUNDLE_FILES.iter().map(|entry| entry.path).collect();
    if let Some(extra) = declared.difference(&pinned).next() {
        return Err(VerifyError::ExtraFile {
            path: (**extra).to_owned(),
        });
    }
    if let Some(missing) = pinned.difference(&declared).next() {
        return Err(VerifyError::ManifestInventoryMismatch {
            message: format!("pinned file is not declared: {missing}"),
        });
    }

    let mut expected: BTreeSet<String> = declared.iter().map(|path| (*path).to_owned()).collect();
    expected.insert(AUTHORITY_MANIFEST_PATH.to_owned());
    if let Some(path) = expected.intersection(actual_dirs).next() {
        return Err(VerifyError::NonRegularFile {
            path: path.clone(),
            kind: "directory",
        });
    }
    if let Some(missing) = expected.difference(actual_files).next() {
        return Err(VerifyError::MissingFile {
            path: missing.clone(),
        });
    }
    if let Some(extra) = actual_files.difference(&expected).next() {
        return Err(VerifyError::UnlistedFile {
            path: extra.clone(),
        });
    }

    let required_dirs: BTreeSet<String> = expected
        .iter()
        .flat_map(|path| {
            let mut parents = Vec::new();
            let mut current = PathBuf::from(path);
            while current.pop() && !current.as_os_str().is_empty() {
                parents.push(current.to_string_lossy().replace('\\', "/"));
            }
            parents
        })
        .collect();
    if let Some(extra) = actual_dirs.difference(&required_dirs).next() {
        return Err(VerifyError::ExtraFile {
            path: extra.clone(),
        });
    }
    if let Some(missing) = required_dirs.difference(actual_dirs).next() {
        return Err(VerifyError::MissingFile {
            path: missing.clone(),
        });
    }
    Ok(())
}

fn compare_digest(path: &str, bytes: &[u8], expected: &str) -> Option<VerifyError> {
    let actual = format!("{:x}", Sha256::digest(bytes));
    (actual != expected).then(|| VerifyError::DigestMismatch {
        path: path.to_owned(),
        expected: expected.to_owned(),
        actual,
    })
}

fn verify_manifest_fields(manifest: &Value) -> Result<(), VerifyError> {
    let object = manifest
        .as_object()
        .ok_or_else(|| VerifyError::MalformedManifest {
            message: "top level must be an object".to_owned(),
        })?;
    let exact = [
        (
            "bundle_schema_identity",
            serde_json::json!(BUNDLE_SCHEMA_IDENTITY),
        ),
        ("bundle_semver", serde_json::json!(BUNDLE_SEMVER)),
        ("component_closure", serde_json::json!(COMPONENT_CLOSURE)),
        (
            "consumer_identifiers",
            serde_json::json!(CONSUMER_IDENTIFIERS),
        ),
        ("generator_identity", serde_json::json!(GENERATOR_IDENTITY)),
        (
            "observer_protocol_version",
            serde_json::json!(OBSERVER_PROTOCOL_VERSION),
        ),
        (
            "openapi_document_version",
            serde_json::json!(OPENAPI_DOCUMENT_VERSION),
        ),
        (
            "openapi_spec_version",
            serde_json::json!(OPENAPI_SPEC_VERSION),
        ),
        ("operation_ids", serde_json::json!(OPERATION_IDS)),
        ("projection_path", serde_json::json!(PROJECTION_PATH)),
        ("schema_dialect_uri", serde_json::json!(SCHEMA_DIALECT_URI)),
        (
            "supported_response_variants",
            serde_json::json!(SUPPORTED_RESPONSE_VARIANTS),
        ),
    ];
    for (field, expected) in exact {
        if object.get(field) != Some(&expected) {
            return Err(VerifyError::ManifestFieldMismatch {
                field: field.to_owned(),
            });
        }
    }
    let targets = object
        .get("windows_linux_rollout_targets")
        .and_then(Value::as_array)
        .ok_or_else(|| VerifyError::ManifestFieldMismatch {
            field: "windows_linux_rollout_targets".to_owned(),
        })?;
    let target_ids: Option<Vec<&str>> = targets
        .iter()
        .map(|target| target.get("consumer_identifier").and_then(Value::as_str))
        .collect();
    if target_ids.as_deref() != Some(INITIAL_TARGETS) {
        return Err(VerifyError::ManifestFieldMismatch {
            field: "windows_linux_rollout_targets".to_owned(),
        });
    }
    let inputs = object
        .get("generator_inputs")
        .and_then(Value::as_array)
        .ok_or_else(|| VerifyError::ManifestFieldMismatch {
            field: "generator_inputs".to_owned(),
        })?;
    if inputs.is_empty()
        || inputs.iter().any(|entry| {
            let Some(entry) = entry.as_object() else {
                return true;
            };
            !matches!(entry.get("id"), Some(Value::String(_)))
                || !matches!(entry.get("path"), Some(Value::String(_)))
                || !matches!(entry.get("role"), Some(Value::String(_)))
                || !entry
                    .get("sha256")
                    .and_then(Value::as_str)
                    .is_some_and(is_sha256)
        })
    {
        return Err(VerifyError::ManifestFieldMismatch {
            field: "generator_inputs".to_owned(),
        });
    }
    for required in ["audited_consumer_revisions", "vocabularies"] {
        if !object.get(required).is_some_and(Value::is_array) {
            return Err(VerifyError::ManifestFieldMismatch {
                field: required.to_owned(),
            });
        }
    }
    Ok(())
}

fn verify_projection(path: &Path) -> Result<(), VerifyError> {
    let bytes = read_file(path, PROJECTION_PATH)?;
    let projection = parse_json(&bytes, PROJECTION_PATH)?;
    let paths = projection
        .get("paths")
        .and_then(Value::as_object)
        .ok_or_else(|| VerifyError::ProjectionMismatch {
            message: "paths must be an object".to_owned(),
        })?;
    let methods = [
        "get", "put", "post", "delete", "options", "head", "patch", "trace",
    ];
    let mut operations = BTreeMap::<String, (String, String)>::new();
    for (operation_path, item) in paths {
        let Some(item) = item.as_object() else {
            continue;
        };
        for method in methods {
            let Some(operation) = item.get(method) else {
                continue;
            };
            let operation_id = operation
                .get("operationId")
                .and_then(Value::as_str)
                .ok_or_else(|| VerifyError::ProjectionMismatch {
                    message: format!("{method} {operation_path} lacks operationId"),
                })?;
            if operations
                .insert(
                    operation_id.to_owned(),
                    (method.to_ascii_uppercase(), operation_path.clone()),
                )
                .is_some()
            {
                return Err(VerifyError::ProjectionMismatch {
                    message: format!("duplicate operationId {operation_id}"),
                });
            }
        }
    }
    if operations
        .keys()
        .map(String::as_str)
        .ne(OPERATION_IDS.iter().copied())
    {
        return Err(VerifyError::ProjectionMismatch {
            message: "operation ID set differs from the authority pin".to_owned(),
        });
    }
    for mapping in WINDOWS_OPERATION_MAPPINGS {
        let expected = (mapping.method.to_owned(), mapping.path.to_owned());
        if operations.get(mapping.operation_id) != Some(&expected) {
            return Err(VerifyError::ProjectionMismatch {
                message: format!("mapping differs for {}", mapping.operation_id),
            });
        }
    }
    Ok(())
}

fn verify_id_document(
    path: &Path,
    label: &str,
    array_field: &str,
    full: &[&str],
    adopted: &[&str],
    fixture: bool,
) -> Result<(), VerifyError> {
    let bytes = read_file(path, label)?;
    let document = parse_json(&bytes, label)?;
    let records = document
        .get(array_field)
        .and_then(Value::as_array)
        .ok_or_else(|| id_error(fixture, format!("{array_field} must be an array")))?;
    let mut ids = BTreeSet::new();
    for record in records {
        let id = record
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| id_error(fixture, "record lacks string id".to_owned()))?;
        if !ids.insert(id) {
            return Err(id_error(fixture, format!("duplicate ID {id}")));
        }
    }
    if ids.iter().copied().ne(full.iter().copied()) {
        return Err(id_error(
            fixture,
            "full ID set differs from the authority pin".to_owned(),
        ));
    }
    if adopted.iter().any(|id| !ids.contains(id)) {
        return Err(id_error(
            fixture,
            "adopted ID is absent from the full set".to_owned(),
        ));
    }
    Ok(())
}

fn id_error(fixture: bool, message: String) -> VerifyError {
    if fixture {
        VerifyError::FixtureSetMismatch { message }
    } else {
        VerifyError::VectorSetMismatch { message }
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn io_error(path: &Path, error: std::io::Error) -> VerifyError {
    VerifyError::Io {
        path: path.display().to_string(),
        message: error.to_string(),
    }
}

fn file_kind(metadata: &fs::Metadata) -> &'static str {
    let kind = metadata.file_type();
    if kind.is_symlink() {
        "symlink"
    } else if kind.is_dir() {
        "directory"
    } else if kind.is_file() {
        "regular file"
    } else {
        "special file"
    }
}

#[cfg(unix)]
fn verify_mode(path: &Path, metadata: &fs::Metadata, directory: bool) -> Result<(), VerifyError> {
    use std::os::unix::fs::MetadataExt;
    let mode = metadata.mode() & 0o7777;
    let forbidden = if directory {
        mode & 0o7000
    } else {
        mode & 0o7111
    };
    if forbidden != 0 {
        return Err(VerifyError::InvalidFileMode {
            path: path.display().to_string(),
            mode,
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_mode(
    _path: &Path,
    _metadata: &fs::Metadata,
    _directory: bool,
) -> Result<(), VerifyError> {
    Ok(())
}
