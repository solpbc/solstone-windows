// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Filesystem confinement and atomic promotion primitives for release finalization.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::hash::{BuildHasher, Hasher, RandomState};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use semver::Version;

use crate::artifact_fs::{
    self, check_case_collision, validate_relative_path, verify_contained_path, ContainedRoot,
    UnixModePolicy,
};
use crate::rust_release_manifest::{companion_basename, BundleNames};

const RELEASES_DIR: &str = "Releases";
const CANDIDATE_DIR: &str = "target/release-candidate";
pub(crate) const FINALIZATION_RECEIPT: &str = "rust-release-finalization.json";
pub(crate) const FINALIZATION_RECEIPT_TEMP: &str = ".rust-release-finalization.json.tmp";
pub(crate) const WINDOWS_NATIVE_PROOF: &str = "windows-native-proof.json";
const TEMP_NONCE_ATTEMPTS: usize = 16;

static NEXT_TEMP_NONCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CatalogTargetKind {
    File,
    DirectoryTree,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CatalogTarget {
    relative: String,
    authority: String,
    kind: CatalogTargetKind,
}

/// Complete, version-bound catalog of paths a finalization cleanup may remove.
#[derive(Clone, Debug)]
pub struct ReleaseCleanupCatalog {
    checkout_root: PathBuf,
    canonical_checkout: PathBuf,
    version: Version,
    targets: Vec<CatalogTarget>,
    finalization_receipt: String,
    finalization_receipt_identity: Option<(PathBuf, MetadataIdentity)>,
    native_proof: String,
}

/// A cleanup operation fully discovered and verified before any mutation.
#[derive(Clone, Debug)]
pub struct DeletionPlan {
    catalog: ReleaseCleanupCatalog,
    delta_base_fulls: Vec<String>,
    entries: BTreeMap<String, PlannedEntry>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PlannedKind {
    File,
    Directory,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PlannedEntry {
    relative: String,
    authority: String,
    canonical: PathBuf,
    kind: PlannedKind,
    identity: MetadataIdentity,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MetadataIdentity {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    mode: u32,
    #[cfg(windows)]
    attributes: u32,
    #[cfg(windows)]
    creation_time: u64,
    #[cfg(windows)]
    last_write_time: u64,
    #[cfg(windows)]
    volume_serial_number: Option<u32>,
    #[cfg(windows)]
    file_index: Option<u64>,
}

/// A newly-created candidate directory which can only promote beside itself.
#[derive(Debug)]
pub struct CandidateTempDir {
    checkout_root: PathBuf,
    canonical_checkout: PathBuf,
    version: Version,
    relative: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReleaseFinalizerFsError {
    InvalidVersion,
    Confinement { path: String },
    CatalogKind { path: String },
    DuplicateDeltaBase,
    InvalidDeltaBase,
    DeltaBaseNotOlder,
    DeltaBaseMissing,
    DeltaBaseNotRegular,
    UnknownReleasesEntry,
    NativeProofExists,
    FilesystemChanged,
    MutationFailed { path: String },
    CandidateParentInvalid,
    CandidateTempCreationFailed,
    CandidateTempInvalid,
    PromotionTargetExists,
    PromotionFailed,
}

impl fmt::Display for ReleaseFinalizerFsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidVersion => write!(
                formatter,
                "release version is not canonical SemVer; pass the exact cargo metadata version"
            ),
            Self::Confinement { path } => write!(
                formatter,
                "release cleanup confinement failed for {path}; remove the link, reparse point, special file, collision, or escaped path and retry"
            ),
            Self::CatalogKind { path } => write!(
                formatter,
                "release cleanup expected the cataloged kind at {path}; restore a regular contained file or directory and retry"
            ),
            Self::DuplicateDeltaBase => write!(
                formatter,
                "a delta-base full package was supplied more than once; pass each canonical historical full basename exactly once"
            ),
            Self::InvalidDeltaBase => write!(
                formatter,
                "a delta-base full package name is not canonical; pass Solstone-<SEMVER>-full.nupkg"
            ),
            Self::DeltaBaseNotOlder => write!(
                formatter,
                "a delta-base full package is not older than the candidate; choose a strictly older canonical version"
            ),
            Self::DeltaBaseMissing => write!(
                formatter,
                "a requested delta-base full package is missing from Releases; restore that exact package or remove the delta-base argument"
            ),
            Self::DeltaBaseNotRegular => write!(
                formatter,
                "a requested delta-base full package is not one regular contained file; replace it with the exact historical package and retry"
            ),
            Self::UnknownReleasesEntry => write!(
                formatter,
                "Releases contains an unknown entry; move non-canonical content out of Releases and retry"
            ),
            Self::NativeProofExists => write!(
                formatter,
                "windows-native-proof.json already exists for this version; use a new version instead of re-finalizing proven bytes"
            ),
            Self::FilesystemChanged => write!(
                formatter,
                "release cleanup inputs changed after preflight; restore the inspected tree and retry from a new transaction"
            ),
            Self::MutationFailed { path } => write!(
                formatter,
                "release cleanup could not remove {path}; restore permissions or close the process holding it and retry"
            ),
            Self::CandidateParentInvalid => write!(
                formatter,
                "candidate staging parent is not a real contained directory; remove links or reparse points beneath target and retry"
            ),
            Self::CandidateTempCreationFailed => write!(
                formatter,
                "could not create a unique candidate staging directory; clear stale cataloged staging directories and retry"
            ),
            Self::CandidateTempInvalid => write!(
                formatter,
                "candidate staging directory is missing, non-empty, linked, reparsed, or outside its authority; rebuild it from a new empty staging directory"
            ),
            Self::PromotionTargetExists => write!(
                formatter,
                "the final candidate path already exists; run the confined cleanup before promotion and retry"
            ),
            Self::PromotionFailed => write!(
                formatter,
                "candidate atomic promotion failed; keep the staging directory on the same filesystem and retry"
            ),
        }
    }
}

impl std::error::Error for ReleaseFinalizerFsError {}

impl ReleaseCleanupCatalog {
    /// Materialize all and only the namespaces finalization may delete.
    pub fn for_version(
        checkout_root: &Path,
        version: &str,
    ) -> Result<Self, ReleaseFinalizerFsError> {
        let version = parse_canonical_version(version)?;
        let checkout = ContainedRoot::new(checkout_root, "checkout", UnixModePolicy::AllowExecute)
            .map_err(|_| confinement("checkout"))?;
        let canonical_checkout = checkout.canonical_path().to_path_buf();
        let checkout_root = checkout.path().to_path_buf();
        let version_text = version.to_string();
        let names = BundleNames::for_version(&version_text);

        let mut targets = Vec::new();
        for name in names.artifact_names(true) {
            targets.push(CatalogTarget::file(
                format!("{RELEASES_DIR}/{name}"),
                RELEASES_DIR,
            ));
        }
        targets.push(CatalogTarget::file(
            format!("{RELEASES_DIR}/{}", BundleNames::velopack_setup_exe()),
            RELEASES_DIR,
        ));

        let releases_companion = format!("{RELEASES_DIR}/{}", companion_basename());
        if existing_relative(&checkout_root, &canonical_checkout, &releases_companion)? {
            targets.push(CatalogTarget::file(releases_companion, RELEASES_DIR));
        }
        catalog_historical_release_packages(&checkout_root, &canonical_checkout, &mut targets)?;

        targets.extend([
            CatalogTarget::directory(
                format!("target/release-finalizer/{version_text}"),
                "target/release-finalizer",
            ),
            CatalogTarget::directory("target/vpk-stage", "target"),
            CatalogTarget::file(format!("target/release-notes-{version_text}.md"), "target"),
            CatalogTarget::directory(format!("{CANDIDATE_DIR}/{version_text}"), CANDIDATE_DIR),
            CatalogTarget::file(
                format!("target/release-evidence/{version_text}/{FINALIZATION_RECEIPT_TEMP}"),
                format!("target/release-evidence/{version_text}"),
            ),
        ]);

        let candidate_parent = CANDIDATE_DIR.to_owned();
        if existing_relative(&checkout_root, &canonical_checkout, &candidate_parent)? {
            let parent = verify_existing(&checkout_root, &canonical_checkout, &candidate_parent)?;
            if !parent.metadata().file_type().is_dir() {
                return Err(ReleaseFinalizerFsError::CatalogKind {
                    path: candidate_parent,
                });
            }
            let prefix = format!(".{version_text}.finalize-");
            let suffix = ".tmp";
            let mut folded_temps = BTreeMap::new();
            let entries = fs::read_dir(checkout_root.join(CANDIDATE_DIR))
                .map_err(|_| confinement(CANDIDATE_DIR))?;
            for entry in entries {
                let entry = entry.map_err(|_| confinement(CANDIDATE_DIR))?;
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| confinement(CANDIDATE_DIR))?;
                if is_candidate_temp_name(
                    &name.to_ascii_lowercase(),
                    &prefix.to_ascii_lowercase(),
                    suffix,
                ) {
                    check_case_collision(&mut folded_temps, &name)
                        .map_err(|_| confinement(CANDIDATE_DIR))?;
                }
                if is_candidate_temp_name(&name, &prefix, suffix) {
                    targets.push(CatalogTarget::directory(
                        format!("{CANDIDATE_DIR}/{name}"),
                        CANDIDATE_DIR,
                    ));
                }
            }
        }

        let evidence_authority = format!("target/release-evidence/{version_text}");
        let native_proof = format!("{evidence_authority}/{WINDOWS_NATIVE_PROOF}");
        if existing_relative(&checkout_root, &canonical_checkout, &native_proof)? {
            return Err(ReleaseFinalizerFsError::NativeProofExists);
        }
        let finalization_receipt = format!("{evidence_authority}/{FINALIZATION_RECEIPT}");
        let finalization_receipt_identity =
            if existing_relative(&checkout_root, &canonical_checkout, &finalization_receipt)? {
                let verified =
                    verify_existing(&checkout_root, &canonical_checkout, &finalization_receipt)?;
                if !verified.metadata().file_type().is_file() {
                    return Err(ReleaseFinalizerFsError::CatalogKind {
                        path: finalization_receipt.clone(),
                    });
                }
                Some((
                    verified.canonical_path().to_path_buf(),
                    MetadataIdentity::from_metadata(verified.metadata()),
                ))
            } else {
                None
            };

        let legacy_authority = format!("target/release-manifest/{version_text}");
        let legacy_manifest = format!("{legacy_authority}/{}", companion_basename());
        if existing_relative(&checkout_root, &canonical_checkout, &legacy_manifest)? {
            targets.push(CatalogTarget::file(legacy_manifest, legacy_authority));
        }

        targets.sort_by(|left, right| left.relative.cmp(&right.relative));
        Ok(Self {
            checkout_root,
            canonical_checkout,
            version,
            targets,
            finalization_receipt,
            finalization_receipt_identity,
            native_proof,
        })
    }

    pub fn deletable_paths(&self) -> Vec<String> {
        self.targets
            .iter()
            .map(|target| target.relative.clone())
            .collect()
    }

    pub fn finalization_receipt_path(&self) -> &str {
        &self.finalization_receipt
    }

    pub fn native_proof_path(&self) -> &str {
        &self.native_proof
    }
}

impl CatalogTarget {
    fn file(relative: impl Into<String>, authority: impl Into<String>) -> Self {
        Self {
            relative: relative.into(),
            authority: authority.into(),
            kind: CatalogTargetKind::File,
        }
    }

    fn directory(relative: impl Into<String>, authority: impl Into<String>) -> Self {
        Self {
            relative: relative.into(),
            authority: authority.into(),
            kind: CatalogTargetKind::DirectoryTree,
        }
    }
}

impl DeletionPlan {
    /// Fully discover and validate a cleanup. This function never mutates the tree.
    pub fn materialize(
        catalog: &ReleaseCleanupCatalog,
        delta_base_fulls: &[String],
    ) -> Result<Self, ReleaseFinalizerFsError> {
        let allowlist = validate_delta_base_allowlist(catalog, delta_base_fulls)?;
        let deletion_targets: Vec<CatalogTarget> = catalog
            .targets
            .iter()
            .filter(|target| {
                target
                    .relative
                    .strip_prefix("Releases/")
                    .is_none_or(|basename| !allowlist.contains(basename))
            })
            .cloned()
            .collect();

        let mut entries = BTreeMap::new();
        let mut folded = BTreeMap::new();
        for target in &deletion_targets {
            validate_potential_path(catalog, &target.authority, &target.relative)?;
            if !existing_relative(
                &catalog.checkout_root,
                &catalog.canonical_checkout,
                &target.relative,
            )? {
                continue;
            }
            let verified = verify_under_authority(catalog, &target.authority, &target.relative)?;
            match target.kind {
                CatalogTargetKind::File => {
                    if !verified.metadata().file_type().is_file() {
                        return Err(ReleaseFinalizerFsError::CatalogKind {
                            path: target.relative.clone(),
                        });
                    }
                    insert_planned(
                        catalog,
                        &mut entries,
                        &mut folded,
                        &target.relative,
                        &target.authority,
                    )?;
                }
                CatalogTargetKind::DirectoryTree => {
                    if !verified.metadata().file_type().is_dir() {
                        return Err(ReleaseFinalizerFsError::CatalogKind {
                            path: target.relative.clone(),
                        });
                    }
                    let inventory = artifact_fs::walk_directory(
                        &catalog.checkout_root.join(&target.relative),
                        &target.relative,
                        UnixModePolicy::AllowExecute,
                    )
                    .map_err(|_| confinement(&target.relative))?;
                    insert_planned(
                        catalog,
                        &mut entries,
                        &mut folded,
                        &target.relative,
                        &target.authority,
                    )?;
                    for child in inventory.files.iter().chain(inventory.directories.iter()) {
                        let relative = format!("{}/{child}", target.relative);
                        insert_planned(
                            catalog,
                            &mut entries,
                            &mut folded,
                            &relative,
                            &target.authority,
                        )?;
                    }
                }
            }
        }

        for retained in &allowlist {
            let relative = format!("{RELEASES_DIR}/{retained}");
            validate_potential_path(catalog, RELEASES_DIR, &relative)?;
            let verified = verify_under_authority(catalog, RELEASES_DIR, &relative)
                .map_err(|_| ReleaseFinalizerFsError::DeltaBaseNotRegular)?;
            if !verified.metadata().file_type().is_file() {
                return Err(ReleaseFinalizerFsError::DeltaBaseNotRegular);
            }
            check_case_collision(&mut folded, &relative).map_err(|_| confinement(&relative))?;
        }

        Ok(Self {
            catalog: catalog.clone(),
            delta_base_fulls: allowlist.into_iter().collect(),
            entries,
        })
    }

    pub fn paths(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }

    pub fn retained_delta_bases(&self) -> &[String] {
        &self.delta_base_fulls
    }

    /// Re-verify the complete plan, then remove leaves before empty directories.
    pub fn execute(self) -> Result<(), ReleaseFinalizerFsError> {
        let refreshed = ReleaseCleanupCatalog::for_version(
            &self.catalog.checkout_root,
            &self.catalog.version.to_string(),
        )?;
        let rechecked = Self::materialize(&refreshed, &self.delta_base_fulls)?;
        if rechecked.entries != self.entries
            || refreshed.finalization_receipt_identity != self.catalog.finalization_receipt_identity
        {
            return Err(ReleaseFinalizerFsError::FilesystemChanged);
        }

        let mut files: Vec<&PlannedEntry> = self
            .entries
            .values()
            .filter(|entry| entry.kind == PlannedKind::File)
            .collect();
        files.sort_by(|left, right| deepest_first(&left.relative, &right.relative));
        for entry in files {
            fs::remove_file(self.catalog.checkout_root.join(&entry.relative)).map_err(|_| {
                ReleaseFinalizerFsError::MutationFailed {
                    path: entry.relative.clone(),
                }
            })?;
        }

        let mut directories: Vec<&PlannedEntry> = self
            .entries
            .values()
            .filter(|entry| entry.kind == PlannedKind::Directory)
            .collect();
        directories.sort_by(|left, right| deepest_first(&left.relative, &right.relative));
        for entry in directories {
            fs::remove_dir(self.catalog.checkout_root.join(&entry.relative)).map_err(|_| {
                ReleaseFinalizerFsError::MutationFailed {
                    path: entry.relative.clone(),
                }
            })?;
        }
        Ok(())
    }
}

impl CandidateTempDir {
    pub fn path(&self) -> PathBuf {
        self.checkout_root.join(&self.relative)
    }

    pub fn relative_path(&self) -> &str {
        &self.relative
    }

    /// Atomically rename the assembled sibling into the final candidate name.
    pub fn promote(self) -> Result<PathBuf, ReleaseFinalizerFsError> {
        let verified = verify_under_checkout(
            &self.checkout_root,
            &self.canonical_checkout,
            &self.relative,
        )
        .map_err(|_| ReleaseFinalizerFsError::CandidateTempInvalid)?;
        if !verified.metadata().file_type().is_dir() {
            return Err(ReleaseFinalizerFsError::CandidateTempInvalid);
        }
        let parent = CANDIDATE_DIR.to_owned();
        let parent_verified =
            verify_under_checkout(&self.checkout_root, &self.canonical_checkout, &parent)
                .map_err(|_| ReleaseFinalizerFsError::CandidateParentInvalid)?;
        if !parent_verified.metadata().file_type().is_dir()
            || !verified
                .canonical_path()
                .starts_with(parent_verified.canonical_path())
        {
            return Err(ReleaseFinalizerFsError::CandidateTempInvalid);
        }

        let final_relative = format!("{CANDIDATE_DIR}/{}", self.version);
        match fs::symlink_metadata(self.checkout_root.join(&final_relative)) {
            Ok(_) => return Err(ReleaseFinalizerFsError::PromotionTargetExists),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(_) => return Err(ReleaseFinalizerFsError::PromotionTargetExists),
        }
        let final_path = self.checkout_root.join(&final_relative);
        fs::rename(self.path(), &final_path)
            .map_err(|_| ReleaseFinalizerFsError::PromotionFailed)?;
        Ok(final_path)
    }
}

/// Create a newly empty candidate staging directory beside its final target.
pub fn create_candidate_temp(
    checkout_root: &Path,
    version: &str,
) -> Result<CandidateTempDir, ReleaseFinalizerFsError> {
    let version = parse_canonical_version(version)?;
    let checkout = ContainedRoot::new(checkout_root, "checkout", UnixModePolicy::AllowExecute)
        .map_err(|_| confinement("checkout"))?;
    let checkout_root = checkout.path().to_path_buf();
    let canonical_checkout = checkout.canonical_path().to_path_buf();
    create_contained_directory(&checkout_root, &canonical_checkout, "target")?;
    create_contained_directory(&checkout_root, &canonical_checkout, CANDIDATE_DIR)?;

    let version_text = version.to_string();
    for _ in 0..TEMP_NONCE_ATTEMPTS {
        let suffix = random_suffix();
        let relative = format!("{CANDIDATE_DIR}/.{version_text}.finalize-{suffix}.tmp");
        match fs::create_dir(checkout_root.join(&relative)) {
            Ok(()) => {
                let verified =
                    verify_under_checkout(&checkout_root, &canonical_checkout, &relative)
                        .map_err(|_| ReleaseFinalizerFsError::CandidateTempInvalid)?;
                if !verified.metadata().file_type().is_dir()
                    || fs::read_dir(checkout_root.join(&relative))
                        .map_err(|_| ReleaseFinalizerFsError::CandidateTempInvalid)?
                        .next()
                        .is_some()
                {
                    return Err(ReleaseFinalizerFsError::CandidateTempInvalid);
                }
                return Ok(CandidateTempDir {
                    checkout_root,
                    canonical_checkout,
                    version,
                    relative,
                });
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(ReleaseFinalizerFsError::CandidateTempCreationFailed),
        }
    }
    Err(ReleaseFinalizerFsError::CandidateTempCreationFailed)
}

fn validate_delta_base_allowlist(
    catalog: &ReleaseCleanupCatalog,
    delta_base_fulls: &[String],
) -> Result<BTreeSet<String>, ReleaseFinalizerFsError> {
    let mut allowlist = BTreeSet::new();
    for basename in delta_base_fulls {
        let version = parse_full_package_basename(basename)
            .ok_or(ReleaseFinalizerFsError::InvalidDeltaBase)?;
        if version >= catalog.version {
            return Err(ReleaseFinalizerFsError::DeltaBaseNotOlder);
        }
        if !allowlist.insert(basename.clone()) {
            return Err(ReleaseFinalizerFsError::DuplicateDeltaBase);
        }
    }
    for basename in &allowlist {
        let relative = format!("{RELEASES_DIR}/{basename}");
        if !existing_relative(
            &catalog.checkout_root,
            &catalog.canonical_checkout,
            &relative,
        )? {
            return Err(ReleaseFinalizerFsError::DeltaBaseMissing);
        }
        let verified = verify_under_authority(catalog, RELEASES_DIR, &relative)
            .map_err(|_| ReleaseFinalizerFsError::DeltaBaseNotRegular)?;
        if !verified.metadata().file_type().is_file() {
            return Err(ReleaseFinalizerFsError::DeltaBaseNotRegular);
        }
    }
    Ok(allowlist)
}

fn catalog_historical_release_packages(
    checkout_root: &Path,
    canonical_checkout: &Path,
    targets: &mut Vec<CatalogTarget>,
) -> Result<(), ReleaseFinalizerFsError> {
    if !existing_relative(checkout_root, canonical_checkout, RELEASES_DIR)? {
        return Ok(());
    }
    let releases = verify_existing(checkout_root, canonical_checkout, RELEASES_DIR)?;
    if !releases.metadata().file_type().is_dir() {
        return Err(ReleaseFinalizerFsError::CatalogKind {
            path: RELEASES_DIR.to_owned(),
        });
    }

    let current: BTreeSet<String> = targets
        .iter()
        .filter_map(|target| target.relative.strip_prefix("Releases/").map(str::to_owned))
        .collect();
    let entries =
        fs::read_dir(checkout_root.join(RELEASES_DIR)).map_err(|_| confinement(RELEASES_DIR))?;
    for entry in entries {
        let entry = entry.map_err(|_| confinement(RELEASES_DIR))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| ReleaseFinalizerFsError::UnknownReleasesEntry)?;
        let relative = format!("{RELEASES_DIR}/{name}");
        if current.contains(&name) {
            continue;
        }
        if parse_full_package_basename(&name).is_some()
            || parse_delta_package_basename(&name).is_some()
        {
            targets.push(CatalogTarget::file(relative, RELEASES_DIR));
            continue;
        }
        return Err(ReleaseFinalizerFsError::UnknownReleasesEntry);
    }
    Ok(())
}

fn parse_full_package_basename(basename: &str) -> Option<Version> {
    parse_package_basename(basename, "-full.nupkg", |names| {
        names.full_package().to_owned()
    })
}

fn parse_delta_package_basename(basename: &str) -> Option<Version> {
    parse_package_basename(basename, "-delta.nupkg", |names| {
        names.delta_package().to_owned()
    })
}

fn parse_package_basename(
    basename: &str,
    suffix: &str,
    expected: impl FnOnce(&BundleNames) -> String,
) -> Option<Version> {
    let version_text = basename.strip_prefix("Solstone-")?.strip_suffix(suffix)?;
    let version = Version::parse(version_text).ok()?;
    if version.to_string() != version_text {
        return None;
    }
    let names = BundleNames::for_version(version_text);
    (expected(&names) == basename).then_some(version)
}

fn insert_planned(
    catalog: &ReleaseCleanupCatalog,
    entries: &mut BTreeMap<String, PlannedEntry>,
    folded: &mut BTreeMap<String, String>,
    relative: &str,
    authority: &str,
) -> Result<(), ReleaseFinalizerFsError> {
    if entries.contains_key(relative) {
        return Ok(());
    }
    check_case_collision(folded, relative).map_err(|_| confinement(relative))?;
    let verified = verify_under_authority(catalog, authority, relative)?;
    let metadata = verified.metadata();
    let kind = if metadata.file_type().is_file() {
        PlannedKind::File
    } else if metadata.file_type().is_dir() {
        PlannedKind::Directory
    } else {
        return Err(confinement(relative));
    };
    entries.insert(
        relative.to_owned(),
        PlannedEntry {
            relative: relative.to_owned(),
            authority: authority.to_owned(),
            canonical: verified.canonical_path().to_path_buf(),
            kind,
            identity: MetadataIdentity::from_metadata(metadata),
        },
    );
    Ok(())
}

impl MetadataIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        #[cfg(windows)]
        use std::os::windows::fs::MetadataExt;

        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(unix)]
            mode: metadata.mode(),
            #[cfg(windows)]
            attributes: metadata.file_attributes(),
            #[cfg(windows)]
            creation_time: metadata.creation_time(),
            #[cfg(windows)]
            last_write_time: metadata.last_write_time(),
            #[cfg(windows)]
            volume_serial_number: metadata.volume_serial_number(),
            #[cfg(windows)]
            file_index: metadata.file_index(),
        }
    }
}

