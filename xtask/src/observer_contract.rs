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

use crate::artifact_fs::{self, ArtifactFsError, UnixModePolicy};

pub use crate::artifact_fs::{validate_relative_path, UnsafePathReason};

pub const ADOPTION_SCHEMA_VERSION: u64 = 1;
pub const CONSUMER_IDENTIFIER: &str = "solstone-windows";
pub const AUTHORITY_REPOSITORY: &str = "https://github.com/solpbc/solstone-journal";
pub const AUTHORITY_COMMIT: &str = "dd76c42a21a7892fccc1b0cfa790ce1ad31bf78b";
pub const BUNDLE_SEMVER: &str = "9.0.0";
pub const ARCHIVE_SHA256: &str = "8711c7e811cd83f0bdb38d4a8a525c0c40e9bccf96716e7b567829efe7b97a89";
pub const ARCHIVE_SIZE_BYTES: u64 = 5_671;
/// All authority paths are relative to the explicit bundle directory.
pub const AUTHORITY_MANIFEST_PATH: &str = "manifest.json";
pub const AUTHORITY_MANIFEST_SHA256: &str =
    "93b2a5a1604f1ba6fad30624c00cac98ea3d04a80cb1718886cf665c16f58834";
pub const GENERATOR_IDENTITY: &str =
    "solstone.repository_contracts.observer_client_contract_bundle.v1";
pub const BUNDLE_SCHEMA_IDENTITY: &str = "solstone.observer-client-contract-bundle.schema.v1";
pub const SCHEMA_DIALECT_URI: &str = "https://json-schema.org/draft/2020-12/schema";
pub const OPENAPI_DOCUMENT_VERSION: &str = "1.0.0";
pub const OPENAPI_SPEC_VERSION: &str = "3.1.0";
pub const PROJECTION_PATH: &str = "projection.openapi.json";
pub const OBSERVER_PROTOCOL_VERSION: u64 = 3;
pub const SUPPORTED_RESPONSE_VARIANTS: &[u64] = &[3];
pub const SCOPE_RATIONALE: &str = "This ingest-triad bundle projects only the four Rust-served linked-device devices/ingest operations. Pairing and root SSE are live but out of scope; deferred legacy observer operations are not projected.";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FilePin {
    pub path: &'static str,
    pub sha256: &'static str,
}

pub const BUNDLE_FILES: &[FilePin] = &[
    FilePin {
        path: "consumer-audit.json",
        sha256: "522a1d0417086a85d755e058b9043e519665c63eace50b146bab1f1157ff0cdf",
    },
    FilePin {
        path: "fixtures/wire-behavior.json",
        sha256: "65917fe91620f517d9988664e24b559722acbd6fb798bc518d1e092ef8f8771c",
    },
    FilePin {
        path: "projection.openapi.json",
        sha256: "7780c76380ae59504c069e388042401b050c60caf9027e1cf22a4cce0c19103e",
    },
    FilePin {
        path: "vectors.json",
        sha256: "fd99f21f225573cd2aead2217e9716237e7b4f66bd36a225d0e7019ded4a222b",
    },
];

pub const COMPONENT_CLOSURE: &[&str] = &["Error", "SegmentFile", "SegmentItem", "SegmentsEnvelope"];
pub const CONSUMER_IDENTIFIERS: &[&str] =
    &["solstone-browser", "solstone-linux", "solstone-windows"];
/// This order is pinned to the authority manifest, which is intentionally not lexical.
pub const OPERATION_IDS: &[&str] = &[
    "observer.ingestUpload",
    "observer.ingestManifest",
    "observer.ingestManifestDay",
    "observer.ingestSegments",
];
/// Adoption coverage is sorted independently of the authority's manifest order.
pub const ADOPTED_OPERATION_IDS: &[&str] = &[
    "observer.ingestManifest",
    "observer.ingestManifestDay",
    "observer.ingestSegments",
    "observer.ingestUpload",
];

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct OperationMapping {
    pub operation_id: &'static str,
    pub method: &'static str,
    pub path: &'static str,
}

pub const WINDOWS_OPERATION_MAPPINGS: &[OperationMapping] = &[
    OperationMapping {
        operation_id: "observer.ingestUpload",
        method: "POST",
        path: "/app/devices/ingest",
    },
    OperationMapping {
        operation_id: "observer.ingestManifest",
        method: "GET",
        path: "/app/devices/ingest/manifest",
    },
    OperationMapping {
        operation_id: "observer.ingestManifestDay",
        method: "GET",
        path: "/app/devices/ingest/manifest/{day}",
    },
    OperationMapping {
        operation_id: "observer.ingestSegments",
        method: "GET",
        path: "/app/devices/ingest/segments/{day}",
    },
];

