// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Offline Rust release-manifest validation, classification, and rendering.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use jsonschema::{PatternOptions, Validator};
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha1::Sha1;
use sha2::{Digest, Sha256};

use crate::artifact_fs::{self, ArtifactFsError, UnixModePolicy};
use crate::version_gate;

pub const SCHEMA_SHA256: &str = "d4eabf52bcc68b56945912d351f818e5444fe8c6461cb5c48b096f87b17a875c";
pub const SCHEMA_ID: &str = "https://solpbc.org/schemas/rust-release-manifest/v1.json";
pub const SCHEMA_DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";
pub const PRODUCT: &str = "solstone-windows";
pub const TARGET_TRIPLE: &str = "x86_64-pc-windows-msvc";
pub const TARGET_PROFILE: &str = "release";
pub const TARGET_FEATURES: &[&str] = &["custom-protocol"];
pub const COMPANION_BASENAME: &str =
    "solstone-windows-x86_64-pc-windows-msvc.rust-release-manifest.json";
pub const MANIFEST_DISCLAIMER: &str =
    "MANIFEST mode verifies named sibling bytes; this is not a complete/publishable-directory classification.";

const SCHEMA_BYTES: &[u8] = include_bytes!("../../schemas/rust-release-manifest/v1.json");
const RELEASE_TOOLCHAIN_PATH: &str = "packaging/release-toolchain.json";
const DENY_PATH: &str = "deny.toml";
const CARGO_LOCK_PATH: &str = "Cargo.lock";
const FIXTURE_ROOT: &str = "xtask/tests/fixtures/rust-release-manifest";

const UNSIGNED_TOOL_NAMES: &[&str] = &[
    "rustc",
    "cargo",
    "cargo-deny",
    "dotnet",
    "vpk",
    "node",
    "npm",
    "msvc-cl",
    "windows-sdk",
    "powershell",
];
const SIGNED_ADDITIONAL_TOOL_NAMES: &[&str] = &["smctl", "signtool"];
const UNSIGNED_NATIVE_KEYS: &[&str] = &[
    "dotnet",
    "msvc-cl",
    "node",
    "npm",
    "powershell",
    "signing_mode",
    "vpk",
    "windows-sdk",
];

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u64,
    pub product: String,
    pub version: String,
    pub source_commit: String,
    pub source_dirty: bool,
    pub cargo_lock_sha256: String,
    pub rust: RustEvidence,
    pub target: TargetEvidence,
    pub native_tools: BTreeMap<String, String>,
    pub dependency_policy: DependencyPolicy,
    pub active_exceptions: Vec<String>,
    pub artifacts: Vec<ArtifactEvidence>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RustEvidence {
    pub rustc_verbose: String,
    pub cargo_version: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(tag = "kind")]