fn validate_potential_path(
    catalog: &ReleaseCleanupCatalog,
    authority: &str,
    relative: &str,
) -> Result<(), ReleaseFinalizerFsError> {
    validate_relative_path(authority).map_err(|_| confinement(authority))?;
    validate_relative_path(relative).map_err(|_| confinement(relative))?;
    if relative == authority || !relative.starts_with(&format!("{authority}/")) {
        return Err(confinement(relative));
    }
    verify_deepest_existing(catalog, authority)?;
    verify_deepest_existing(catalog, relative)?;
    Ok(())
}

fn verify_deepest_existing(
    catalog: &ReleaseCleanupCatalog,
    relative: &str,
) -> Result<(), ReleaseFinalizerFsError> {
    validate_relative_path(relative).map_err(|_| confinement(relative))?;
    verify_checkout_root(&catalog.checkout_root, &catalog.canonical_checkout)?;
    let components: Vec<&str> = relative.split('/').collect();
    let mut deepest = None;
    for end in 1..=components.len() {
        let prefix = components[..end].join("/");
        match fs::symlink_metadata(catalog.checkout_root.join(&prefix)) {
            Ok(_) => deepest = Some(prefix),
            Err(error) if error.kind() == ErrorKind::NotFound => break,
            Err(_) => return Err(confinement(relative)),
        }
    }
    if let Some(deepest) = deepest {
        let verified = verify_under_checkout(
            &catalog.checkout_root,
            &catalog.canonical_checkout,
            &deepest,
        )?;
        if deepest != relative && !verified.metadata().file_type().is_dir() {
            return Err(confinement(relative));
        }
    }
    Ok(())
}