pub const FIXTURE_IDS: &[&str] = &[
    "declared.observer.ingestUpload.status.collision",
    "declared.observer.ingestUpload.status.conflict",
    "declared.observer.ingestUpload.status.duplicate",
    "declared.observer.ingestUpload.status.failed",
    "declared.observer.ingestUpload.status.ok",
];
pub const VECTOR_IDS: &[&str] = &[
    "observer.ingestUpload.status.collision",
    "observer.ingestUpload.status.conflict",
    "observer.ingestUpload.status.duplicate",
    "observer.ingestUpload.status.failed",
    "observer.ingestUpload.status.ok",
];

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
        expected: String,
        actual: String,
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
        expected: String,
        actual: String,
    },
    ManifestFieldMismatch {
        field: String,
        expected: String,
        actual: String,
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
            Self::AdoptionFieldMismatch {
                field,
                expected,
                actual,
            } => write!(
                f,
                "adoption field mismatch for {field}: expected {expected}, got {actual}"
            ),
            Self::AdoptionCoverageDuplicate { field, id } => {
                write!(f, "duplicate adoption coverage ID in {field}: {id}")
            }
            Self::AdoptionCoverageUnsorted { field } => {
                write!(f, "adoption coverage is not sorted: {field}")
            }
            Self::AdoptionCoverageMismatch {
                field,
                expected,
                actual,
            } => write!(
                f,
                "adoption coverage mismatch for {field}: expected {expected}, got {actual}"
            ),
            Self::ManifestFieldMismatch {
                field,
                expected,
                actual,
            } => write!(
                f,
                "manifest field mismatch for {field}: expected {expected}, got {actual}"
            ),
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

impl From<ArtifactFsError> for VerifyError {
    fn from(error: ArtifactFsError) -> Self {
        match error {
            ArtifactFsError::Io { path, message } => Self::Io { path, message },
            ArtifactFsError::UnsafePath { path, reason } => Self::UnsafePath { path, reason },
            ArtifactFsError::Traversal { path } => Self::Traversal { path },
            ArtifactFsError::Backslash { path } => Self::Backslash { path },
            ArtifactFsError::ControlChar { path } => Self::ControlChar { path },
            ArtifactFsError::CaseCollision { first, second } => {
                Self::CaseCollision { first, second }
            }
            ArtifactFsError::NonRegularFile { path, kind } => Self::NonRegularFile { path, kind },
            ArtifactFsError::ReparsePoint { path } => Self::NonRegularFile {
                path,
                kind: "reparse point",
            },
            ArtifactFsError::UnsafeResolution { path } => Self::NonRegularFile {
                path,
                kind: "unsafe resolution",
            },
            ArtifactFsError::InvalidFileMode { path, mode } => Self::InvalidFileMode { path, mode },
        }
    }
}

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
    verify_projection(
        &bundle_dir.join(PROJECTION_PATH),
        WINDOWS_OPERATION_MAPPINGS,
    )?;
    verify_id_document(
        &bundle_dir.join("fixtures/wire-behavior.json"),
        "fixtures/wire-behavior.json",
        "fixtures",
        FIXTURE_IDS,
        true,
    )?;
    verify_id_document(
        &bundle_dir.join("vectors.json"),
        "vectors.json",
        "vectors",
        VECTOR_IDS,
        false,
    )?;

    if let Some(error) = pending_digest {
        return Err(error);
    }

    Ok(VerifyReport {
        bundle_semver: BUNDLE_SEMVER,
        operation_count: OPERATION_IDS.len(),
        fixture_count: FIXTURE_IDS.len(),
        vector_count: VECTOR_IDS.len(),
    })
}

fn verify_regular_file(path: &Path, label: &str) -> Result<(), VerifyError> {
    artifact_fs::verify_regular_file(path, label, UnixModePolicy::StrictNoExecute)?;
    Ok(())
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
                    expected: json_text(&$expected),
                    actual: json_text(&record.$field),
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
            expected: json_text(&expected_files),
            actual: json_text(&actual_files),
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
        FIXTURE_IDS,
    )?;
    verify_coverage("adopted_vector_ids", &record.adopted_vector_ids, VECTOR_IDS)
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
            expected: json_text(expected),
            actual: json_text(actual),
        });
    }
    Ok(())
}