pub enum TargetEvidence {
    #[serde(rename = "compiled")]
    Compiled {
        triple: String,
        profile: String,
        features: Vec<String>,
    },
    #[serde(rename = "source")]
    Source,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DependencyPolicy {
    pub cargo_deny_version: String,
    pub deterministic_gate: String,
    pub advisory_checked_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ArtifactEvidence {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}

/// Checkout-derived authority injected into semantic checks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckoutFacts {
    pub product: String,
    pub version: String,
    pub source_commit: String,
    pub source_dirty: bool,
    pub cargo_lock_sha256: String,
    pub rustc_verbose: String,
    pub cargo_version: String,
    pub target_triple: String,
    pub target_profile: String,
    pub target_features: Vec<String>,
    pub cargo_deny_version: String,
    pub active_exceptions: Vec<String>,
    pub unsigned_native_tools: BTreeMap<String, String>,
    pub signed_native_tools: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseToolProjection {
    pub rustc_verbose: String,
    pub cargo_version: String,
    pub cargo_deny_version: String,
    pub unsigned_native_tools: BTreeMap<String, String>,
    pub signed_native_tools: BTreeMap<String, String>,
}

/// Explicit, normalized renderer input. Rendering has no ambient inputs.
#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
pub struct ReleaseEvidence {
    pub schema_version: u64,
    pub product: String,
    pub version: String,
    pub source_commit: String,
    pub source_dirty: bool,
    pub cargo_lock_sha256: String,
    pub rust: RustEvidence,
    pub target: TargetEvidence,
    pub native_tools: BTreeMap<String, String>,
    pub dependency_policy: DependencyPolicy,
    pub active_exceptions: Vec<String>,
    pub artifacts: Vec<ArtifactEvidence>,
}

impl From<Manifest> for ReleaseEvidence {
    fn from(manifest: Manifest) -> Self {
        Self {
            schema_version: manifest.schema_version,
            product: manifest.product,
            version: manifest.version,
            source_commit: manifest.source_commit,
            source_dirty: manifest.source_dirty,
            cargo_lock_sha256: manifest.cargo_lock_sha256,
            rust: manifest.rust,
            target: manifest.target,
            native_tools: manifest.native_tools,
            dependency_policy: manifest.dependency_policy,
            active_exceptions: manifest.active_exceptions,
            artifacts: manifest.artifacts,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClassificationMode {
    FixtureSelfCheck,
    SiblingBytesOnly,
    CompleteCurrentBundle,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClassifierReport {
    pub mode: ClassificationMode,
    pub artifact_count: usize,
    pub disclaimer: Option<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManifestError {
    Usage,
    SchemaDigestMismatch,
    SchemaIdentityMismatch,
    SchemaCompile,
    SchemaViolation,
    SchemaFileMismatch,
    ManifestJsonMalformed,
    ProductMismatch,
    VersionMismatch,
    SourceCommitMismatch,
    SourceDirty,
    CargoLockMismatch,
    RustcEvidenceMismatch,
    CargoVersionMismatch,
    TargetKindMismatch,
    TargetTripleMismatch,
    TargetProfileMismatch,
    TargetFeaturesMismatch,
    CargoDenyVersionMismatch,
    DeterministicGateMismatch,
    ActiveExceptionsMismatch,
    SigningModeInvalid,
    NativeToolsMismatch,
    ToolchainContractMalformed,
    ToolchainContractMismatch,
    DenyTomlMalformed,
    CheckoutFactUnavailable,
    Io {
        path: String,
    },
    UnsafePath {
        path: String,
        reason: artifact_fs::UnsafePathReason,
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
    CaseCollision {
        first: String,
        second: String,
    },
    NonRegularFile {
        path: String,
        kind: &'static str,
    },
    ReparsePoint {
        path: String,
    },
    UnsafeResolution {
        path: String,
    },
    InvalidSpecialMode {
        path: String,
        mode: u32,
    },
    ArtifactDuplicate,
    ArtifactInventoryMismatch,
    ArtifactMissing,
    ArtifactBytesMismatch,
    ArtifactSha256Mismatch,
    WrongManifestName,
    ExtraManifest,
    DirectoryNotFlat,
    MissingBundleEntry,
    UnknownBundleEntry,
    HistoricalArtifact,
    UnmanifestedRustOutput,
    LedgerJsonMalformed,
    LedgerRecordMalformed,
    LedgerPackageIdMismatch,
    LedgerPathUnsafe,
    LedgerDuplicate,
    LedgerConflict,
    LedgerVersionMalformed,
    LedgerVersionNewerThanCandidate,
    LedgerCurrentMismatch,
    ReleasesBomMissing,
    ReleasesMalformed,
    AssetsDefaultSetupForbidden,
    DeltaMismatch,
    EvidenceNotCanonical {
        field: &'static str,
    },
    EvidenceInvalid {
        field: &'static str,
    },
    RendererSerialization,
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Usage => "select exactly one rust-release-manifest check mode",
            Self::SchemaDigestMismatch => "embedded release-manifest schema digest mismatch",
            Self::SchemaIdentityMismatch => "embedded release-manifest schema identity mismatch",
            Self::SchemaCompile => "embedded release-manifest schema failed to compile",
            Self::SchemaViolation => "candidate does not satisfy the release-manifest schema",
            Self::SchemaFileMismatch => "vendored schema bytes differ from embedded schema bytes",
            Self::ManifestJsonMalformed => "candidate manifest is not valid JSON",
            Self::ProductMismatch => "manifest product does not match checkout authority",
            Self::VersionMismatch => "manifest version does not match checkout authority",
            Self::SourceCommitMismatch => {
                "manifest source commit does not match checkout authority"
            }
            Self::SourceDirty => "checkout has uncommitted source changes",
            Self::CargoLockMismatch => "manifest lock digest does not match checkout authority",
            Self::RustcEvidenceMismatch => {
                "manifest rustc evidence does not match the canonical projection"
            }
            Self::CargoVersionMismatch => "manifest cargo version does not match the tool contract",
            Self::TargetKindMismatch => "manifest target kind does not match the compiled lane",
            Self::TargetTripleMismatch => "manifest target triple does not match the compiled lane",
            Self::TargetProfileMismatch => {
                "manifest target profile does not match the compiled lane"
            }
            Self::TargetFeaturesMismatch => {
                "manifest target features do not match the compiled lane"
            }
            Self::CargoDenyVersionMismatch => {
                "manifest cargo-deny version does not match the tool contract"
            }
            Self::DeterministicGateMismatch => "manifest deterministic gate is not pass",
            Self::ActiveExceptionsMismatch => {
                "manifest active exceptions do not match repository policy"
            }
            Self::SigningModeInvalid => "manifest signing mode is invalid",
            Self::NativeToolsMismatch => {
                "manifest native-tool evidence does not match the exact projection"
            }
            Self::ToolchainContractMalformed => "release-toolchain contract is malformed",
            Self::ToolchainContractMismatch => {
                "release-toolchain contract does not match its required shape"
            }
            Self::DenyTomlMalformed => "dependency policy document is malformed",
            Self::CheckoutFactUnavailable => "a checkout authority fact could not be established",
            Self::Io { .. } => "artifact I/O failed",
            Self::UnsafePath { .. } => "artifact path is unsafe",
            Self::Traversal { .. } => "artifact path contains traversal",
            Self::Backslash { .. } => "artifact path contains a backslash",
            Self::ControlChar { .. } => "artifact path contains a control character",
            Self::CaseCollision { .. } => "artifact paths collide under ASCII case folding",
            Self::NonRegularFile { .. } => "artifact is not a regular file",
            Self::ReparsePoint { .. } => "artifact is a reparse point or symbolic link",
            Self::UnsafeResolution { .. } => "artifact containment could not be established",
            Self::InvalidSpecialMode { .. } => "artifact has a forbidden special mode bit",
            Self::ArtifactDuplicate => "manifest contains a duplicate artifact path",
            Self::ArtifactInventoryMismatch => {
                "manifest artifact inventory is not the exact current set"
            }
            Self::ArtifactMissing => "a named artifact is missing",
            Self::ArtifactBytesMismatch => "artifact byte count does not match the manifest",
            Self::ArtifactSha256Mismatch => "artifact digest does not match the manifest",
            Self::WrongManifestName => "complete bundle uses the wrong companion manifest name",
            Self::ExtraManifest => "complete bundle contains an extra companion manifest",
            Self::DirectoryNotFlat => "complete bundle contains a subdirectory",
            Self::MissingBundleEntry => "complete bundle is missing a required file",
            Self::UnknownBundleEntry => "complete bundle contains an unknown file",
            Self::HistoricalArtifact => "complete bundle contains a historical artifact",
            Self::UnmanifestedRustOutput => "complete bundle contains unmanifested release output",
            Self::LedgerJsonMalformed => "release ledger JSON is malformed",
            Self::LedgerRecordMalformed => "release ledger contains a malformed record",
            Self::LedgerPackageIdMismatch => "release ledger package identifier is invalid",
            Self::LedgerPathUnsafe => "release ledger filename is unsafe",
            Self::LedgerDuplicate => "release ledger contains a duplicate version/type record",
            Self::LedgerConflict => "release ledger contains conflicting version/type records",
            Self::LedgerVersionMalformed => "release ledger contains a malformed semantic version",
            Self::LedgerVersionNewerThanCandidate => {
                "release ledger contains a version newer than the candidate"
            }
            Self::LedgerCurrentMismatch => {
                "current release ledger evidence does not match artifact bytes"
            }
            Self::ReleasesBomMissing => "RELEASES is missing its UTF-8 BOM",
            Self::ReleasesMalformed => "RELEASES does not match its exact row grammar",
            Self::AssetsDefaultSetupForbidden => {
                "assets ledger references the forbidden default setup name"
            }
            Self::DeltaMismatch => {
                "current delta presence is inconsistent across manifest and ledgers"
            }
            Self::EvidenceNotCanonical { .. } => "release evidence is not canonical",
            Self::EvidenceInvalid { .. } => "release evidence is invalid",
            Self::RendererSerialization => "release evidence could not be serialized",
        };
        f.write_str(message)
    }
}

impl std::error::Error for ManifestError {}

impl From<ArtifactFsError> for ManifestError {
    fn from(error: ArtifactFsError) -> Self {
        match error {
            ArtifactFsError::Io { path, .. } => Self::Io { path },
            ArtifactFsError::UnsafePath { path, reason } => Self::UnsafePath { path, reason },
            ArtifactFsError::Traversal { path } => Self::Traversal { path },
            ArtifactFsError::Backslash { path } => Self::Backslash { path },
            ArtifactFsError::ControlChar { path } => Self::ControlChar { path },
            ArtifactFsError::CaseCollision { first, second } => {
                Self::CaseCollision { first, second }
            }
            ArtifactFsError::NonRegularFile { path, kind } => Self::NonRegularFile { path, kind },
            ArtifactFsError::ReparsePoint { path } => Self::ReparsePoint { path },
            ArtifactFsError::UnsafeResolution { path } => Self::UnsafeResolution { path },
            ArtifactFsError::InvalidFileMode { path, mode } => {
                Self::InvalidSpecialMode { path, mode }
            }
        }
    }
}

static COMPILED_SCHEMA: OnceLock<Result<Validator, ManifestError>> = OnceLock::new();

fn compiled_schema() -> Result<&'static Validator, ManifestError> {
    COMPILED_SCHEMA
        .get_or_init(compile_schema)
        .as_ref()
        .map_err(Clone::clone)
}

fn compile_schema() -> Result<Validator, ManifestError> {
    if sha256_hex(SCHEMA_BYTES) != SCHEMA_SHA256 {
        return Err(ManifestError::SchemaDigestMismatch);
    }
    let schema: Value =
        serde_json::from_slice(SCHEMA_BYTES).map_err(|_| ManifestError::SchemaIdentityMismatch)?;
    if schema.get("$id").and_then(Value::as_str) != Some(SCHEMA_ID)
        || schema.get("$schema").and_then(Value::as_str) != Some(SCHEMA_DIALECT)
    {
        return Err(ManifestError::SchemaIdentityMismatch);
    }
    compile_schema_value(&schema)
}

fn compile_schema_value(schema: &Value) -> Result<Validator, ManifestError> {
    jsonschema::draft202012::options()
        .with_pattern_options(PatternOptions::fancy_regex())
        .should_validate_formats(true)
        .should_ignore_unknown_formats(false)
        .build(schema)
        .map_err(|_| ManifestError::SchemaCompile)
}

pub fn verify_vendored_schema(root: &Path) -> Result<(), ManifestError> {
    let bytes = fs::read(root.join("schemas/rust-release-manifest/v1.json"))
        .map_err(|_| ManifestError::SchemaFileMismatch)?;
    if bytes != SCHEMA_BYTES || sha256_hex(&bytes) != SCHEMA_SHA256 {
        return Err(ManifestError::SchemaFileMismatch);
    }
    compiled_schema().map(|_| ())
}

pub fn validate_manifest_bytes(bytes: &[u8]) -> Result<Manifest, ManifestError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| ManifestError::ManifestJsonMalformed)?;
    compiled_schema()?
        .validate(&value)
        .map_err(|_| ManifestError::SchemaViolation)?;
    serde_json::from_value(value).map_err(|_| ManifestError::SchemaViolation)
}

pub fn project_release_toolchain(root: &Path) -> Result<ReleaseToolProjection, ManifestError> {
    let bytes = fs::read(root.join(RELEASE_TOOLCHAIN_PATH))
        .map_err(|_| ManifestError::ToolchainContractMalformed)?;
    let contract: Value =
        serde_json::from_slice(&bytes).map_err(|_| ManifestError::ToolchainContractMalformed)?;
    let top = contract
        .as_object()
        .ok_or(ManifestError::ToolchainContractMalformed)?;
    exact_object_keys(top, &["schema", "groups", "tools"])?;
    if top.get("schema").and_then(Value::as_str) != Some("solstone.release-toolchain.v1") {
        return Err(ManifestError::ToolchainContractMismatch);
    }

    let groups = top
        .get("groups")
        .and_then(Value::as_object)
        .ok_or(ManifestError::ToolchainContractMalformed)?;
    exact_object_keys(groups, &["unsigned", "signedAdditional"])?;
    require_string_array(groups.get("unsigned"), UNSIGNED_TOOL_NAMES)?;
    require_string_array(groups.get("signedAdditional"), SIGNED_ADDITIONAL_TOOL_NAMES)?;

    let tools = top
        .get("tools")
        .and_then(Value::as_object)
        .ok_or(ManifestError::ToolchainContractMalformed)?;
    let all_tools: Vec<&str> = UNSIGNED_TOOL_NAMES
        .iter()
        .chain(SIGNED_ADDITIONAL_TOOL_NAMES)
        .copied()
        .collect();
    exact_object_keys(tools, &all_tools)?;

    let rustc = expected_object(tools, "rustc", &["release", "host"])?;
    let rust_release = required_string(rustc, "release")?;
    let rust_host = required_string(rustc, "host")?;
    let rustc_verbose = format!("release: {rust_release}\nhost: {rust_host}");
    let cargo_version = expected_version(tools, "cargo")?;
    let cargo_deny_version = expected_version(tools, "cargo-deny")?;

    let mut unsigned = BTreeMap::new();
    unsigned.insert("dotnet".to_owned(), expected_version(tools, "dotnet")?);

    let vpk = expected_object(tools, "vpk", &["packageId", "version", "command"])?;
    if required_string(vpk, "packageId")? != "vpk" || required_string(vpk, "command")? != "vpk" {
        return Err(ManifestError::ToolchainContractMismatch);
    }
    unsigned.insert(
        "vpk".to_owned(),
        required_string(vpk, "version")?.to_owned(),
    );
    unsigned.insert("node".to_owned(), expected_version(tools, "node")?);
    unsigned.insert("npm".to_owned(), expected_version(tools, "npm")?);

    let msvc = expected_object(
        tools,
        "msvc-cl",
        &["compilerVersion", "toolsetVersion", "host", "target"],
    )?;
    unsigned.insert(
        "msvc-cl".to_owned(),
        format!(
            "{} toolset {} {}->{}",
            required_string(msvc, "compilerVersion")?,
            required_string(msvc, "toolsetVersion")?,
            required_string(msvc, "host")?,
            required_string(msvc, "target")?
        ),
    );
    unsigned.insert(
        "windows-sdk".to_owned(),
        expected_version(tools, "windows-sdk")?,
    );
    let powershell = expected_object(tools, "powershell", &["majorMinor"])?;
    unsigned.insert(
        "powershell".to_owned(),
        required_string(powershell, "majorMinor")?.to_owned(),
    );
    unsigned.insert("signing_mode".to_owned(), "unsigned".to_owned());

    let mut signed = unsigned.clone();
    signed.insert("signing_mode".to_owned(), "signed-verified".to_owned());
    signed.insert("smctl".to_owned(), expected_version(tools, "smctl")?);
    let signtool = expected_object(
        tools,
        "signtool",
        &["path", "productVersion", "originalFilename"],
    )?;
    let signtool_path = required_string(signtool, "path")?;
    let basename = signtool_path.rsplit(['\\', '/']).next().unwrap_or("");
    if basename != "signtool.exe"
        || required_string(signtool, "originalFilename")? != "SIGNTOOL.EXE"
    {
        return Err(ManifestError::ToolchainContractMismatch);
    }
    signed.insert(
        "signtool".to_owned(),
        format!(
            "productVersion {}",
            required_string(signtool, "productVersion")?
        ),
    );

    Ok(ReleaseToolProjection {
        rustc_verbose,
        cargo_version,
        cargo_deny_version,
        unsigned_native_tools: unsigned,
        signed_native_tools: signed,
    })
}

fn exact_object_keys(object: &Map<String, Value>, expected: &[&str]) -> Result<(), ManifestError> {
    let actual: BTreeSet<&str> = object.keys().map(String::as_str).collect();
    let expected: BTreeSet<&str> = expected.iter().copied().collect();
    if actual != expected {
        return Err(ManifestError::ToolchainContractMismatch);
    }
    Ok(())
}

fn require_string_array(value: Option<&Value>, expected: &[&str]) -> Result<(), ManifestError> {
    let values = value
        .and_then(Value::as_array)
        .ok_or(ManifestError::ToolchainContractMalformed)?;
    let actual: Option<Vec<&str>> = values.iter().map(Value::as_str).collect();
    if actual.as_deref() != Some(expected) {
        return Err(ManifestError::ToolchainContractMismatch);
    }
    Ok(())
}

fn expected_object<'a>(
    tools: &'a Map<String, Value>,
    tool: &str,
    expected_keys: &[&str],
) -> Result<&'a Map<String, Value>, ManifestError> {
    let tool = tools
        .get(tool)
        .and_then(Value::as_object)
        .ok_or(ManifestError::ToolchainContractMalformed)?;
    exact_object_keys(tool, &["expected", "observation", "repair"])?;
    if tool.get("observation").and_then(Value::as_str).is_none()
        || tool.get("repair").and_then(Value::as_str).is_none()
    {
        return Err(ManifestError::ToolchainContractMalformed);
    }
    let expected = tool
        .get("expected")
        .and_then(Value::as_object)
        .ok_or(ManifestError::ToolchainContractMalformed)?;
    exact_object_keys(expected, expected_keys)?;
    Ok(expected)
}

fn expected_version(tools: &Map<String, Value>, tool: &str) -> Result<String, ManifestError> {
    let expected = expected_object(tools, tool, &["version"])?;
    Ok(required_string(expected, "version")?.to_owned())
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    key: &str,
) -> Result<&'a str, ManifestError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(ManifestError::ToolchainContractMalformed)
}

