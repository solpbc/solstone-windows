// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Deterministic cargo-deny advisory policy and isolated RustSec snapshots.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use semver::Version;
use serde::Serialize;
use sha2::{Digest, Sha256};
use toml::Value;

use crate::artifact_fs::{
    self, child_process_path_text, verify_contained_path, ContainedRoot, UnixModePolicy,
};
use crate::release_clock::{Clock, UtcTimestamp};
use crate::release_exec::{CommandOutput, CommandRunner};
use crate::release_selection::SelectedAction;

pub const MIRROR_COHORT_ID: &str = "sol-controlled-rustsec-mirror-v1";
pub const MIRROR_MINISIGN_PUBKEY_SHA256: &str =
    "c9fb713fe57791afbdebddde7b334e950ce1efcc167d49daf4cc1cbd930bb122";
pub const ADVISORY_DB_RELATIVE: &str = "target/release-advisory-db";
const MAX_SNAPSHOT_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_RECEIPT_FUTURE: Duration = Duration::from_secs(5 * 60);
const REQUIRED_RECEIPT_MAX_AGE: u64 = 24 * 60 * 60;
const EXPECTED_IGNORE_IDS: [&str; 2] = ["RUSTSEC-2026-0194", "RUSTSEC-2026-0195"];

pub struct MirrorPacketInputs<'a> {
    pub locator: &'a str,
    pub receipt_path: &'a Path,
    pub public_key_path: &'a Path,
    pub minisign_program: &'a Path,
    pub expected_public_key_sha256: &'a str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FreshnessFields {
    synced_commit: String,
    utc: UtcTimestamp,
    max_age: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaterializedAdvisoryConfig {
    pub path: PathBuf,
    pub database_root: PathBuf,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AdvisorySnapshot {
    pub source_id: String,
    pub commit: String,
    pub tree_sha256: String,
    pub acquired_at: String,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AdvisoryProvenance {
    pub source_id: String,
    pub commit: String,
    pub tree_sha256: String,
    pub acquired_at: String,
    pub checked_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdvisoryError {
    MirrorLocatorInvalid,
    PublicRustsecSourceForbidden,
    FreshnessReceiptPathInvalid,
    MirrorPublicKeyPathInvalid,
    FreshnessReceiptMissing,
    FreshnessSignatureMissing,
    MirrorPublicKeyMissing,
    MirrorPublicKeyPinMismatch,
    FreshnessVerificationScratch,
    MinisignInvocationFailed,
    FreshnessSignatureInvalid,
    FreshnessTrustedCommentMissing,
    FreshnessTrustedCommentPrefix,
    FreshnessTrustedCommentFields,
    FreshnessSyncedCommitMalformed,
    FreshnessUtcMalformed,
    FreshnessMaxAgeInvalid,
    FreshnessBodyMismatch,
    FreshnessUtcFuture,
    FreshnessStale,
    FreshnessCommitMismatch,
    InvalidVersion,
    CheckoutContainment,
    PolicyMalformed,
    PolicyMismatch,
    ConfigLocationInvalid,
    ConfigMaterializationFailed,
    DatabaseRootInvalid,
    RepositoryCount,
    RepositoryName,
    SnapshotContainment,
    SnapshotDirty,
    SourceMismatch,
    CommitMalformed,
    ShallowRepository,
    RecordedTreeDigestMalformed,
    ArchiveFailed,
    ArchiveDigestMismatch,
    FetchHeadMissing,
    AcquisitionFuture,
    SnapshotStale,
    GitInvocationFailed { step: &'static str },
    AdvisoryActionInvalid,
    CargoDenyFailed,
    ClockUnavailable,
}

impl fmt::Display for AdvisoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MirrorLocatorInvalid => write!(
                formatter,
                "advisory mirror locator is missing or malformed; set SOLSTONE_ADVISORY_MIRROR_LOCATOR to the approved private Git URL whose final path component is exactly advisory-db or rustsec-advisory-db.git and retry"
            ),
            Self::PublicRustsecSourceForbidden => write!(
                formatter,
                "advisory mirror locator names the public RustSec GitHub repository; set the approved private mirror locator and retry"
            ),
            Self::FreshnessReceiptPathInvalid => write!(
                formatter,
                "advisory mirror freshness receipt path is invalid; set SOLSTONE_ADVISORY_RECEIPT to one absolute operator-controlled file and retry"
            ),
            Self::MirrorPublicKeyPathInvalid => write!(
                formatter,
                "advisory mirror public-key path is invalid; set SOLSTONE_ADVISORY_MIRROR_PUB to one absolute operator-controlled file and retry"
            ),
            Self::FreshnessReceiptMissing => write!(
                formatter,
                "advisory mirror freshness receipt is missing or unsafe; supply the current regular receipt body file and retry"
            ),
            Self::FreshnessSignatureMissing => write!(
                formatter,
                "advisory mirror freshness signature is missing or unsafe; place the current regular .minisig file beside the receipt body and retry"
            ),
            Self::MirrorPublicKeyMissing => write!(
                formatter,
                "advisory mirror public key is missing or unsafe; supply the pinned regular public-key file and retry"
            ),
            Self::MirrorPublicKeyPinMismatch => write!(
                formatter,
                "advisory mirror public key does not match the pinned mirror identity; restore the approved public-key file and retry"
            ),
            Self::FreshnessVerificationScratch => write!(
                formatter,
                "advisory mirror signature scratch could not be prepared safely; restore transaction-directory permissions and retry"
            ),
            Self::MinisignInvocationFailed => write!(
                formatter,
                "advisory mirror signature verification could not run; restore the selected minisign executable and retry"
            ),
            Self::FreshnessSignatureInvalid => write!(
                formatter,
                "advisory mirror freshness signature is invalid; acquire a current packet signed by the approved mirror key and retry"
            ),
            Self::FreshnessTrustedCommentMissing => write!(
                formatter,
                "advisory mirror freshness signature lacks a readable trusted comment; acquire a canonical signed packet and retry"
            ),
            Self::FreshnessTrustedCommentPrefix => write!(
                formatter,
                "advisory mirror freshness trusted-comment prefix is invalid; acquire a canonical signed packet and retry"
            ),
            Self::FreshnessTrustedCommentFields => write!(
                formatter,
                "advisory mirror freshness trusted-comment fields are not canonical; acquire a canonical signed packet and retry"
            ),
            Self::FreshnessSyncedCommitMalformed => write!(
                formatter,
                "advisory mirror freshness commit is not one full lowercase commit id; acquire a canonical signed packet and retry"
            ),
            Self::FreshnessUtcMalformed => write!(
                formatter,
                "advisory mirror freshness time is not canonical UTC; acquire a canonical signed packet and retry"
            ),
            Self::FreshnessMaxAgeInvalid => write!(
                formatter,
                "advisory mirror freshness max_age is not the required 86400 seconds; acquire a canonical signed packet and retry"
            ),
            Self::FreshnessBodyMismatch => write!(
                formatter,
                "advisory mirror freshness body and trusted comment disagree; acquire a canonical signed packet and retry"
            ),
            Self::FreshnessUtcFuture => write!(
                formatter,
                "advisory mirror freshness time is too far in the future; correct the host clock or acquire a current packet and retry"
            ),
            Self::FreshnessStale => write!(
                formatter,
                "advisory mirror freshness receipt is older than 24 hours; acquire a current signed packet and retry"
            ),
            Self::FreshnessCommitMismatch => write!(
                formatter,
                "advisory mirror repository HEAD differs from the signed freshness commit; acquire one matching mirror packet and retry"
            ),
            Self::InvalidVersion => write!(
                formatter,
                "advisory config version is not canonical SemVer; pass the exact cargo metadata version"
            ),
            Self::CheckoutContainment => write!(
                formatter,
                "advisory checkout containment failed; use one real checkout without links or reparse points"
            ),
            Self::PolicyMalformed => write!(
                formatter,
                "committed deny.toml advisories policy is malformed; restore the reviewed [advisories] table and retry"
            ),
            Self::PolicyMismatch => write!(
                formatter,
                "committed deny.toml advisories policy differs from the release contract; restore yanked, unmaintained, and both reviewed ignores"
            ),
            Self::ConfigLocationInvalid => write!(
                formatter,
                "advisory config location is not one new contained output beneath the checkout; recreate its real parent directory and retry"
            ),
            Self::ConfigMaterializationFailed => write!(
                formatter,
                "advisory config could not be materialized deterministically; recreate the version transaction root and retry"
            ),
            Self::DatabaseRootInvalid => write!(
                formatter,
                "isolated advisory database root is missing or unsafe; provision target/release-advisory-db as one contained directory"
            ),
            Self::RepositoryCount => write!(
                formatter,
                "isolated advisory database root does not contain exactly one RustSec repository; replace it with one URL-derived cache and retry"
            ),
            Self::RepositoryName => write!(
                formatter,
                "isolated advisory repository name is not a cargo-deny URL-derived cache name; reprovision the isolated RustSec cache"
            ),
            Self::SnapshotContainment => write!(
                formatter,
                "advisory snapshot containment failed; remove every link, junction, reparse point, special file, or collision and reprovision the cache"
            ),
            Self::SnapshotDirty => write!(
                formatter,
                "advisory snapshot is dirty, including tracked, untracked, or ignored content; restore a clean isolated RustSec cache"
            ),
            Self::SourceMismatch => write!(
                formatter,
                "advisory snapshot origin does not equal the approved private mirror locator; reprovision the isolated mirror cache and retry"
            ),
            Self::CommitMalformed => write!(
                formatter,
                "advisory snapshot HEAD is not one full lowercase commit id; reprovision the isolated RustSec cache"
            ),
            Self::ShallowRepository => write!(
                formatter,
                "advisory snapshot is shallow; provision a full isolated RustSec repository and retry"
            ),
            Self::RecordedTreeDigestMalformed => write!(
                formatter,
                "recorded advisory tree_sha256 is not 64 lowercase hexadecimal characters; provide the reviewed snapshot digest"
            ),
            Self::ArchiveFailed => write!(
                formatter,
                "advisory snapshot tree could not be archived from local HEAD; reprovision the isolated RustSec cache"
            ),
            Self::ArchiveDigestMismatch => write!(
                formatter,
                "advisory snapshot archive digest differs from recorded tree_sha256; reprovision the reviewed isolated cache"
            ),
            Self::FetchHeadMissing => write!(
                formatter,
                "advisory snapshot acquisition evidence .git/FETCH_HEAD is missing or unreadable; refresh the isolated RustSec cache and rerun finalization"
            ),
            Self::AcquisitionFuture => write!(
                formatter,
                "advisory snapshot acquisition is in the future; correct the host clock, refresh the isolated RustSec cache, and rerun finalization"
            ),
            Self::SnapshotStale => write!(
                formatter,
                "advisory snapshot stale: FETCH_HEAD acquisition is older than 24h; refresh the isolated RustSec cache and rerun finalization"
            ),
            Self::GitInvocationFailed { step } => write!(
                formatter,
                "local advisory git {step} inspection failed; restore the selected Git executable and isolated cache, then retry"
            ),
            Self::AdvisoryActionInvalid => write!(
                formatter,
                "selected cargo-deny advisory action is not the closed offline template; rerun release-tool preflight and pass its record unchanged"
            ),
            Self::CargoDenyFailed => write!(
                formatter,
                "offline cargo-deny advisories check failed; remediate the reported advisory or policy failure and rerun finalization"
            ),
            Self::ClockUnavailable => write!(
                formatter,
                "canonical UTC release time is unavailable; correct the host clock and retry"
            ),
        }
    }
}

impl std::error::Error for AdvisoryError {}

pub fn render_advisory_config(
    deny_toml: &[u8],
    isolated_database_root: &Path,
    locator: &str,
) -> Result<Vec<u8>, AdvisoryError> {
    if !isolated_database_root.is_absolute() {
        return Err(AdvisoryError::DatabaseRootInvalid);
    }
    let source = std::str::from_utf8(deny_toml).map_err(|_| AdvisoryError::PolicyMalformed)?;
    let document: Value = toml::from_str(source).map_err(|_| AdvisoryError::PolicyMalformed)?;
    let advisories = document
        .get("advisories")
        .and_then(Value::as_table)
        .ok_or(AdvisoryError::PolicyMalformed)?;
    let keys: BTreeSet<&str> = advisories.keys().map(String::as_str).collect();
    if keys != BTreeSet::from(["ignore", "unmaintained", "yanked"])
        || advisories.get("yanked").and_then(Value::as_str) != Some("warn")
        || advisories.get("unmaintained").and_then(Value::as_str) != Some("workspace")
    {
        return Err(AdvisoryError::PolicyMismatch);
    }
    let ignores = advisories
        .get("ignore")
        .and_then(Value::as_array)
        .ok_or(AdvisoryError::PolicyMalformed)?;
    if ignores.len() != EXPECTED_IGNORE_IDS.len() {
        return Err(AdvisoryError::PolicyMismatch);
    }
    let mut parsed_ignores = Vec::with_capacity(ignores.len());
    for (index, ignore) in ignores.iter().enumerate() {
        let table = ignore.as_table().ok_or(AdvisoryError::PolicyMalformed)?;
        let ignore_keys: BTreeSet<&str> = table.keys().map(String::as_str).collect();
        let id = table
            .get("id")
            .and_then(Value::as_str)
            .ok_or(AdvisoryError::PolicyMalformed)?;
        let reason = table
            .get("reason")
            .and_then(Value::as_str)
            .ok_or(AdvisoryError::PolicyMalformed)?;
        if ignore_keys != BTreeSet::from(["id", "reason"])
            || id != EXPECTED_IGNORE_IDS[index]
            || reason.trim().is_empty()
        {
            return Err(AdvisoryError::PolicyMismatch);
        }
        parsed_ignores.push((id, reason));
    }

    let database_root = child_process_path_text(isolated_database_root)
        .ok_or(AdvisoryError::DatabaseRootInvalid)?;
    let mut rendered = String::new();
    rendered.push_str("[advisories]\n");
    rendered.push_str("db-path = ");
    rendered.push_str(&toml_string(&database_root));
    rendered.push('\n');
    rendered.push_str("db-urls = [");
    rendered.push_str(&toml_string(locator));
    rendered.push_str("]\n");
    rendered.push_str("yanked = \"warn\"\n");
    rendered.push_str("unmaintained = \"workspace\"\n");
    rendered.push_str("ignore = [\n");
    for (id, reason) in parsed_ignores {
        rendered.push_str("  { id = ");
        rendered.push_str(&toml_string(id));
        rendered.push_str(", reason = ");
        rendered.push_str(&toml_string(reason));
        rendered.push_str(" },\n");
    }
    rendered.push_str("]\n");
    Ok(rendered.into_bytes())
}

pub fn materialize_advisory_config(
    checkout_root: &Path,
    version: &str,
    locator: &str,
) -> Result<MaterializedAdvisoryConfig, AdvisoryError> {
    let parsed = Version::parse(version).map_err(|_| AdvisoryError::InvalidVersion)?;
    if parsed.to_string() != version {
        return Err(AdvisoryError::InvalidVersion);
    }
    let checkout = ContainedRoot::new(
        checkout_root,
        "release checkout",
        UnixModePolicy::AllowExecute,
    )
    .map_err(|_| AdvisoryError::CheckoutContainment)?;
    let database_root = verified_isolated_database_root(&checkout)?;
    let bytes = render_checkout_advisory_config(&checkout, &database_root, locator)?;

    let transaction_relative = format!("target/release-finalizer/{version}");
    let transaction = verify_contained_path(
        checkout.path(),
        checkout.canonical_path(),
        &transaction_relative,
        "release checkout",
        "release finalizer transaction",
    )
    .map_err(|_| AdvisoryError::ConfigLocationInvalid)?;
    if !transaction.metadata().file_type().is_dir() {
        return Err(AdvisoryError::ConfigLocationInvalid);
    }
    let advisory_dir = checkout
        .canonical_path()
        .join(&transaction_relative)
        .join("advisory");
    fs::create_dir(&advisory_dir).map_err(|_| AdvisoryError::ConfigMaterializationFailed)?;
    let advisory_relative = format!("{transaction_relative}/advisory");
    verify_contained_path(
        checkout.path(),
        checkout.canonical_path(),
        &advisory_relative,
        "release checkout",
        "release advisory config directory",
    )
    .map_err(|_| AdvisoryError::ConfigLocationInvalid)?;
    let path = advisory_dir.join("deny.toml");
    write_new_config(&path, &bytes)?;

    Ok(MaterializedAdvisoryConfig {
        path,
        database_root,
        bytes,
    })
}

/// Materialize the canonical advisory policy at a caller-selected contained
/// output path. This is the CI acceptance seam; release finalization continues
/// to use [`materialize_advisory_config`] and its version-owned location.
pub fn materialize_advisory_config_at(
    checkout_root: &Path,
    isolated_database_root: &Path,
    output_path: &Path,
    locator: &str,
) -> Result<MaterializedAdvisoryConfig, AdvisoryError> {
    if !isolated_database_root.is_absolute() || !output_path.is_absolute() {
        return Err(AdvisoryError::ConfigLocationInvalid);
    }
    let checkout = ContainedRoot::new(
        checkout_root,
        "release checkout",
        UnixModePolicy::AllowExecute,
    )
    .map_err(|_| AdvisoryError::CheckoutContainment)?;
    let database_root = verified_isolated_database_root(&checkout)?;
    if database_root
        != fs::canonicalize(isolated_database_root)
            .map_err(|_| AdvisoryError::DatabaseRootInvalid)?
    {
        return Err(AdvisoryError::DatabaseRootInvalid);
    }

    let output_parent = output_path
        .parent()
        .ok_or(AdvisoryError::ConfigLocationInvalid)?;
    let parent_relative = output_parent
        .strip_prefix(checkout.canonical_path())
        .map_err(|_| AdvisoryError::ConfigLocationInvalid)?
        .to_str()
        .ok_or(AdvisoryError::ConfigLocationInvalid)?;
    let verified_parent = verify_contained_path(
        checkout.path(),
        checkout.canonical_path(),
        parent_relative,
        "release checkout",
        "advisory config output directory",
    )
    .map_err(|_| AdvisoryError::ConfigLocationInvalid)?;
    if !verified_parent.metadata().file_type().is_dir()
        || output_path.parent() != Some(verified_parent.canonical_path())
    {
        return Err(AdvisoryError::ConfigLocationInvalid);
    }

    let bytes = render_checkout_advisory_config(&checkout, &database_root, locator)?;
    write_new_config(output_path, &bytes)?;
    Ok(MaterializedAdvisoryConfig {
        path: output_path.to_path_buf(),
        database_root,
        bytes,
    })
}

fn verified_isolated_database_root(checkout: &ContainedRoot) -> Result<PathBuf, AdvisoryError> {
    let database = verify_contained_path(
        checkout.path(),
        checkout.canonical_path(),
        ADVISORY_DB_RELATIVE,
        "release checkout",
        "isolated advisory database",
    )
    .map_err(|_| AdvisoryError::DatabaseRootInvalid)?;
    if !database.metadata().file_type().is_dir() {
        return Err(AdvisoryError::DatabaseRootInvalid);
    }
    Ok(database.canonical_path().to_path_buf())
}

fn render_checkout_advisory_config(
    checkout: &ContainedRoot,
    database_root: &Path,
    locator: &str,
) -> Result<Vec<u8>, AdvisoryError> {
    let deny_bytes = checkout
        .read("deny.toml", "deny.toml")
        .map_err(|_| AdvisoryError::PolicyMalformed)?;
    render_advisory_config(&deny_bytes, database_root, locator)
}

fn write_new_config(path: &Path, bytes: &[u8]) -> Result<(), AdvisoryError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| AdvisoryError::ConfigMaterializationFailed)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| AdvisoryError::ConfigMaterializationFailed)
}