fn walk_bundle(bundle_dir: &Path) -> Result<(BTreeSet<String>, BTreeSet<String>), VerifyError> {
    let inventory =
        artifact_fs::walk_directory(bundle_dir, "bundle", UnixModePolicy::StrictNoExecute)?;
    Ok((inventory.files, inventory.directories))
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
        artifact_fs::check_case_collision(&mut folded, &file.path)?;
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
        ("scope_rationale", serde_json::json!(SCOPE_RATIONALE)),
        (
            "generator_inputs",
            serde_json::json!([{
                "id": "openapi.convey_clients",
                "path": "docs/openapi/convey-clients.json",
                "role": "openapi_source",
                "sha256": "434d103d7b2accc8d6244f9886884c6c05ce59096bd0b8ee9bfbc67d564f6563",
            }]),
        ),
        (
            "audited_consumer_revisions",
            serde_json::json!([
                { "consumer_identifier": "solstone-windows", "revision": "19c972c4fea775176cea6421ac8b87f3bb20ab42" },
                { "consumer_identifier": "solstone-linux", "revision": "1c679db1ce6f9a65db70c5aae0ca2fad677416ef" },
                { "consumer_identifier": "solstone-browser", "revision": "998c1095cd8f766dd188bece5ad6527444f8dfac" },
            ]),
        ),
        (
            "vocabularies",
            serde_json::json!([
                {
                    "classification": "closed",
                    "id": "SegmentFile.status",
                    "source_pointer": "/components/schemas/SegmentFile/properties/status",
                    "unknown_value_behavior": "reject",
                    "values": ["present", "missing", "processed"],
                },
                {
                    "classification": "closed",
                    "id": "observer.ingestUpload.status",
                    "source_pointers": [
                        "/paths/~1app~1devices~1ingest/post/responses/200/content/application~1json/schema/properties/status",
                        "/paths/~1app~1devices~1ingest/post/responses/409",
                    ],
                    "unknown_value_behavior": "reject",
                    "values": ["ok", "duplicate", "collision", "conflict", "failed"],
                },
            ]),
        ),
        (
            "windows_linux_rollout_targets",
            serde_json::json!([
                {
                    "adoption_blocker_ids": ["solstone-linux-legacy-v2-unmigrated"],
                    "consumer_identifier": "solstone-linux",
                },
                {
                    "adoption_blocker_ids": ["solstone-windows-legacy-v2-unmigrated"],
                    "consumer_identifier": "solstone-windows",
                },
            ]),
        ),
    ];
    for (field, expected) in exact {
        if object.get(field) != Some(&expected) {
            return Err(VerifyError::ManifestFieldMismatch {
                field: field.to_owned(),
                expected: expected.to_string(),
                actual: optional_json_text(object.get(field)),
            });
        }
    }
    Ok(())
}

pub fn verify_projection(path: &Path, mappings: &[OperationMapping]) -> Result<(), VerifyError> {
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
    let actual_ids: BTreeSet<&str> = operations.keys().map(String::as_str).collect();
    let expected_ids: BTreeSet<&str> = OPERATION_IDS.iter().copied().collect();
    if actual_ids != expected_ids {
        return Err(VerifyError::ProjectionMismatch {
            message: "operation ID set differs from the authority pin".to_owned(),
        });
    }
    let actual_mappings: BTreeSet<(&str, &str, &str)> = operations
        .iter()
        .map(|(id, (method, path))| (id.as_str(), method.as_str(), path.as_str()))
        .collect();
    let expected_mappings: BTreeSet<(&str, &str, &str)> = mappings
        .iter()
        .map(|mapping| (mapping.operation_id, mapping.method, mapping.path))
        .collect();
    if actual_mappings != expected_mappings {
        return Err(VerifyError::ProjectionMismatch {
            message: "operation method/path set differs from the authority pin".to_owned(),
        });
    }
    Ok(())
}

fn verify_id_document(
    path: &Path,
    label: &str,
    array_field: &str,
    expected_ids: &[&str],
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
        if fixture {
            let valid = record
                .get("schema_validation")
                .and_then(Value::as_object)
                .and_then(|validation| validation.get("valid"))
                .and_then(Value::as_bool)
                .ok_or_else(|| {
                    id_error(
                        true,
                        format!("fixture {id} lacks boolean schema_validation.valid"),
                    )
                })?;
            if id == "declared.observer.ingestUpload.status.failed" && valid {
                return Err(id_error(
                    true,
                    "failed fixture must deliberately carry schema_validation.valid=false"
                        .to_owned(),
                ));
            }
        }
    }
    let actual_ids: Vec<&str> = ids.iter().copied().collect();
    if actual_ids.iter().copied().ne(expected_ids.iter().copied()) {
        return Err(id_error(
            fixture,
            format!(
                "ID set differs from the authority pin: expected {}, got {}",
                json_text(expected_ids),
                json_text(&actual_ids)
            ),
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

fn json_text<T: serde::Serialize + ?Sized>(value: &T) -> String {
    serde_json::to_string(value).expect("public verifier values serialize as JSON")
}

fn optional_json_text(value: Option<&Value>) -> String {
    value
        .map(Value::to_string)
        .unwrap_or_else(|| "<missing>".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_reparse_identity_maps_to_the_existing_observer_variant() {
        let error = VerifyError::from(ArtifactFsError::ReparsePoint {
            path: "consumer-audit.json".to_owned(),
        });
        assert!(matches!(
            error,
            VerifyError::NonRegularFile { path, kind }
                if path == "consumer-audit.json" && kind == "reparse point"
        ));
    }
}