pub fn read_active_exceptions(root: &Path) -> Result<Vec<String>, ManifestError> {
    let source =
        fs::read_to_string(root.join(DENY_PATH)).map_err(|_| ManifestError::DenyTomlMalformed)?;
    let document: toml::Value =
        toml::from_str(&source).map_err(|_| ManifestError::DenyTomlMalformed)?;
    let ignore = document
        .get("advisories")
        .and_then(|value| value.get("ignore"))
        .and_then(toml::Value::as_array)
        .ok_or(ManifestError::DenyTomlMalformed)?;
    let mut ids = BTreeSet::new();
    for entry in ignore {
        let id = match entry {
            toml::Value::String(id) => id.as_str(),
            toml::Value::Table(table) => table
                .get("id")
                .and_then(toml::Value::as_str)
                .ok_or(ManifestError::DenyTomlMalformed)?,
            _ => return Err(ManifestError::DenyTomlMalformed),
        };
        if id.is_empty() || !ids.insert(id.to_owned()) {
            return Err(ManifestError::DenyTomlMalformed);
        }
    }
    Ok(ids.into_iter().collect())
}

pub fn gather_checkout_facts(
    root: &Path,
    cargo: &OsStr,
    git: &OsStr,
) -> Result<CheckoutFacts, ManifestError> {
    let version = version_gate::authoritative_version(root, cargo)
        .map_err(|_| ManifestError::CheckoutFactUnavailable)?;
    Version::parse(&version).map_err(|_| ManifestError::LedgerVersionMalformed)?;

    let commit_output = Command::new(git)
        .args(["rev-parse", "--verify", "HEAD^{commit}"])
        .current_dir(root)
        .output()
        .map_err(|_| ManifestError::CheckoutFactUnavailable)?;
    if !commit_output.status.success() {
        return Err(ManifestError::CheckoutFactUnavailable);
    }
    let source_commit = std::str::from_utf8(&commit_output.stdout)
        .map_err(|_| ManifestError::CheckoutFactUnavailable)?
        .trim_end_matches(['\r', '\n'])
        .to_owned();
    if source_commit.len() != 40
        || !source_commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ManifestError::CheckoutFactUnavailable);
    }

    let status = Command::new(git)
        .args([
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ])
        .current_dir(root)
        .output()
        .map_err(|_| ManifestError::CheckoutFactUnavailable)?;
    if !status.status.success() {
        return Err(ManifestError::CheckoutFactUnavailable);
    }
    if !status.stdout.is_empty() {
        return Err(ManifestError::SourceDirty);
    }

    let lock =
        fs::read(root.join(CARGO_LOCK_PATH)).map_err(|_| ManifestError::CheckoutFactUnavailable)?;
    let projection = project_release_toolchain(root)?;
    let active_exceptions = read_active_exceptions(root)?;
    Ok(CheckoutFacts {
        product: PRODUCT.to_owned(),
        version,
        source_commit,
        source_dirty: false,
        cargo_lock_sha256: sha256_hex(&lock),
        rustc_verbose: projection.rustc_verbose,
        cargo_version: projection.cargo_version,
        target_triple: TARGET_TRIPLE.to_owned(),
        target_profile: TARGET_PROFILE.to_owned(),
        target_features: TARGET_FEATURES
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        cargo_deny_version: projection.cargo_deny_version,
        active_exceptions,
        unsigned_native_tools: projection.unsigned_native_tools,
        signed_native_tools: projection.signed_native_tools,
    })
}