pub fn validate_mirror_locator(locator: &str) -> Result<(), AdvisoryError> {
    if locator.is_empty() || locator.trim() != locator || locator.chars().any(char::is_control) {
        return Err(AdvisoryError::MirrorLocatorInvalid);
    }
    if is_public_rustsec_github_locator(locator) {
        return Err(AdvisoryError::PublicRustsecSourceForbidden);
    }
    if locator.ends_with('/')
        || locator.contains(['?', '#'])
        || !matches!(
            locator_final_path(locator),
            Some("advisory-db") | Some("rustsec-advisory-db.git")
        )
    {
        return Err(AdvisoryError::MirrorLocatorInvalid);
    }
    Ok(())
}

pub fn validate_freshness_receipt_path(path: &Path) -> Result<(), AdvisoryError> {
    validate_operator_path(path).ok_or(AdvisoryError::FreshnessReceiptPathInvalid)
}

pub fn validate_mirror_public_key_path(path: &Path) -> Result<(), AdvisoryError> {
    validate_operator_path(path).ok_or(AdvisoryError::MirrorPublicKeyPathInvalid)
}

pub fn freshness_signature_path(receipt_path: &Path) -> PathBuf {
    let mut signature = OsString::from(receipt_path.as_os_str());
    signature.push(".minisig");
    PathBuf::from(signature)
}

