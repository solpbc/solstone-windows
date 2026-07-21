// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Deterministic cargo-deny advisory policy and isolated RustSec snapshots.

use std::collections::BTreeSet;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use semver::Version;
use serde::Serialize;
use sha2::{Digest, Sha256};
use toml::Value;

use crate::artifact_fs::{self, verify_contained_path, ContainedRoot, UnixModePolicy};
use crate::release_clock::{Clock, UtcTimestamp};
use crate::release_exec::{CommandOutput, CommandRunner};
use crate::release_selection::SelectedAction;

pub const RUSTSEC_SOURCE_ID: &str = "https://github.com/RustSec/advisory-db";
pub const ADVISORY_DB_RELATIVE: &str = "target/release-advisory-db";
const MAX_SNAPSHOT_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const EXPECTED_IGNORE_IDS: [&str; 2] = ["RUSTSEC-2026-0194", "RUSTSEC-2026-0195"];

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
                "isolated advisory repository name is not cargo-deny's canonical URL-derived name; reprovision the isolated RustSec cache"
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
                "advisory snapshot source is not the configured RustSec source id; reprovision the isolated cache from the approved source"
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

    let database_root = isolated_database_root
        .to_str()
        .ok_or(AdvisoryError::DatabaseRootInvalid)?;
    let mut rendered = String::new();
    rendered.push_str("[advisories]\n");
    rendered.push_str("db-path = ");
    rendered.push_str(&toml_string(database_root));
    rendered.push('\n');
    rendered.push_str("db-urls = [\"");
    rendered.push_str(RUSTSEC_SOURCE_ID);
    rendered.push_str("\"]\n");
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
    let bytes = render_checkout_advisory_config(&checkout, &database_root)?;

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

    let bytes = render_checkout_advisory_config(&checkout, &database_root)?;
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
) -> Result<Vec<u8>, AdvisoryError> {
    let deny_bytes = checkout
        .read("deny.toml", "deny.toml")
        .map_err(|_| AdvisoryError::PolicyMalformed)?;
    render_advisory_config(&deny_bytes, database_root)
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

impl AdvisorySnapshot {
    pub fn inspect<R: CommandRunner + ?Sized, C: Clock + ?Sized>(
        checkout_root: &Path,
        git_program: &Path,
        recorded_tree_sha256: &str,
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

        let mut children = fs::read_dir(&database_root)
            .map_err(|_| AdvisoryError::DatabaseRootInvalid)?
            .map(|entry| {
                entry
                    .map_err(|_| AdvisoryError::DatabaseRootInvalid)?
                    .file_name()
                    .into_string()
                    .map_err(|_| AdvisoryError::RepositoryName)
            })
            .collect::<Result<Vec<_>, _>>()?;
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

        let repository_arg = repository
            .canonical_path()
            .to_str()
            .ok_or(AdvisoryError::SnapshotContainment)?;
        let status = run_git(
            runner,
            git_program,
            repository_arg,
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
            repository_arg,
            &["remote", "get-url", "origin"],
            "source",
        )?;
        if source.status != 0
            || parse_git_line(&source.stdout).as_deref() != Some(RUSTSEC_SOURCE_ID)
        {
            return Err(AdvisoryError::SourceMismatch);
        }
        let commit_output = run_git(
            runner,
            git_program,
            repository_arg,
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
            repository_arg,
            &["rev-parse", "--is-shallow-repository"],
            "shallow",
        )?;
        if shallow.status != 0 || parse_git_line(&shallow.stdout).as_deref() != Some("false") {
            return Err(AdvisoryError::ShallowRepository);
        }
        let archive = run_git(
            runner,
            git_program,
            repository_arg,
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
            source_id: RUSTSEC_SOURCE_ID.to_owned(),
            commit,
            tree_sha256,
            acquired_at,
        })
    }
}

pub fn run_advisory_check<R: CommandRunner + ?Sized, C: Clock + ?Sized>(
    checkout_root: &Path,
    version: &str,
    git_program: &Path,
    recorded_tree_sha256: &str,
    advisory_action: &SelectedAction,
    runner: &R,
    clock: &C,
) -> Result<AdvisoryProvenance, AdvisoryError> {
    let config = materialize_advisory_config(checkout_root, version)?;
    let snapshot = AdvisorySnapshot::inspect(
        checkout_root,
        git_program,
        recorded_tree_sha256,
        runner,
        clock,
    )?;
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
    let config_path = config
        .path
        .to_str()
        .ok_or(AdvisoryError::ConfigLocationInvalid)?;
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
                config_path.to_owned()
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

fn is_url_derived_repository_name(name: &str) -> bool {
    name.strip_prefix("advisory-db-").is_some_and(|suffix| {
        suffix.len() == 16
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
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