pub fn validate_semantic_binding(
    manifest: &Manifest,
    facts: &CheckoutFacts,
) -> Result<(), ManifestError> {
    Version::parse(&facts.version).map_err(|_| ManifestError::LedgerVersionMalformed)?;
    if manifest.product != facts.product || manifest.product != PRODUCT {
        return Err(ManifestError::ProductMismatch);
    }
    if manifest.version != facts.version {
        return Err(ManifestError::VersionMismatch);
    }
    if manifest.source_commit != facts.source_commit {
        return Err(ManifestError::SourceCommitMismatch);
    }
    if facts.source_dirty || manifest.source_dirty {
        return Err(ManifestError::SourceDirty);
    }
    if manifest.cargo_lock_sha256 != facts.cargo_lock_sha256 {
        return Err(ManifestError::CargoLockMismatch);
    }
    if manifest.rust.rustc_verbose.as_bytes() != facts.rustc_verbose.as_bytes() {
        return Err(ManifestError::RustcEvidenceMismatch);
    }
    if manifest.rust.cargo_version != facts.cargo_version {
        return Err(ManifestError::CargoVersionMismatch);
    }
    match &manifest.target {
        TargetEvidence::Source => return Err(ManifestError::TargetKindMismatch),
        TargetEvidence::Compiled {
            triple,
            profile,
            features,
        } => {
            if triple != &facts.target_triple {
                return Err(ManifestError::TargetTripleMismatch);
            }
            if profile != &facts.target_profile {
                return Err(ManifestError::TargetProfileMismatch);
            }
            if features != &facts.target_features || !is_strictly_sorted(features) {
                return Err(ManifestError::TargetFeaturesMismatch);
            }
        }
    }
    if manifest.dependency_policy.cargo_deny_version != facts.cargo_deny_version {
        return Err(ManifestError::CargoDenyVersionMismatch);
    }
    if manifest.dependency_policy.deterministic_gate != "pass" {
        return Err(ManifestError::DeterministicGateMismatch);
    }
    if manifest.active_exceptions != facts.active_exceptions
        || !is_strictly_sorted(&manifest.active_exceptions)
    {
        return Err(ManifestError::ActiveExceptionsMismatch);
    }
    let signing_mode = manifest
        .native_tools
        .get("signing_mode")
        .map(String::as_str);
    let expected = match signing_mode {
        Some("unsigned") => &facts.unsigned_native_tools,
        Some("signed-verified") => &facts.signed_native_tools,
        _ => return Err(ManifestError::SigningModeInvalid),
    };
    if &manifest.native_tools != expected {
        return Err(ManifestError::NativeToolsMismatch);
    }
    Ok(())
}