pub fn format_advisory_mirror_trusted_comment(
    synced_commit: &str,
    utc: &str,
    max_age: u64,
) -> String {
    format!("solpbc-advisory-mirror-v1 synced_commit={synced_commit} utc={utc} max_age={max_age}")
}

pub fn canonical_freshness_body(synced_commit: &str, utc: &str, max_age: u64) -> Vec<u8> {
    format!("{{\"max_age\":{max_age},\"synced_commit\":\"{synced_commit}\",\"utc\":\"{utc}\"}}\n")
        .into_bytes()
}

fn validate_operator_path(path: &Path) -> Option<()> {
    if !path.is_absolute() || path.as_os_str().is_empty() || child_process_path_text(path).is_none()
    {
        return None;
    }
    Some(())
}

fn is_public_rustsec_github_locator(locator: &str) -> bool {
    let locator = locator.split(['?', '#']).next().unwrap_or(locator);
    let lower = locator.to_ascii_lowercase();
    let normalized = lower.trim_end_matches('/');
    let normalized = normalized.strip_suffix(".git").unwrap_or(normalized);
    let normalized = normalized.trim_end_matches('/');

    if let Some((scheme, rest)) = normalized.split_once("://") {
        if !matches!(scheme, "http" | "https" | "git" | "ssh") {
            return false;
        }
        let Some((authority, path)) = rest.split_once('/') else {
            return false;
        };
        let host = authority.rsplit('@').next().unwrap_or(authority);
        let host = host.split(':').next().unwrap_or(host);
        return host == "github.com" && path.trim_matches('/') == "rustsec/advisory-db";
    }

    let Some((authority, path)) = normalized.split_once(':') else {
        return false;
    };
    let host = authority.rsplit('@').next().unwrap_or(authority);
    host == "github.com" && path.trim_matches('/') == "rustsec/advisory-db"
}