fn verify_under_authority(
    catalog: &ReleaseCleanupCatalog,
    authority: &str,
    relative: &str,
) -> Result<artifact_fs::VerifiedContainedPath, ReleaseFinalizerFsError> {
    let authority_verified = verify_under_checkout(
        &catalog.checkout_root,
        &catalog.canonical_checkout,
        authority,
    )?;
    if !authority_verified.metadata().file_type().is_dir() {
        return Err(confinement(authority));
    }
    let verified = verify_under_checkout(
        &catalog.checkout_root,
        &catalog.canonical_checkout,
        relative,
    )?;
    if verified.canonical_path() == authority_verified.canonical_path()
        || !verified
            .canonical_path()
            .starts_with(authority_verified.canonical_path())
    {
        return Err(confinement(relative));
    }
    Ok(verified)
}

fn verify_existing(
    checkout_root: &Path,
    canonical_checkout: &Path,
    relative: &str,
) -> Result<artifact_fs::VerifiedContainedPath, ReleaseFinalizerFsError> {
    verify_under_checkout(checkout_root, canonical_checkout, relative)
}

fn verify_under_checkout(
    checkout_root: &Path,
    canonical_checkout: &Path,
    relative: &str,
) -> Result<artifact_fs::VerifiedContainedPath, ReleaseFinalizerFsError> {
    verify_contained_path(
        checkout_root,
        canonical_checkout,
        relative,
        "checkout",
        relative,
    )
    .map_err(|_| confinement(relative))
}