pub fn render_release_evidence(evidence: &ReleaseEvidence) -> Result<Vec<u8>, ManifestError> {
    let features = match &evidence.target {
        TargetEvidence::Compiled {
            triple,
            profile,
            features,
        } => {
            if triple != TARGET_TRIPLE {
                return Err(ManifestError::EvidenceInvalid {
                    field: "target.triple",
                });
            }
            if profile != TARGET_PROFILE {
                return Err(ManifestError::EvidenceInvalid {
                    field: "target.profile",
                });
            }
            features
        }
        TargetEvidence::Source => {
            return Err(ManifestError::EvidenceInvalid {
                field: "target.kind",
            });
        }
    };
    if evidence.schema_version != 1 {
        return Err(ManifestError::EvidenceInvalid {
            field: "schema_version",
        });
    }
    if evidence.product != PRODUCT {
        return Err(ManifestError::EvidenceInvalid { field: "product" });
    }
    if evidence.source_dirty {
        return Err(ManifestError::EvidenceInvalid {
            field: "source_dirty",
        });
    }
    if evidence.dependency_policy.deterministic_gate != "pass" {
        return Err(ManifestError::EvidenceInvalid {
            field: "dependency_policy.deterministic_gate",
        });
    }
    if !matches!(evidence.source_commit.len(), 40 | 64) || !is_lower_hex(&evidence.source_commit) {
        return Err(ManifestError::EvidenceInvalid {
            field: "source_commit",
        });
    }
    if evidence.cargo_lock_sha256.len() != 64 || !is_lower_hex(&evidence.cargo_lock_sha256) {
        return Err(ManifestError::EvidenceInvalid {
            field: "cargo_lock_sha256",
        });
    }
    if !features
        .iter()
        .map(String::as_str)
        .eq(TARGET_FEATURES.iter().copied())
    {
        return Err(ManifestError::EvidenceInvalid {
            field: "target.features",
        });
    }
    if !is_strictly_sorted(&evidence.active_exceptions) {
        return Err(ManifestError::EvidenceNotCanonical {
            field: "active_exceptions",
        });
    }
    let mut artifact_names = BTreeSet::new();
    for artifact in &evidence.artifacts {
        if artifact_fs::validate_relative_path(&artifact.path).is_err()
            || artifact.path.contains('/')
        {
            return Err(ManifestError::EvidenceInvalid {
                field: "artifacts.path",
            });
        }
        if !artifact_names.insert(&artifact.path) {
            return Err(ManifestError::EvidenceNotCanonical {
                field: "artifacts.path",
            });
        }
        if artifact.bytes == 0 {
            return Err(ManifestError::EvidenceInvalid {
                field: "artifacts.bytes",
            });
        }
        if artifact.sha256.len() != 64 || !is_lower_hex(&artifact.sha256) {
            return Err(ManifestError::EvidenceInvalid {
                field: "artifacts.sha256",
            });
        }
    }
    if !evidence
        .artifacts
        .windows(2)
        .all(|pair| pair[0].path < pair[1].path)
    {
        return Err(ManifestError::EvidenceNotCanonical { field: "artifacts" });
    }
    let signing_mode = evidence
        .native_tools
        .get("signing_mode")
        .map(String::as_str);
    let mut expected_keys: BTreeSet<&str> = UNSIGNED_NATIVE_KEYS.iter().copied().collect();
    match signing_mode {
        Some("unsigned") => {}
        Some("signed-verified") => {
            expected_keys.extend(SIGNED_ADDITIONAL_TOOL_NAMES.iter().copied());
        }
        _ => {
            return Err(ManifestError::EvidenceInvalid {
                field: "native_tools.signing_mode",
            });
        }
    }
    let actual_keys: BTreeSet<&str> = evidence.native_tools.keys().map(String::as_str).collect();
    if actual_keys != expected_keys {
        return Err(ManifestError::EvidenceInvalid {
            field: "native_tools",
        });
    }
    let value = serde_json::to_value(evidence).map_err(|_| ManifestError::RendererSerialization)?;
    compiled_schema()?
        .validate(&value)
        .map_err(|_| ManifestError::EvidenceInvalid { field: "schema" })?;
    let mut bytes = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"  ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut bytes, formatter);
    evidence
        .serialize(&mut serializer)
        .map_err(|_| ManifestError::RendererSerialization)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn is_strictly_sorted(values: &[String]) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1])
}

fn is_lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn sha256_hex(bytes: &[u8]) -> String {
    lower_hex(&Sha256::digest(bytes))
}

fn sha1_hex(bytes: &[u8]) -> String {
    lower_hex(&Sha1::digest(bytes))
}

fn lower_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;

    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

pub fn expected_artifact_names(version: &str, has_delta: bool) -> BTreeSet<String> {
    let mut names = BTreeSet::from([
        "RELEASES".to_owned(),
        format!("Solstone-{version}-full.nupkg"),
        "Solstone-win-Portable.zip".to_owned(),
        "assets.win.json".to_owned(),
        "releases.win.json".to_owned(),
        version_gate::setup_exe_name(version),
    ]);
    if has_delta {
        names.insert(format!("Solstone-{version}-delta.nupkg"));
    }
    names
}

pub fn validate_manifest_with_facts(
    manifest_path: &Path,
    facts: &CheckoutFacts,
) -> Result<ClassifierReport, ManifestError> {
    let base = match manifest_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let manifest_name = manifest_path.file_name().and_then(OsStr::to_str).ok_or(
        ManifestError::UnsafeResolution {
            path: "manifest".to_owned(),
        },
    )?;
    let resolver =
        artifact_fs::ContainedRoot::new(base, "manifest directory", UnixModePolicy::AllowExecute)?;
    let manifest = read_manifest(&resolver, manifest_name)?;
    validate_semantic_binding(&manifest, facts)?;
    let has_delta = validate_manifest_inventory(&manifest, &facts.version)?;
    verify_artifacts(&resolver, &manifest.artifacts)?;
    validate_ledgers(&resolver, &facts.version, has_delta)?;
    Ok(ClassifierReport {
        mode: ClassificationMode::SiblingBytesOnly,
        artifact_count: manifest.artifacts.len(),
        disclaimer: Some(MANIFEST_DISCLAIMER),
    })
}