fn locator_final_path(locator: &str) -> Option<&str> {
    let without_suffix = locator.split(['?', '#']).next()?;
    let path = if let Some((_, rest)) = without_suffix.split_once("://") {
        rest.split_once('/')?.1
    } else if let Some((_, path)) = without_suffix.split_once(':') {
        path
    } else {
        without_suffix
    };
    path.rsplit('/').next()
}

fn verify_mirror_freshness<R: CommandRunner + ?Sized, C: Clock + ?Sized>(
    checkout_root: &Path,
    version: &str,
    mirror: &MirrorPacketInputs<'_>,
    runner: &R,
    clock: &C,
) -> Result<String, AdvisoryError> {
    let parsed = Version::parse(version).map_err(|_| AdvisoryError::InvalidVersion)?;
    if parsed.to_string() != version {
        return Err(AdvisoryError::InvalidVersion);
    }
    let signature_path = freshness_signature_path(mirror.receipt_path);
    let body = read_packet_file(
        checkout_root,
        mirror.receipt_path,
        "advisory mirror freshness receipt",
        AdvisoryError::FreshnessReceiptMissing,
    )?;
    let signature = read_packet_file(
        checkout_root,
        &signature_path,
        "advisory mirror freshness signature",
        AdvisoryError::FreshnessSignatureMissing,
    )?;
    let public_key = read_packet_file(
        checkout_root,
        mirror.public_key_path,
        "advisory mirror public key",
        AdvisoryError::MirrorPublicKeyMissing,
    )?;
    if !is_lower_hex(mirror.expected_public_key_sha256, 64)
        || sha256_hex(&public_key) != mirror.expected_public_key_sha256
    {
        return Err(AdvisoryError::MirrorPublicKeyPinMismatch);
    }

    verify_freshness_signature(
        checkout_root,
        version,
        mirror.minisign_program,
        &public_key,
        &body,
        &signature,
        runner,
    )?;
    let trusted_comment = trusted_comment(&signature)?;
    let fields = parse_freshness_trusted_comment(trusted_comment)?;
    if body != canonical_freshness_body(&fields.synced_commit, fields.utc.as_str(), fields.max_age)
    {
        return Err(AdvisoryError::FreshnessBodyMismatch);
    }
    validate_freshness_time(&fields, clock)?;
    Ok(fields.synced_commit)
}