fn existing_relative(
    checkout_root: &Path,
    canonical_checkout: &Path,
    relative: &str,
) -> Result<bool, ReleaseFinalizerFsError> {
    validate_relative_path(relative).map_err(|_| confinement(relative))?;
    verify_checkout_root(checkout_root, canonical_checkout)?;
    let components: Vec<&str> = relative.split('/').collect();
    let mut deepest = None;
    for end in 1..=components.len() {
        let prefix = components[..end].join("/");
        match fs::symlink_metadata(checkout_root.join(&prefix)) {
            Ok(_) => deepest = Some(prefix),
            Err(error) if error.kind() == ErrorKind::NotFound => {
                if let Some(deepest) = deepest {
                    let verified =
                        verify_under_checkout(checkout_root, canonical_checkout, &deepest)?;
                    if !verified.metadata().file_type().is_dir() {
                        return Err(confinement(relative));
                    }
                }
                return Ok(false);
            }
            Err(_) => return Err(confinement(relative)),
        }
    }
    verify_under_checkout(checkout_root, canonical_checkout, relative)?;
    Ok(true)
}

fn verify_checkout_root(
    checkout_root: &Path,
    canonical_checkout: &Path,
) -> Result<(), ReleaseFinalizerFsError> {
    let observed = ContainedRoot::new(checkout_root, "checkout", UnixModePolicy::AllowExecute)
        .map_err(|_| confinement("checkout"))?;
    if observed.canonical_path() != canonical_checkout {
        return Err(confinement("checkout"));
    }
    Ok(())
}