pub fn validate_release_dir_with_facts(
    release_dir: &Path,
    facts: &CheckoutFacts,
) -> Result<ClassifierReport, ManifestError> {
    let resolver = artifact_fs::ContainedRoot::new(
        release_dir,
        "release directory",
        UnixModePolicy::AllowExecute,
    )?;
    let inventory = artifact_fs::walk_directory(
        resolver.path(),
        "release directory",
        UnixModePolicy::AllowExecute,
    )?;
    if !inventory.directories.is_empty() {
        return Err(ManifestError::DirectoryNotFlat);
    }
    let manifest_names: Vec<&String> = inventory
        .files
        .iter()
        .filter(|name| name.ends_with(".rust-release-manifest.json"))
        .collect();
    if !inventory.files.contains(COMPANION_BASENAME) {
        if manifest_names.is_empty() {
            return Err(ManifestError::MissingBundleEntry);
        }
        return Err(ManifestError::WrongManifestName);
    }
    if manifest_names.len() != 1 {
        return Err(ManifestError::ExtraManifest);
    }

    let manifest = read_manifest(&resolver, COMPANION_BASENAME)?;
    validate_semantic_binding(&manifest, facts)?;
    let has_delta = validate_manifest_inventory(&manifest, &facts.version)?;

    let mut expected = expected_artifact_names(&facts.version, has_delta);
    expected.insert(COMPANION_BASENAME.to_owned());
    if let Some(extra) = inventory.files.difference(&expected).next() {
        if extra.ends_with(".rust-release-manifest.json") {
            return Err(ManifestError::ExtraManifest);
        }
        if artifact_version(extra).is_some_and(|version| version != facts.version) {
            return Err(ManifestError::HistoricalArtifact);
        }
        if is_release_output(extra) {
            return Err(ManifestError::UnmanifestedRustOutput);
        }
        return Err(ManifestError::UnknownBundleEntry);
    }
    if expected.difference(&inventory.files).next().is_some() {
        return Err(ManifestError::MissingBundleEntry);
    }

    verify_artifacts(&resolver, &manifest.artifacts)?;
    validate_ledgers(&resolver, &facts.version, has_delta)?;
    Ok(ClassifierReport {
        mode: ClassificationMode::CompleteCurrentBundle,
        artifact_count: manifest.artifacts.len(),
        disclaimer: None,
    })
}

fn read_manifest(
    resolver: &artifact_fs::ContainedRoot,
    relative: &str,
) -> Result<Manifest, ManifestError> {
    let bytes = resolver.read(relative, "manifest")?;
    validate_manifest_bytes(&bytes)
}

fn validate_manifest_inventory(manifest: &Manifest, version: &str) -> Result<bool, ManifestError> {
    let delta_name = format!("Solstone-{version}-delta.nupkg");
    let has_delta = manifest
        .artifacts
        .iter()
        .any(|artifact| artifact.path == delta_name);
    let expected = expected_artifact_names(version, has_delta);
    let mut actual = BTreeSet::new();
    let mut folded = BTreeMap::new();
    for artifact in &manifest.artifacts {
        artifact_fs::validate_relative_path(&artifact.path)?;
        if Path::new(&artifact.path).parent() != Some(Path::new("")) {
            return Err(ManifestError::ArtifactInventoryMismatch);
        }
        if !actual.insert(artifact.path.clone()) {
            return Err(ManifestError::ArtifactDuplicate);
        }
        artifact_fs::check_case_collision(&mut folded, &artifact.path)?;
    }
    if actual != expected {
        return Err(ManifestError::ArtifactInventoryMismatch);
    }
    Ok(has_delta)
}