fn read_packet_file(
    checkout_root: &Path,
    path: &Path,
    label: &str,
    error: AdvisoryError,
) -> Result<Vec<u8>, AdvisoryError> {
    artifact_fs::verify_regular_file(path, label, UnixModePolicy::StrictNoExecute)
        .map_err(|_| error.clone())?;
    let canonical = fs::canonicalize(path).map_err(|_| error.clone())?;
    let database_root = fs::canonicalize(checkout_root.join(ADVISORY_DB_RELATIVE))
        .map_err(|_| AdvisoryError::DatabaseRootInvalid)?;
    if canonical.starts_with(database_root) {
        return Err(error);
    }
    fs::read(canonical).map_err(|_| error)
}

fn verify_freshness_signature<R: CommandRunner + ?Sized>(
    checkout_root: &Path,
    version: &str,
    minisign_program: &Path,
    public_key: &[u8],
    body: &[u8],
    signature: &[u8],
    runner: &R,
) -> Result<(), AdvisoryError> {
    let checkout = ContainedRoot::new(
        checkout_root,
        "release checkout",
        UnixModePolicy::AllowExecute,
    )
    .map_err(|_| AdvisoryError::CheckoutContainment)?;
    let transaction_relative = format!("target/release-finalizer/{version}");
    let transaction = verify_contained_path(
        checkout.path(),
        checkout.canonical_path(),
        &transaction_relative,
        "release checkout",
        "release finalizer transaction",
    )
    .map_err(|_| AdvisoryError::FreshnessVerificationScratch)?;
    if !transaction.metadata().file_type().is_dir() {
        return Err(AdvisoryError::FreshnessVerificationScratch);
    }
    let scratch = transaction.canonical_path().join(".advisory-mirror-verify");
    fs::create_dir(&scratch).map_err(|_| AdvisoryError::FreshnessVerificationScratch)?;
    let result = (|| {
        let body_path = scratch.join("freshness.json");
        let signature_path = scratch.join("freshness.json.minisig");
        let public_key_path = scratch.join("mirror.pub");
        write_scratch_file(&body_path, body)?;
        write_scratch_file(&signature_path, signature)?;
        write_scratch_file(&public_key_path, public_key)?;
        let output = runner
            .run(
                minisign_program,
                &[
                    "-V".to_owned(),
                    "-p".to_owned(),
                    child_process_path_text(&public_key_path)
                        .ok_or(AdvisoryError::FreshnessVerificationScratch)?,
                    "-m".to_owned(),
                    child_process_path_text(&body_path)
                        .ok_or(AdvisoryError::FreshnessVerificationScratch)?,
                    "-x".to_owned(),
                    child_process_path_text(&signature_path)
                        .ok_or(AdvisoryError::FreshnessVerificationScratch)?,
                ],
                None,
                None,
            )
            .map_err(|_| AdvisoryError::MinisignInvocationFailed)?;
        if output.status == 0 {
            Ok(())
        } else {
            Err(AdvisoryError::FreshnessSignatureInvalid)
        }
    })();
    let cleanup = fs::remove_dir_all(&scratch);
    match (result, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(_)) => Err(AdvisoryError::FreshnessVerificationScratch),
    }
}