pub(crate) fn create_contained_directory(
    checkout_root: &Path,
    canonical_checkout: &Path,
    relative: &str,
) -> Result<(), ReleaseFinalizerFsError> {
    let mut prefix = String::new();
    for component in relative.split('/') {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(component);
        match fs::symlink_metadata(checkout_root.join(&prefix)) {
            Ok(_) => {
                let verified = verify_under_checkout(checkout_root, canonical_checkout, &prefix)
                    .map_err(|_| ReleaseFinalizerFsError::CandidateParentInvalid)?;
                if !verified.metadata().file_type().is_dir() {
                    return Err(ReleaseFinalizerFsError::CandidateParentInvalid);
                }
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                fs::create_dir(checkout_root.join(&prefix))
                    .map_err(|_| ReleaseFinalizerFsError::CandidateParentInvalid)?;
                let verified = verify_under_checkout(checkout_root, canonical_checkout, &prefix)
                    .map_err(|_| ReleaseFinalizerFsError::CandidateParentInvalid)?;
                if !verified.metadata().file_type().is_dir() {
                    return Err(ReleaseFinalizerFsError::CandidateParentInvalid);
                }
            }
            Err(_) => return Err(ReleaseFinalizerFsError::CandidateParentInvalid),
        }
    }
    Ok(())
}