fn verify_artifacts(
    resolver: &artifact_fs::ContainedRoot,
    artifacts: &[ArtifactEvidence],
) -> Result<(), ManifestError> {
    for artifact in artifacts {
        let bytes = resolver.read(&artifact.path, &artifact.path)?;
        if artifact.bytes != bytes.len() as u64 {
            return Err(ManifestError::ArtifactBytesMismatch);
        }
        if artifact.sha256 != sha256_hex(&bytes) {
            return Err(ManifestError::ArtifactSha256Mismatch);
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct ReleaseFeed {
    #[serde(rename = "Assets")]
    assets: Vec<FeedAsset>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct FeedAsset {
    #[serde(rename = "PackageId")]
    package_id: String,
    #[serde(rename = "Version")]
    version: String,
    #[serde(rename = "Type")]
    asset_type: String,
    #[serde(rename = "FileName")]
    file_name: String,
    #[serde(rename = "SHA1")]
    sha1: String,
    #[serde(rename = "SHA256")]
    sha256: String,
    #[serde(rename = "Size")]
    size: u64,
    #[serde(rename = "NotesMarkdown")]
    notes_markdown: String,
    #[serde(rename = "NotesHTML")]
    notes_html: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct AssetRecord {
    #[serde(rename = "RelativeFileName")]
    relative_file_name: String,
    #[serde(rename = "Type")]
    asset_type: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ActualFile {
    bytes: u64,
    sha1: String,
    sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReleaseRow {
    sha1: String,
    filename: String,
    size: u64,
}

fn validate_ledgers(
    resolver: &artifact_fs::ContainedRoot,
    candidate_text: &str,
    manifest_has_delta: bool,
) -> Result<(), ManifestError> {
    let candidate =
        Version::parse(candidate_text).map_err(|_| ManifestError::LedgerVersionMalformed)?;
    let full_name = format!("Solstone-{candidate_text}-full.nupkg");
    let delta_name = format!("Solstone-{candidate_text}-delta.nupkg");
    let full = digest_file(resolver, &full_name)?;
    let delta = if manifest_has_delta {
        Some(digest_file(resolver, &delta_name)?)
    } else {
        None
    };

    let feed_bytes = read_ledger(resolver, "releases.win.json")?;
    let feed: ReleaseFeed =
        serde_json::from_slice(&feed_bytes).map_err(|_| ManifestError::LedgerJsonMalformed)?;
    let feed_has_delta =
        validate_release_feed(&feed, &candidate, candidate_text, &full, delta.as_ref())?;

    let releases = read_ledger(resolver, "RELEASES")?;
    validate_releases(&releases, &candidate, candidate_text, &full, &feed)?;

    let assets_bytes = read_ledger(resolver, "assets.win.json")?;
    let assets: Vec<AssetRecord> =
        serde_json::from_slice(&assets_bytes).map_err(|_| ManifestError::LedgerJsonMalformed)?;
    let assets_has_delta = validate_assets(&assets, candidate_text)?;

    if manifest_has_delta != feed_has_delta || manifest_has_delta != assets_has_delta {
        return Err(ManifestError::DeltaMismatch);
    }
    Ok(())
}

pub fn validate_release_ledgers(
    base: &Path,
    candidate: &str,
    has_delta: bool,
) -> Result<(), ManifestError> {
    let resolver =
        artifact_fs::ContainedRoot::new(base, "release directory", UnixModePolicy::AllowExecute)?;
    validate_ledgers(&resolver, candidate, has_delta)
}

fn validate_release_feed(
    feed: &ReleaseFeed,
    candidate: &Version,
    candidate_text: &str,
    full: &ActualFile,
    delta: Option<&ActualFile>,
) -> Result<bool, ManifestError> {
    let mut records = BTreeMap::<(Version, String), FeedAsset>::new();
    for asset in &feed.assets {
        if asset.package_id != "Solstone" {
            return Err(ManifestError::LedgerPackageIdMismatch);
        }
        let version =
            Version::parse(&asset.version).map_err(|_| ManifestError::LedgerVersionMalformed)?;
        if &version > candidate {
            return Err(ManifestError::LedgerVersionNewerThanCandidate);
        }
        let suffix = match asset.asset_type.as_str() {
            "Full" => "full",
            "Delta" => "delta",
            _ => return Err(ManifestError::LedgerRecordMalformed),
        };
        let expected_name = format!("Solstone-{}-{suffix}.nupkg", asset.version);
        validate_ledger_basename(&asset.file_name)?;
        if asset.file_name != expected_name
            || !is_hex(&asset.sha1, 40)
            || !is_hex(&asset.sha256, 64)
            || asset.size == 0
        {
            return Err(ManifestError::LedgerRecordMalformed);
        }
        let key = (version, asset.asset_type.clone());
        if let Some(previous) = records.get(&key) {
            return if previous == asset {
                Err(ManifestError::LedgerDuplicate)
            } else {
                Err(ManifestError::LedgerConflict)
            };
        }
        records.insert(key, asset.clone());
    }

    let full_record = records
        .get(&(candidate.clone(), "Full".to_owned()))
        .ok_or(ManifestError::LedgerCurrentMismatch)?;
    if full_record.version != candidate_text
        || full_record.file_name != format!("Solstone-{candidate_text}-full.nupkg")
        || full_record.size != full.bytes
        || !full_record.sha1.eq_ignore_ascii_case(&full.sha1)
        || !full_record.sha256.eq_ignore_ascii_case(&full.sha256)
    {
        return Err(ManifestError::LedgerCurrentMismatch);
    }
    let delta_record = records.get(&(candidate.clone(), "Delta".to_owned()));
    match (delta_record, delta) {
        (Some(record), Some(actual))
            if record.version == candidate_text
                && record.file_name == format!("Solstone-{candidate_text}-delta.nupkg")
                && record.size == actual.bytes
                && record.sha1.eq_ignore_ascii_case(&actual.sha1)
                && record.sha256.eq_ignore_ascii_case(&actual.sha256) =>
        {
            Ok(true)
        }
        (None, None) => Ok(false),
        (Some(_), Some(_)) => Err(ManifestError::LedgerCurrentMismatch),
        _ => Ok(delta_record.is_some()),
    }
}

fn validate_releases(
    bytes: &[u8],
    candidate: &Version,
    candidate_text: &str,
    full: &ActualFile,
    feed: &ReleaseFeed,
) -> Result<(), ManifestError> {
    const BOM: &[u8] = &[0xef, 0xbb, 0xbf];
    let payload = bytes
        .strip_prefix(BOM)
        .ok_or(ManifestError::ReleasesBomMissing)?;
    let text = std::str::from_utf8(payload).map_err(|_| ManifestError::ReleasesMalformed)?;
    let text = text.strip_suffix('\n').unwrap_or(text);
    if text.is_empty() || text.contains('\r') || text.ends_with('\n') {
        return Err(ManifestError::ReleasesMalformed);
    }
    let mut rows = BTreeMap::<Version, ReleaseRow>::new();
    for line in text.split('\n') {
        let columns: Vec<&str> = line.split(' ').collect();
        if columns.len() != 3 || columns.iter().any(|column| column.is_empty()) {
            return Err(ManifestError::ReleasesMalformed);
        }
        if !is_hex(columns[0], 40) {
            return Err(ManifestError::ReleasesMalformed);
        }
        validate_ledger_basename(columns[1])?;
        let version_text = columns[1]
            .strip_prefix("Solstone-")
            .and_then(|value| value.strip_suffix("-full.nupkg"))
            .ok_or(ManifestError::ReleasesMalformed)?;
        let version =
            Version::parse(version_text).map_err(|_| ManifestError::LedgerVersionMalformed)?;
        if &version > candidate {
            return Err(ManifestError::LedgerVersionNewerThanCandidate);
        }
        let size = columns[2]
            .parse::<u64>()
            .ok()
            .filter(|size| *size > 0)
            .ok_or(ManifestError::ReleasesMalformed)?;
        let row = ReleaseRow {
            sha1: columns[0].to_owned(),
            filename: columns[1].to_owned(),
            size,
        };
        if let Some(previous) = rows.get(&version) {
            return if previous == &row {
                Err(ManifestError::LedgerDuplicate)
            } else {
                Err(ManifestError::LedgerConflict)
            };
        }
        rows.insert(version, row);
    }
    let current = rows
        .get(candidate)
        .ok_or(ManifestError::LedgerCurrentMismatch)?;
    let feed_full = feed
        .assets
        .iter()
        .find(|asset| asset.version == candidate_text && asset.asset_type == "Full")
        .ok_or(ManifestError::LedgerCurrentMismatch)?;
    if current.filename != format!("Solstone-{candidate_text}-full.nupkg")
        || current.size != full.bytes
        || !current.sha1.eq_ignore_ascii_case(&full.sha1)
        || !current.sha1.eq_ignore_ascii_case(&feed_full.sha1)
    {
        return Err(ManifestError::LedgerCurrentMismatch);
    }
    Ok(())
}

fn validate_assets(assets: &[AssetRecord], candidate: &str) -> Result<bool, ManifestError> {
    let mut by_type = BTreeMap::<String, String>::new();
    for asset in assets {
        if asset.relative_file_name == "Solstone-win-Setup.exe" {
            return Err(ManifestError::AssetsDefaultSetupForbidden);
        }
        validate_ledger_basename(&asset.relative_file_name)?;
        let expected = match asset.asset_type.as_str() {
            "Full" => format!("Solstone-{candidate}-full.nupkg"),
            "Delta" => format!("Solstone-{candidate}-delta.nupkg"),
            "Installer" => version_gate::setup_exe_name(candidate),
            "Portable" => "Solstone-win-Portable.zip".to_owned(),
            _ => return Err(ManifestError::LedgerRecordMalformed),
        };
        if asset.relative_file_name != expected {
            return Err(ManifestError::LedgerCurrentMismatch);
        }
        if let Some(previous) =
            by_type.insert(asset.asset_type.clone(), asset.relative_file_name.clone())
        {
            return if previous == asset.relative_file_name {
                Err(ManifestError::LedgerDuplicate)
            } else {
                Err(ManifestError::LedgerConflict)
            };
        }
    }
    let has_delta = by_type.contains_key("Delta");
    let expected: BTreeSet<&str> = if has_delta {
        BTreeSet::from(["Delta", "Full", "Installer", "Portable"])
    } else {
        BTreeSet::from(["Full", "Installer", "Portable"])
    };
    let actual: BTreeSet<&str> = by_type.keys().map(String::as_str).collect();
    if actual != expected {
        return Err(ManifestError::LedgerCurrentMismatch);
    }
    Ok(has_delta)
}

fn validate_ledger_basename(path: &str) -> Result<(), ManifestError> {
    artifact_fs::validate_relative_path(path).map_err(|_| ManifestError::LedgerPathUnsafe)?;
    if Path::new(path).parent() != Some(Path::new("")) {
        return Err(ManifestError::LedgerPathUnsafe);
    }
    Ok(())
}

fn digest_file(
    resolver: &artifact_fs::ContainedRoot,
    name: &str,
) -> Result<ActualFile, ManifestError> {
    let bytes = match resolver.read(name, name) {
        Ok(bytes) => bytes,
        Err(ArtifactFsError::Io { .. }) => return Err(ManifestError::ArtifactMissing),
        Err(error) => return Err(error.into()),
    };
    Ok(ActualFile {
        bytes: bytes.len() as u64,
        sha1: sha1_hex(&bytes),
        sha256: sha256_hex(&bytes),
    })
}

fn read_ledger(
    resolver: &artifact_fs::ContainedRoot,
    name: &str,
) -> Result<Vec<u8>, ManifestError> {
    resolver.read(name, name).map_err(ManifestError::from)
}

fn is_hex(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_release_output(path: &str) -> bool {
    let folded = path.to_ascii_lowercase();
    [".nupkg", ".exe", ".zip", ".dll", ".pdb"]
        .iter()
        .any(|suffix| folded.ends_with(suffix))
}

fn artifact_version(path: &str) -> Option<String> {
    if let Some(value) = path
        .strip_prefix("Solstone-")
        .and_then(|value| value.strip_suffix("-full.nupkg"))
        .or_else(|| {
            path.strip_prefix("Solstone-")
                .and_then(|value| value.strip_suffix("-delta.nupkg"))
        })
        .or_else(|| {
            path.strip_prefix("solstone-setup-")
                .and_then(|value| value.strip_suffix(".exe"))
        })
    {
        return Version::parse(value).ok().map(|_| value.to_owned());
    }
    None
}

pub fn run_check(
    root: &Path,
    cargo: &OsStr,
    git: &OsStr,
    manifest_path: Option<&OsStr>,
    release_dir: Option<&OsStr>,
) -> Result<ClassifierReport, ManifestError> {
    match (manifest_path, release_dir) {
        (None, None) => run_self_check(root),
        (Some(_), Some(_)) => Err(ManifestError::Usage),
        (Some(path), None) => {
            let facts = gather_checkout_facts(root, cargo, git)?;
            validate_manifest_with_facts(&PathBuf::from(path), &facts)
        }
        (None, Some(path)) => {
            let facts = gather_checkout_facts(root, cargo, git)?;
            validate_release_dir_with_facts(&PathBuf::from(path), &facts)
        }
    }
}

pub fn run_self_check(root: &Path) -> Result<ClassifierReport, ManifestError> {
    verify_vendored_schema(root)?;
    let fixture = root.join(FIXTURE_ROOT);
    let release_dir = fixture.join("release-dir");
    let resolver = artifact_fs::ContainedRoot::new(
        &release_dir,
        "release directory",
        UnixModePolicy::AllowExecute,
    )?;
    let manifest = read_manifest(&resolver, COMPANION_BASENAME)?;
    let full_package = format!("Solstone-{}-full.nupkg", manifest.version);
    resolver.resolve(&full_package, "release fixture full package")?;
    let manifest_fixture = artifact_fs::ContainedRoot::new(
        &fixture.join("manifest-mode"),
        "manifest fixture directory",
        UnixModePolicy::AllowExecute,
    )?;
    manifest_fixture.resolve(&full_package, "manifest fixture full package")?;
    let facts = fixture_facts(root, &manifest)?;
    let release_report = validate_release_dir_with_facts(&release_dir, &facts)?;
    validate_manifest_with_facts(&fixture.join("manifest-mode/manifest.json"), &facts)?;

    let evidence = ReleaseEvidence::from(manifest.clone());
    let first = render_release_evidence(&evidence)?;
    let second = render_release_evidence(&evidence)?;
    if first != second {
        return Err(ManifestError::RendererSerialization);
    }

    let mut invalid_date =
        serde_json::to_value(manifest).map_err(|_| ManifestError::RendererSerialization)?;
    invalid_date["dependency_policy"]["advisory_checked_at"] = Value::String("invalid".to_owned());
    let invalid_bytes =
        serde_json::to_vec(&invalid_date).map_err(|_| ManifestError::RendererSerialization)?;
    if validate_manifest_bytes(&invalid_bytes) != Err(ManifestError::SchemaViolation) {
        return Err(ManifestError::SchemaViolation);
    }
    Ok(ClassifierReport {
        mode: ClassificationMode::FixtureSelfCheck,
        artifact_count: release_report.artifact_count,
        disclaimer: None,
    })
}

fn fixture_facts(root: &Path, manifest: &Manifest) -> Result<CheckoutFacts, ManifestError> {
    let projection = project_release_toolchain(root)?;
    let active_exceptions = read_active_exceptions(root)?;
    Ok(CheckoutFacts {
        product: PRODUCT.to_owned(),
        version: manifest.version.clone(),
        source_commit: manifest.source_commit.clone(),
        source_dirty: false,
        cargo_lock_sha256: manifest.cargo_lock_sha256.clone(),
        rustc_verbose: projection.rustc_verbose,
        cargo_version: projection.cargo_version,
        target_triple: TARGET_TRIPLE.to_owned(),
        target_profile: TARGET_PROFILE.to_owned(),
        target_features: TARGET_FEATURES
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        cargo_deny_version: projection.cargo_deny_version,
        active_exceptions,
        unsigned_native_tools: projection.unsigned_native_tools,
        signed_native_tools: projection.signed_native_tools,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_release_manifest_schema_compile_errors_fail_closed() {
        let mut schema: Value = serde_json::from_slice(SCHEMA_BYTES).unwrap();
        schema["properties"]["artifacts"]["items"]["properties"]["path"]["pattern"] =
            Value::String("(".to_owned());
        assert!(matches!(
            compile_schema_value(&schema),
            Err(ManifestError::SchemaCompile)
        ));
    }
}