fn write_scratch_file(path: &Path, bytes: &[u8]) -> Result<(), AdvisoryError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| AdvisoryError::FreshnessVerificationScratch)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| AdvisoryError::FreshnessVerificationScratch)
}

fn trusted_comment(signature: &[u8]) -> Result<&str, AdvisoryError> {
    let text = std::str::from_utf8(signature)
        .map_err(|_| AdvisoryError::FreshnessTrustedCommentMissing)?;
    text.lines()
        .find_map(|line| line.strip_prefix("trusted comment: "))
        .ok_or(AdvisoryError::FreshnessTrustedCommentMissing)
}

fn parse_freshness_trusted_comment(
    trusted_comment: &str,
) -> Result<FreshnessFields, AdvisoryError> {
    let fields: Vec<&str> = trusted_comment.split(' ').collect();
    if fields.len() != 4 || fields.iter().any(|field| field.is_empty()) {
        return Err(AdvisoryError::FreshnessTrustedCommentFields);
    }
    if fields[0] != "solpbc-advisory-mirror-v1" {
        return Err(AdvisoryError::FreshnessTrustedCommentPrefix);
    }
    let synced_commit = required_comment_value(fields[1], "synced_commit=")?;
    let utc = required_comment_value(fields[2], "utc=")?;
    let max_age = required_comment_value(fields[3], "max_age=")?;
    if !is_lower_hex(synced_commit, 40) {
        return Err(AdvisoryError::FreshnessSyncedCommitMalformed);
    }
    let utc = UtcTimestamp::parse(utc).map_err(|_| AdvisoryError::FreshnessUtcMalformed)?;
    if max_age != "86400" {
        return Err(AdvisoryError::FreshnessMaxAgeInvalid);
    }
    let parsed = FreshnessFields {
        synced_commit: synced_commit.to_owned(),
        utc,
        max_age: REQUIRED_RECEIPT_MAX_AGE,
    };
    if trusted_comment
        != format_advisory_mirror_trusted_comment(
            &parsed.synced_commit,
            parsed.utc.as_str(),
            parsed.max_age,
        )
    {
        return Err(AdvisoryError::FreshnessTrustedCommentFields);
    }
    Ok(parsed)
}

fn required_comment_value<'a>(field: &'a str, prefix: &str) -> Result<&'a str, AdvisoryError> {
    field
        .strip_prefix(prefix)
        .filter(|value| !value.is_empty())
        .ok_or(AdvisoryError::FreshnessTrustedCommentFields)
}

fn validate_freshness_time<C: Clock + ?Sized>(
    fields: &FreshnessFields,
    clock: &C,
) -> Result<(), AdvisoryError> {
    let now = clock.now().map_err(|_| AdvisoryError::ClockUnavailable)?;
    let latest_accepted = now
        .system_time()
        .checked_add(MAX_RECEIPT_FUTURE)
        .ok_or(AdvisoryError::ClockUnavailable)?;
    if fields.utc.system_time() > latest_accepted {
        return Err(AdvisoryError::FreshnessUtcFuture);
    }
    let expires = fields
        .utc
        .system_time()
        .checked_add(Duration::from_secs(fields.max_age))
        .ok_or(AdvisoryError::ClockUnavailable)?;
    if now.system_time() > expires {
        return Err(AdvisoryError::FreshnessStale);
    }
    Ok(())
}