fn is_candidate_temp_name(name: &str, prefix: &str, suffix: &str) -> bool {
    name.strip_prefix(prefix)
        .and_then(|rest| rest.strip_suffix(suffix))
        .is_some_and(|nonce| {
            nonce.len() == 32
                && nonce
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

fn parse_canonical_version(version: &str) -> Result<Version, ReleaseFinalizerFsError> {
    let parsed = Version::parse(version).map_err(|_| ReleaseFinalizerFsError::InvalidVersion)?;
    if parsed.to_string() != version {
        return Err(ReleaseFinalizerFsError::InvalidVersion);
    }
    Ok(parsed)
}

fn random_suffix() -> String {
    let sequence = NEXT_TEMP_NONCE.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let state = RandomState::new();
    let mut first = state.build_hasher();
    first.write_u32(std::process::id());
    first.write_u64(sequence);
    first.write_u128(timestamp);
    let mut second = state.build_hasher();
    second.write_u128(timestamp.rotate_left(37));
    second.write_u64(sequence.rotate_left(19));
    format!("{:016x}{:016x}", first.finish(), second.finish())
}

fn deepest_first(left: &str, right: &str) -> std::cmp::Ordering {
    right
        .split('/')
        .count()
        .cmp(&left.split('/').count())
        .then_with(|| right.cmp(left))
}

fn confinement(path: &str) -> ReleaseFinalizerFsError {
    ReleaseFinalizerFsError::Confinement {
        path: path.to_owned(),
    }
}

// A live NTFS junction exercises the same reject_reparse_point path on Windows;
// that native junction mutation remains a post-ship box witness.
