// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Canonical, privacy-clean receipts for release finalization and native proof.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::artifact_fs::{verify_contained_path, ContainedRoot, UnixModePolicy};
use crate::release_advisory::MIRROR_COHORT_ID;
use crate::release_clock::UtcTimestamp;
use crate::release_finalizer_fs::create_contained_directory;
use crate::rust_release_manifest::{
    companion_basename, render_canonical_json, PRODUCT, TARGET_TRIPLE,
};

pub const FINALIZATION_RECEIPT_SCHEMA: &str = "solstone.rust-release-finalization.v1";
pub const FINALIZATION_RECEIPT_SCHEMA_V2: &str = "solstone.rust-release-finalization.v2";
pub const WINDOWS_NATIVE_PROOF_SCHEMA: &str = "solstone.windows-native-proof.v1";
pub const FINALIZATION_RECEIPT_FILENAME: &str = "rust-release-finalization.json";
pub const WINDOWS_NATIVE_PROOF_FILENAME: &str = "windows-native-proof.json";

pub const EVIDENCE_ROOT: &str = "target/release-evidence";
pub const CANDIDATE_ROOT: &str = "target/release-candidate";
pub(crate) const FINALIZATION_RECEIPT_TEMP: &str = ".rust-release-finalization.json.tmp";
const WINDOWS_NATIVE_PROOF_TEMP: &str = ".windows-native-proof.json.tmp";
const HISTORICAL_ADVISORY_SOURCE_ID_V1: &str = "https://github.com/RustSec/advisory-db";

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CompanionManifestReceipt {
    pub filename: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CandidateReceipt {
    pub relative_path: String,
    pub file_count: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AdvisoryDatabaseReceipt {
    pub source_id: String,
    pub commit: String,
    pub tree_sha256: String,
    pub acquired_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FinalizationReceipt {
    pub schema: String,
    pub product: String,
    pub version: String,
    pub target: String,
    pub source_commit: String,
    pub cargo_lock_sha256: String,
    pub ui_package_lock_sha256: String,
    pub companion_manifest: CompanionManifestReceipt,
    pub candidate: CandidateReceipt,
    pub selection_record_sha256: String,
    pub signing_mode: String,
    pub advisory_database: AdvisoryDatabaseReceipt,
    pub advisory_checked_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WindowsNativeProofReceipt {
    pub schema: String,
    pub product: String,
    pub version: String,
    pub target: String,
    pub source_commit: String,
    pub companion_manifest: CompanionManifestReceipt,
    pub setup_sha256: String,
    pub packaged_executable_sha256: String,
    pub installed_executable_sha256: String,
    pub install_mode: String,
    pub installer_success: bool,
    pub smoke_success: bool,
    pub proved_at: String,
}

#[derive(Debug)]
pub struct StagedReceipt {
    checkout_root: PathBuf,
    canonical_checkout: PathBuf,
    evidence_relative: String,
    temp_filename: &'static str,
    final_filename: &'static str,
    expected_sha256: [u8; 32],
    expected_len: u64,
    existing_final: Option<ReceiptIdentity>,
    replace_final: bool,
    remove_temp_on_drop: bool,
}

#[derive(Clone, Debug)]
struct ReceiptIdentity {
    sha256: [u8; 32],
    len: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReceiptError {
    InvalidField { field: &'static str },
    SerializationFailed,
    CheckoutContainment,
    EvidenceDirectoryInvalid,
    FinalTargetExists,
    FinalTargetInvalid,
    FinalTargetChanged,
    StagedTargetExists,
    StageWriteFailed,
    StagedBytesChanged,
    PromotionFailed,
}

impl fmt::Display for ReceiptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidField { field } => write!(
                formatter,
                "release receipt field `{field}` is not canonical or does not satisfy the receipt contract; rebuild the evidence from verified transaction inputs"
            ),
            Self::SerializationFailed => write!(
                formatter,
                "release receipt canonical serialization failed; restore the typed receipt values and retry"
            ),
            Self::CheckoutContainment => write!(
                formatter,
                "release receipt checkout containment failed; use one real checkout without links or reparse points"
            ),
            Self::EvidenceDirectoryInvalid => write!(
                formatter,
                "release evidence directory is not a real contained version directory; remove links or reparse points beneath target and retry"
            ),
            Self::FinalTargetExists => write!(
                formatter,
                "the final release receipt already exists; use a new version or complete the approved same-version cleanup before retrying"
            ),
            Self::FinalTargetInvalid => write!(
                formatter,
                "the prior finalization receipt is malformed or non-canonical; restore the valid same-version receipt before re-finalizing"
            ),
            Self::FinalTargetChanged => write!(
                formatter,
                "the existing finalization receipt changed during same-version replacement; restore the prior evidence bytes and restart finalization"
            ),
            Self::StagedTargetExists => write!(
                formatter,
                "a staged release receipt already exists; run the confined cleanup for this version and retry"
            ),
            Self::StageWriteFailed => write!(
                formatter,
                "the staged release receipt could not be written and synced; restore target permissions and retry from a new transaction"
            ),
            Self::StagedBytesChanged => write!(
                formatter,
                "the staged release receipt changed after writing; remove the staged file through confined cleanup and retry"
            ),
            Self::PromotionFailed => write!(
                formatter,
                "the staged release receipt could not be atomically renamed; keep staging and final paths on the same filesystem and retry"
            ),
        }
    }
}

impl std::error::Error for ReceiptError {}

pub fn render_finalization_receipt(receipt: &FinalizationReceipt) -> Result<Vec<u8>, ReceiptError> {
    validate_finalization_receipt(receipt)?;
    render_canonical_json(receipt).map_err(|_| ReceiptError::SerializationFailed)
}

pub fn render_windows_native_proof_receipt(
    receipt: &WindowsNativeProofReceipt,
) -> Result<Vec<u8>, ReceiptError> {
    validate_windows_native_proof_receipt(receipt)?;
    render_canonical_json(receipt).map_err(|_| ReceiptError::SerializationFailed)
}

pub fn stage_finalization_receipt(
    checkout_root: &Path,
    receipt: &FinalizationReceipt,
) -> Result<StagedReceipt, ReceiptError> {
    let bytes = render_finalization_receipt(receipt)?;
    stage_receipt(
        checkout_root,
        &receipt.version,
        FINALIZATION_RECEIPT_TEMP,
        FINALIZATION_RECEIPT_FILENAME,
        &bytes,
        true,
    )
}

pub fn stage_windows_native_proof_receipt(
    checkout_root: &Path,
    receipt: &WindowsNativeProofReceipt,
) -> Result<StagedReceipt, ReceiptError> {
    let bytes = render_windows_native_proof_receipt(receipt)?;
    stage_receipt(
        checkout_root,
        &receipt.version,
        WINDOWS_NATIVE_PROOF_TEMP,
        WINDOWS_NATIVE_PROOF_FILENAME,
        &bytes,
        false,
    )
}

impl StagedReceipt {
    pub fn staged_relative_path(&self) -> String {
        format!("{}/{}", self.evidence_relative, self.temp_filename)
    }

    pub fn final_relative_path(&self) -> String {
        format!("{}/{}", self.evidence_relative, self.final_filename)
    }

    pub fn promote(mut self) -> Result<String, ReceiptError> {
        let checkout = ContainedRoot::new(
            &self.checkout_root,
            "release checkout",
            UnixModePolicy::AllowExecute,
        )
        .map_err(|_| ReceiptError::CheckoutContainment)?;
        if checkout.canonical_path() != self.canonical_checkout {
            return Err(ReceiptError::CheckoutContainment);
        }
        let evidence = contained_evidence_root(&checkout, &self.evidence_relative)?;
        let observed = evidence
            .read(self.temp_filename, "staged release receipt")
            .map_err(|_| ReceiptError::StagedBytesChanged)?;
        if u64::try_from(observed.len()).ok() != Some(self.expected_len)
            || Sha256::digest(&observed).as_slice() != self.expected_sha256
        {
            return Err(ReceiptError::StagedBytesChanged);
        }
        let final_path = evidence.canonical_path().join(self.final_filename);
        match &self.existing_final {
            Some(expected) => {
                let final_bytes = evidence
                    .read(self.final_filename, "existing release receipt")
                    .map_err(|_| ReceiptError::FinalTargetChanged)?;
                if u64::try_from(final_bytes.len()).ok() != Some(expected.len)
                    || Sha256::digest(&final_bytes).as_slice() != expected.sha256
                {
                    return Err(ReceiptError::FinalTargetChanged);
                }
            }
            None => require_absent(&final_path, ReceiptError::FinalTargetExists)?,
        }
        let temp_path =
            tempfile::TempPath::try_from_path(evidence.canonical_path().join(self.temp_filename))
                .map_err(|_| ReceiptError::PromotionFailed)?;
        if self.replace_final {
            temp_path
                .persist(&final_path)
                .map_err(|_| ReceiptError::PromotionFailed)?;
        } else {
            temp_path
                .persist_noclobber(&final_path)
                .map_err(|_| ReceiptError::PromotionFailed)?;
        }
        self.remove_temp_on_drop = false;
        Ok(self.final_relative_path())
    }
}

impl Drop for StagedReceipt {
    fn drop(&mut self) {
        if self.remove_temp_on_drop {
            let _ = fs::remove_file(
                self.canonical_checkout
                    .join(&self.evidence_relative)
                    .join(self.temp_filename),
            );
        }
    }
}

fn validate_finalization_receipt(receipt: &FinalizationReceipt) -> Result<(), ReceiptError> {
    let expected_source_id = match receipt.schema.as_str() {
        FINALIZATION_RECEIPT_SCHEMA => HISTORICAL_ADVISORY_SOURCE_ID_V1,
        FINALIZATION_RECEIPT_SCHEMA_V2 => MIRROR_COHORT_ID,
        _ => return invalid("schema"),
    };
    validate_common(
        &receipt.schema,
        &receipt.schema,
        &receipt.product,
        &receipt.version,
        &receipt.target,
        &receipt.source_commit,
    )?;
    validate_sha256(&receipt.cargo_lock_sha256, "cargo_lock_sha256")?;
    validate_sha256(&receipt.ui_package_lock_sha256, "ui_package_lock_sha256")?;
    validate_companion(&receipt.companion_manifest)?;
    if receipt.candidate.relative_path != candidate_relative_path(&receipt.version)? {
        return invalid("candidate.relative_path");
    }
    if !matches!(receipt.candidate.file_count, 7 | 8) {
        return invalid("candidate.file_count");
    }
    validate_sha256(&receipt.selection_record_sha256, "selection_record_sha256")?;
    if !matches!(
        receipt.signing_mode.as_str(),
        "unsigned" | "signed-verified"
    ) {
        return invalid("signing_mode");
    }
    if receipt.advisory_database.source_id != expected_source_id {
        return invalid("advisory_database.source_id");
    }
    validate_commit(
        &receipt.advisory_database.commit,
        "advisory_database.commit",
    )?;
    validate_sha256(
        &receipt.advisory_database.tree_sha256,
        "advisory_database.tree_sha256",
    )?;
    validate_timestamp(
        &receipt.advisory_database.acquired_at,
        "advisory_database.acquired_at",
    )?;
    validate_timestamp(&receipt.advisory_checked_at, "advisory_checked_at")
}

fn validate_windows_native_proof_receipt(
    receipt: &WindowsNativeProofReceipt,
) -> Result<(), ReceiptError> {
    validate_common(
        &receipt.schema,
        WINDOWS_NATIVE_PROOF_SCHEMA,
        &receipt.product,
        &receipt.version,
        &receipt.target,
        &receipt.source_commit,
    )?;
    validate_companion(&receipt.companion_manifest)?;
    validate_sha256(&receipt.setup_sha256, "setup_sha256")?;
    validate_sha256(
        &receipt.packaged_executable_sha256,
        "packaged_executable_sha256",
    )?;
    validate_sha256(
        &receipt.installed_executable_sha256,
        "installed_executable_sha256",
    )?;
    if receipt.packaged_executable_sha256 != receipt.installed_executable_sha256 {
        return invalid("installed_executable_sha256");
    }
    if receipt.install_mode != "isolated-clean" {
        return invalid("install_mode");
    }
    if !receipt.installer_success {
        return invalid("installer_success");
    }
    if !receipt.smoke_success {
        return invalid("smoke_success");
    }
    validate_timestamp(&receipt.proved_at, "proved_at")
}

fn validate_common(
    schema: &str,
    expected_schema: &str,
    product: &str,
    version: &str,
    target: &str,
    source_commit: &str,
) -> Result<(), ReceiptError> {
    if schema != expected_schema {
        return invalid("schema");
    }
    if product != PRODUCT {
        return invalid("product");
    }
    canonical_version(version)?;
    if target != TARGET_TRIPLE {
        return invalid("target");
    }
    validate_commit(source_commit, "source_commit")
}

fn validate_companion(companion: &CompanionManifestReceipt) -> Result<(), ReceiptError> {
    if companion.filename != companion_basename() {
        return invalid("companion_manifest.filename");
    }
    validate_sha256(&companion.sha256, "companion_manifest.sha256")
}

fn canonical_version(version: &str) -> Result<Version, ReceiptError> {
    let parsed =
        Version::parse(version).map_err(|_| ReceiptError::InvalidField { field: "version" })?;
    if parsed.to_string() != version {
        return invalid("version");
    }
    Ok(parsed)
}

pub fn candidate_relative_path(version: &str) -> Result<String, ReceiptError> {
    canonical_version(version)?;
    Ok(format!("{CANDIDATE_ROOT}/{version}"))
}

pub fn evidence_relative_path(version: &str) -> Result<String, ReceiptError> {
    canonical_version(version)?;
    Ok(format!("{EVIDENCE_ROOT}/{version}"))
}

pub fn finalization_receipt_relative_path(version: &str) -> Result<String, ReceiptError> {
    Ok(format!(
        "{}/{}",
        evidence_relative_path(version)?,
        FINALIZATION_RECEIPT_FILENAME
    ))
}

pub fn windows_native_proof_relative_path(version: &str) -> Result<String, ReceiptError> {
    Ok(format!(
        "{}/{}",
        evidence_relative_path(version)?,
        WINDOWS_NATIVE_PROOF_FILENAME
    ))
}

fn validate_commit(value: &str, field: &'static str) -> Result<(), ReceiptError> {
    if is_lower_hex(value, 40) {
        Ok(())
    } else {
        invalid(field)
    }
}

fn validate_sha256(value: &str, field: &'static str) -> Result<(), ReceiptError> {
    if is_lower_hex(value, 64) {
        Ok(())
    } else {
        invalid(field)
    }
}

fn validate_timestamp(value: &str, field: &'static str) -> Result<(), ReceiptError> {
    UtcTimestamp::parse(value)
        .map(|_| ())
        .map_err(|_| ReceiptError::InvalidField { field })
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn invalid<T>(field: &'static str) -> Result<T, ReceiptError> {
    Err(ReceiptError::InvalidField { field })
}

fn stage_receipt(
    checkout_root: &Path,
    version: &str,
    temp_filename: &'static str,
    final_filename: &'static str,
    bytes: &[u8],
    replace_final: bool,
) -> Result<StagedReceipt, ReceiptError> {
    canonical_version(version)?;
    let checkout = ContainedRoot::new(
        checkout_root,
        "release checkout",
        UnixModePolicy::AllowExecute,
    )
    .map_err(|_| ReceiptError::CheckoutContainment)?;
    let evidence_relative = evidence_relative_path(version)?;
    create_contained_directory(
        checkout.path(),
        checkout.canonical_path(),
        &evidence_relative,
    )
    .map_err(|_| ReceiptError::EvidenceDirectoryInvalid)?;
    let evidence = contained_evidence_root(&checkout, &evidence_relative)?;
    let existing_final = if replace_final {
        match fs::symlink_metadata(evidence.canonical_path().join(final_filename)) {
            Ok(_) => {
                let bytes = evidence
                    .read(final_filename, "existing release receipt")
                    .map_err(|_| ReceiptError::FinalTargetChanged)?;
                let receipt: FinalizationReceipt =
                    serde_json::from_slice(&bytes).map_err(|_| ReceiptError::FinalTargetInvalid)?;
                if render_finalization_receipt(&receipt)? != bytes {
                    return Err(ReceiptError::FinalTargetInvalid);
                }
                Some(ReceiptIdentity {
                    sha256: Sha256::digest(&bytes).into(),
                    len: u64::try_from(bytes.len())
                        .map_err(|_| ReceiptError::FinalTargetChanged)?,
                })
            }
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(_) => return Err(ReceiptError::EvidenceDirectoryInvalid),
        }
    } else {
        require_absent(
            &evidence.canonical_path().join(final_filename),
            ReceiptError::FinalTargetExists,
        )?;
        None
    };
    require_absent(
        &evidence.canonical_path().join(temp_filename),
        ReceiptError::StagedTargetExists,
    )?;

    let temp_path = evidence.canonical_path().join(temp_filename);
    write_new_staged_file(&temp_path, |file| file.write_all(bytes))?;
    let expected_sha256: [u8; 32] = Sha256::digest(bytes).into();
    let expected_len = u64::try_from(bytes.len()).map_err(|_| ReceiptError::StageWriteFailed)?;
    let staged = StagedReceipt {
        checkout_root: checkout.path().to_path_buf(),
        canonical_checkout: checkout.canonical_path().to_path_buf(),
        evidence_relative,
        temp_filename,
        final_filename,
        expected_sha256,
        expected_len,
        existing_final,
        replace_final,
        remove_temp_on_drop: true,
    };
    let observed = evidence
        .read(temp_filename, "staged release receipt")
        .map_err(|_| ReceiptError::StagedBytesChanged)?;
    if observed != bytes {
        return Err(ReceiptError::StagedBytesChanged);
    }
    Ok(staged)
}

fn contained_evidence_root(
    checkout: &ContainedRoot,
    evidence_relative: &str,
) -> Result<ContainedRoot, ReceiptError> {
    let verified = verify_contained_path(
        checkout.path(),
        checkout.canonical_path(),
        evidence_relative,
        "release checkout",
        "release evidence directory",
    )
    .map_err(|_| ReceiptError::EvidenceDirectoryInvalid)?;
    if !verified.metadata().file_type().is_dir() {
        return Err(ReceiptError::EvidenceDirectoryInvalid);
    }
    ContainedRoot::new(
        verified.canonical_path(),
        "release evidence directory",
        UnixModePolicy::AllowExecute,
    )
    .map_err(|_| ReceiptError::EvidenceDirectoryInvalid)
}

fn require_absent(path: &Path, exists_error: ReceiptError) -> Result<(), ReceiptError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(exists_error),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(_) => Err(ReceiptError::EvidenceDirectoryInvalid),
    }
}

fn write_new_staged_file(
    path: &Path,
    write: impl FnOnce(&mut fs::File) -> std::io::Result<()>,
) -> Result<(), ReceiptError> {
    let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            return Err(ReceiptError::StagedTargetExists)
        }
        Err(_) => return Err(ReceiptError::StageWriteFailed),
    };
    if write(&mut file).and_then(|()| file.sync_all()).is_err() {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(ReceiptError::StageWriteFailed);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_mid_write_removes_the_new_partial_file() {
        let root = std::env::temp_dir().join(format!(
            "solstone-receipt-write-failure-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir(&root).expect("create test directory");
        let path = root.join("receipt.tmp");
        let error = write_new_staged_file(&path, |file| {
            file.write_all(b"partial")?;
            Err(std::io::Error::other("injected write failure"))
        })
        .expect_err("injected failure must fail");
        assert_eq!(error, ReceiptError::StageWriteFailed);
        assert!(!path.exists());
        fs::remove_dir(&root).expect("remove test directory");
    }
}