impl AdvisorySnapshot {
    pub fn inspect<R: CommandRunner + ?Sized, C: Clock + ?Sized>(
        checkout_root: &Path,
        git_program: &Path,
        recorded_tree_sha256: &str,
        locator: &str,
        runner: &R,
        clock: &C,
    ) -> Result<Self, AdvisoryError> {
        if !is_lower_hex(recorded_tree_sha256, 64) {
            return Err(AdvisoryError::RecordedTreeDigestMalformed);
        }
        let checkout = ContainedRoot::new(
            checkout_root,
            "release checkout",
            UnixModePolicy::AllowExecute,
        )
        .map_err(|_| AdvisoryError::CheckoutContainment)?;
        let database_root = checkout.path().join(ADVISORY_DB_RELATIVE);
        let verified_database = verify_contained_path(
            checkout.path(),
            checkout.canonical_path(),
            ADVISORY_DB_RELATIVE,
            "release checkout",
            "isolated advisory database",
        )
        .map_err(|_| AdvisoryError::DatabaseRootInvalid)?;
        if !verified_database.metadata().file_type().is_dir() {
            return Err(AdvisoryError::DatabaseRootInvalid);
        }
        artifact_fs::walk_directory(
            &database_root,
            "isolated advisory database",
            UnixModePolicy::AllowExecute,
        )
        .map_err(|_| AdvisoryError::SnapshotContainment)?;

        let mut children = Vec::new();
        for entry in fs::read_dir(&database_root).map_err(|_| AdvisoryError::DatabaseRootInvalid)? {
            let entry = entry.map_err(|_| AdvisoryError::DatabaseRootInvalid)?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| AdvisoryError::RepositoryName)?;
            if name == "db.lock"
                && fs::symlink_metadata(entry.path())
                    .map_err(|_| AdvisoryError::DatabaseRootInvalid)?
                    .file_type()
                    .is_file()
            {
                continue;
            }
            children.push(name);
        }
        children.sort();
        if children.len() != 1 {
            return Err(AdvisoryError::RepositoryCount);
        }
        let repository_name = &children[0];
        if !is_url_derived_repository_name(repository_name) {
            return Err(AdvisoryError::RepositoryName);
        }
        let repository_relative = format!("{ADVISORY_DB_RELATIVE}/{repository_name}");
        let verified_repository = verify_contained_path(
            checkout.path(),
            checkout.canonical_path(),
            &repository_relative,
            "release checkout",
            "isolated RustSec repository",
        )
        .map_err(|_| AdvisoryError::SnapshotContainment)?;
        if !verified_repository.metadata().file_type().is_dir() {
            return Err(AdvisoryError::RepositoryCount);
        }
        let repository_path = database_root.join(repository_name);
        let repository = ContainedRoot::new(
            &repository_path,
            "isolated RustSec repository",
            UnixModePolicy::AllowExecute,
        )
        .map_err(|_| AdvisoryError::SnapshotContainment)?;
        let git_dir = verify_contained_path(
            repository.path(),
            repository.canonical_path(),
            ".git",
            "isolated RustSec repository",
            ".git",
        )
        .map_err(|_| AdvisoryError::SnapshotContainment)?;
        if !git_dir.metadata().file_type().is_dir() {
            return Err(AdvisoryError::SnapshotContainment);
        }
        repository
            .resolve(".git/HEAD", ".git/HEAD")
            .map_err(|_| AdvisoryError::SnapshotContainment)?;
        let fetch_head = verify_contained_path(
            repository.path(),
            repository.canonical_path(),
            ".git/FETCH_HEAD",
            "isolated RustSec repository",
            ".git/FETCH_HEAD",
        )
        .map_err(|_| AdvisoryError::FetchHeadMissing)?;
        if !fetch_head.metadata().file_type().is_file() {
            return Err(AdvisoryError::FetchHeadMissing);
        }
        let acquired = fetch_head
            .metadata()
            .modified()
            .map_err(|_| AdvisoryError::FetchHeadMissing)?;

        let repository_arg = child_process_path_text(repository.canonical_path())
            .ok_or(AdvisoryError::SnapshotContainment)?;
        let status = run_git(
            runner,
            git_program,
            &repository_arg,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignored",
            ],
            "status",
        )?;
        if status.status != 0 || !status.stdout.is_empty() {
            return Err(AdvisoryError::SnapshotDirty);
        }
        let source = run_git(
            runner,
            git_program,
            &repository_arg,
            &["remote", "get-url", "origin"],
            "source",
        )?;
        if source.status != 0 || parse_git_line(&source.stdout).as_deref() != Some(locator) {
            return Err(AdvisoryError::SourceMismatch);
        }
        let commit_output = run_git(
            runner,
            git_program,
            &repository_arg,
            &["rev-parse", "HEAD^{commit}"],
            "commit",
        )?;
        let commit = if commit_output.status == 0 {
            parse_git_line(&commit_output.stdout).filter(|value| is_lower_hex(value, 40))
        } else {
            None
        }
        .ok_or(AdvisoryError::CommitMalformed)?;
        let shallow = run_git(
            runner,
            git_program,
            &repository_arg,
            &["rev-parse", "--is-shallow-repository"],
            "shallow",
        )?;
        if shallow.status != 0 || parse_git_line(&shallow.stdout).as_deref() != Some("false") {
            return Err(AdvisoryError::ShallowRepository);
        }
        let archive = run_git(
            runner,
            git_program,
            &repository_arg,
            &["archive", "--format=tar", "HEAD"],
            "archive",
        )?;
        if archive.status != 0 {
            return Err(AdvisoryError::ArchiveFailed);
        }
        let tree_sha256 = sha256_hex(&archive.stdout);
        if tree_sha256 != recorded_tree_sha256 {
            return Err(AdvisoryError::ArchiveDigestMismatch);
        }

        let check_started_at = clock.now().map_err(|_| AdvisoryError::ClockUnavailable)?;
        if acquired > check_started_at.system_time() {
            return Err(AdvisoryError::AcquisitionFuture);
        }
        let age = check_started_at
            .system_time()
            .duration_since(acquired)
            .map_err(|_| AdvisoryError::AcquisitionFuture)?;
        if age > MAX_SNAPSHOT_AGE {
            return Err(AdvisoryError::SnapshotStale);
        }
        let acquired_at = UtcTimestamp::from_system_time(acquired)
            .map_err(|_| AdvisoryError::FetchHeadMissing)?
            .as_str()
            .to_owned();

        Ok(Self {
            source_id: MIRROR_COHORT_ID.to_owned(),
            commit,
            tree_sha256,
            acquired_at,
        })
    }
}

#[allow(clippy::too_many_arguments)]
pub fn run_advisory_check<R: CommandRunner + ?Sized, C: Clock + ?Sized>(
    checkout_root: &Path,
    version: &str,
    git_program: &Path,
    recorded_tree_sha256: &str,
    advisory_action: &SelectedAction,
    mirror: &MirrorPacketInputs<'_>,
    runner: &R,
    clock: &C,
) -> Result<AdvisoryProvenance, AdvisoryError> {
    let synced_commit = verify_mirror_freshness(checkout_root, version, mirror, runner, clock)?;
    let config = materialize_advisory_config(checkout_root, version, mirror.locator)?;
    let snapshot = AdvisorySnapshot::inspect(
        checkout_root,
        git_program,
        recorded_tree_sha256,
        mirror.locator,
        runner,
        clock,
    )?;
    if snapshot.commit != synced_commit {
        return Err(AdvisoryError::FreshnessCommitMismatch);
    }
    let expected_argv = [
        "deny",
        "--locked",
        "--offline",
        "--config",
        "{advisory_config}",
        "check",
        "advisories",
    ];
    if advisory_action
        .argv
        .iter()
        .map(String::as_str)
        .ne(expected_argv)
    {
        return Err(AdvisoryError::AdvisoryActionInvalid);
    }
    let config_path =
        child_process_path_text(&config.path).ok_or(AdvisoryError::ConfigLocationInvalid)?;
    let placeholder_count = advisory_action
        .argv
        .iter()
        .filter(|arg| arg.as_str() == "{advisory_config}")
        .count();
    if placeholder_count != 1 {
        return Err(AdvisoryError::AdvisoryActionInvalid);
    }
    let args: Vec<String> = advisory_action
        .argv
        .iter()
        .map(|arg| {
            if arg == "{advisory_config}" {
                config_path.clone()
            } else {
                arg.clone()
            }
        })
        .collect();
    let output = runner
        .run(&advisory_action.program, &args, None, None)
        .map_err(|_| AdvisoryError::CargoDenyFailed)?;
    if output.status != 0 {
        return Err(AdvisoryError::CargoDenyFailed);
    }
    let checked_at = clock
        .now()
        .map_err(|_| AdvisoryError::ClockUnavailable)?
        .as_str()
        .to_owned();
    Ok(AdvisoryProvenance {
        source_id: snapshot.source_id,
        commit: snapshot.commit,
        tree_sha256: snapshot.tree_sha256,
        acquired_at: snapshot.acquired_at,
        checked_at,
    })
}

fn run_git<R: CommandRunner + ?Sized>(
    runner: &R,
    git_program: &Path,
    repository: &str,
    args: &[&str],
    step: &'static str,
) -> Result<CommandOutput, AdvisoryError> {
    let mut full_args = vec!["-C".to_owned(), repository.to_owned()];
    full_args.extend(args.iter().map(|arg| (*arg).to_owned()));
    runner
        .run(git_program, &full_args, None, None)
        .map_err(|_| AdvisoryError::GitInvocationFailed { step })
}

fn parse_git_line(bytes: &[u8]) -> Option<String> {
    let mut line = bytes;
    if let Some(stripped) = line.strip_suffix(b"\n") {
        line = stripped;
    }
    if let Some(stripped) = line.strip_suffix(b"\r") {
        line = stripped;
    }
    if line.is_empty() || line.iter().any(|byte| byte.is_ascii_control()) {
        return None;
    }
    String::from_utf8(line.to_vec()).ok()
}

/// Accepts cargo-deny's URL-derived cache basename shape:
/// `[a-z0-9][a-z0-9._-]*-[0-9a-f]{16}`. This is structural only;
/// the basename never establishes source authority.
fn is_url_derived_repository_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.len() < 17 {
        return false;
    }

    let prefix_length = bytes.len() - 17;
    let (prefix, suffix) = bytes.split_at(prefix_length);
    let Some((&first, prefix_rest)) = prefix.split_first() else {
        return false;
    };

    let valid_prefix_byte = |byte: u8| {
        byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || byte == b'.'
            || byte == b'_'
            || byte == b'-'
    };

    (first.is_ascii_lowercase() || first.is_ascii_digit())
        && prefix_rest.iter().copied().all(valid_prefix_byte)
        && suffix[0] == b'-'
        && suffix[1..]
            .iter()
            .copied()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn toml_string(value: &str) -> String {
    let mut escaped = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\u{0008}' => escaped.push_str("\\b"),
            '\t' => escaped.push_str("\\t"),
            '\n' => escaped.push_str("\\n"),
            '\u{000C}' => escaped.push_str("\\f"),
            '\r' => escaped.push_str("\\r"),
            character if character.is_control() => {
                escaped.push_str(&format!("\\u{:04X}", u32::from(character)));
            }
            character => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
}

#[cfg(test)]
mod tests {
    use super::is_url_derived_repository_name;

    #[test]
    fn url_derived_repository_name_grammar_is_byte_exact() {
        for name in [
            "rustsec-advisory-db.git-b0b0b0b0b0b0b0b0",
            "advisory-db-a5a5a5a5a5a5a5a5",
            "advisory-db-aaaaaaaaaaaaaaaa",
        ] {
            assert!(
                is_url_derived_repository_name(name),
                "expected accepted repository name: {name:?}"
            );
        }

        for name in [
            "Advisory-db-aaaaaaaaaaaaaaaa",
            "advisory-db-Aaaaaaaaaaaaaaaa",
            "advisory-db-aaaaaaaaaaaaaaa",
            "advisory-db-aaaaaaaaaaaaaaaaa",
            "advisory-db-aaaaaaaaaaaaaaag",
            "-aaaaaaaaaaaaaaaa",
            ".cache-aaaaaaaaaaaaaaaa",
            "cache-aaaaaaaaaaaaaaaa.",
            ".-aaaaaaaaaaaaaaaa",
            "a/b-aaaaaaaaaaaaaaaa",
            "a\\b-aaaaaaaaaaaaaaaa",
            "a:b-aaaaaaaaaaaaaaaa",
            " cache-aaaaaaaaaaaaaaaa",
            "cache-aaaaaaaaaaaaaaaa ",
            "é-aaaaaaaaaaaaaaaa",
            "a\0b-aaaaaaaaaaaaaaaa",
            "a\nb-aaaaaaaaaaaaaaaa",
        ] {
            assert!(
                !is_url_derived_repository_name(name),
                "expected rejected repository name: {name:?}"
            );
        }
    }
}
