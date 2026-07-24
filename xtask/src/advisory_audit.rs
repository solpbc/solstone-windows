// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Signed-packet-bound recurring advisory audit.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::Builder;
use twox_hash::XxHash64;
use url::Url;

use crate::artifact_fs::{self, child_process_path_text, ContainedRoot, UnixModePolicy};
use crate::release_advisory::{
    render_advisory_config, validate_freshness_receipt_path, validate_mirror_locator,
    validate_mirror_public_key_path, verify_mirror_freshness, AdvisoryError,
    AdvisoryVerificationScratch, MirrorPacketInputs, MIRROR_COHORT_ID,
};
use crate::release_clock::Clock;
use crate::release_exec::{minisign_version_is_supported, resolve_path_program, CommandRunner};
use crate::rust_release_manifest::{render_canonical_json, PRODUCT};

pub const ADVISORY_AUDIT_SCHEMA: &str = "solstone.advisory-audit.v1";
pub const CARGO_DENY_VERSION: &str = "0.20.2";
const CACHE_HASH_SEED: u64 = 0xca80de71;

pub const ADVISORY_AUDIT_REMOVED_ENV: [&str; 24] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_REPLACE_REF_BASE",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_NOSYSTEM",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_ALLOW_PROTOCOL",
    "GIT_PROTOCOL_FROM_USER",
    "GIT_TERMINAL_PROMPT",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_PROXY_COMMAND",
    "GIT_TEMPLATE_DIR",
    "GIT_EXEC_PATH",
    "GIT_CEILING_DIRECTORIES",
];

#[derive(Clone, Debug)]
pub struct AdvisoryAuditPrograms<'a> {
    pub git: &'a Path,
    pub minisign: &'a Path,
    pub cargo_deny: &'a Path,
}

#[derive(Clone, Debug)]
pub struct ResolvedAdvisoryAuditPrograms {
    pub git: PathBuf,
    pub minisign: PathBuf,
    pub cargo_deny: PathBuf,
}

#[derive(Clone, Debug)]
pub struct AdvisoryAuditTrust<'a> {
    pub public_key_sha256: &'a str,
    pub public_key_id: &'a str,
}

#[derive(Clone, Debug)]
pub struct AdvisoryAuditRequest<'a> {
    pub checkout_root: &'a Path,
    pub locator: &'a str,
    pub receipt_path: &'a Path,
    pub public_key_path: &'a Path,
    pub bundle_path: &'a Path,
    pub programs: AdvisoryAuditPrograms<'a>,
    pub trust: AdvisoryAuditTrust<'a>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AdvisoryAuditWitness {
    pub schema: String,
    pub product: String,
    pub source_cohort: String,
    pub synced_commit: String,
    pub receipt_utc: String,
    pub max_age: u64,
    pub checked_at: String,
    pub cargo_lock_sha256: String,
    pub cargo_deny_version: String,
    pub verdict: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuditError {
    Authority(AdvisoryError),
    BundleInputInvalid,
    LocatorNotUrl,
    GitUnavailable,
    MinisignUnavailable,
    CargoDenyUnavailable,
    MinisignVersionUnsupported,
    CargoDenyVersionUnsupported,
    ScratchInvalid,
    BundleVerificationFailed,
    BundleHeadsInspectionFailed,
    BundleHeadsMismatch,
    DatabaseMaterializationFailed,
    RepositoryCheckoutFailed,
    RepositoryCommitMismatch,
    RepositoryShallow,
    RepositoryDirty,
    PolicyMaterializationFailed,
    CargoLockUnavailable,
    CargoDenyInvocationFailed,
    CargoDenyRejected,
    ClockUnavailable,
    WitnessSerializationFailed,
    CleanupFailed,
}

impl fmt::Display for AuditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Authority(error) => write!(formatter, "{error}"),
            Self::BundleInputInvalid => write!(formatter, "advisory audit bundle-input gate failed; supply one absolute regular bundle file and retry"),
            Self::LocatorNotUrl => write!(formatter, "advisory audit locator-URL gate failed; supply a hierarchical https or ssh mirror URL and retry"),
            Self::GitUnavailable => write!(formatter, "advisory audit Git preflight failed; install Git on PATH and retry"),
            Self::MinisignUnavailable => write!(formatter, "advisory audit minisign preflight failed; install minisign on PATH and retry"),
            Self::CargoDenyUnavailable => write!(formatter, "advisory audit cargo-deny preflight failed; provision the pinned cargo-deny and retry"),
            Self::MinisignVersionUnsupported => write!(formatter, "advisory audit minisign-version gate failed; install minisign 0.11 or 0.12 and retry"),
            Self::CargoDenyVersionUnsupported => write!(formatter, "advisory audit cargo-deny-version gate failed; provision cargo-deny 0.20.2 and retry"),
            Self::ScratchInvalid => write!(formatter, "advisory audit scratch gate failed; restore target-directory permissions and retry"),
            Self::BundleVerificationFailed => write!(formatter, "advisory audit bundle-verification gate failed; obtain a complete signed mirror bundle and retry"),
            Self::BundleHeadsInspectionFailed => write!(formatter, "advisory audit bundle-head inspection failed; obtain a readable complete mirror bundle and retry"),
            Self::BundleHeadsMismatch => write!(formatter, "advisory audit bundle-head binding failed; obtain a bundle advertising only the signed HEAD and main commit and retry"),
            Self::DatabaseMaterializationFailed => write!(formatter, "advisory audit database-materialization gate failed; obtain a complete bundle and retry"),
            Self::RepositoryCheckoutFailed => write!(formatter, "advisory audit checkout-materialization gate failed; obtain a complete bundle and retry"),
            Self::RepositoryCommitMismatch => write!(formatter, "advisory audit commit-binding gate failed; obtain a matching signed packet and bundle and retry"),
            Self::RepositoryShallow => write!(formatter, "advisory audit repository-depth gate failed; obtain a full repository bundle and retry"),
            Self::RepositoryDirty => write!(formatter, "advisory audit repository-cleanliness gate failed; obtain a clean advisory database bundle and retry"),
            Self::PolicyMaterializationFailed => write!(formatter, "advisory audit policy-materialization gate failed; restore the committed advisory policy and retry"),
            Self::CargoLockUnavailable => write!(formatter, "advisory audit lockfile gate failed; restore the tracked Cargo.lock and retry"),
            Self::CargoDenyInvocationFailed => write!(formatter, "advisory audit cargo-deny invocation failed; restore the pinned cargo-deny installation and retry"),
            Self::CargoDenyRejected => write!(formatter, "advisory audit advisory-policy gate failed; remediate the locked dependency graph and retry"),
            Self::ClockUnavailable => write!(formatter, "advisory audit clock gate failed; correct the host clock and retry"),
            Self::WitnessSerializationFailed => write!(formatter, "advisory audit witness-rendering gate failed; restore the xtask installation and retry"),
            Self::CleanupFailed => write!(formatter, "advisory audit cleanup gate failed; restore target-directory permissions and remove the failed run state"),
        }
    }
}

impl std::error::Error for AuditError {}

pub fn validate_advisory_audit_inputs(
    locator: &str,
    receipt_path: &Path,
    public_key_path: &Path,
    bundle_path: &Path,
) -> Result<(), AuditError> {
    validated_inputs(locator, receipt_path, public_key_path, bundle_path).map(|_| ())
}

pub fn resolve_advisory_audit_programs() -> Result<ResolvedAdvisoryAuditPrograms, AuditError> {
    Ok(ResolvedAdvisoryAuditPrograms {
        git: resolve_path_program("git").ok_or(AuditError::GitUnavailable)?,
        minisign: resolve_path_program("minisign").ok_or(AuditError::MinisignUnavailable)?,
        cargo_deny: resolve_path_program("cargo-deny").ok_or(AuditError::CargoDenyUnavailable)?,
    })
}

pub fn run_advisory_audit<R: CommandRunner + ?Sized, C: Clock + ?Sized>(
    request: &AdvisoryAuditRequest<'_>,
    runner: &R,
    clock: &C,
) -> Result<Vec<u8>, AuditError> {
    let (database_name, bundle_path) = validated_inputs(
        request.locator,
        request.receipt_path,
        request.public_key_path,
        request.bundle_path,
    )?;

    let target = request.checkout_root.join("target");
    fs::create_dir_all(&target).map_err(|_| AuditError::ScratchInvalid)?;
    let target = ContainedRoot::new(&target, "audit target", UnixModePolicy::AllowExecute)
        .map_err(|_| AuditError::ScratchInvalid)?;
    let temporary = Builder::new()
        .prefix(".advisory-audit-")
        .tempdir_in(target.canonical_path())
        .map_err(|_| AuditError::ScratchInvalid)?;

    let result = run_in_temporary(
        request,
        runner,
        clock,
        temporary.path(),
        &bundle_path,
        &database_name,
    );
    let cleanup = temporary.close();
    match (result, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(_)) => Err(AuditError::CleanupFailed),
        (Ok(witness), Ok(())) => Ok(witness),
    }
}

fn validated_inputs(
    locator: &str,
    receipt_path: &Path,
    public_key_path: &Path,
    bundle_path: &Path,
) -> Result<(String, PathBuf), AuditError> {
    validate_mirror_locator(locator).map_err(AuditError::Authority)?;
    let database_name = derive_database_name(locator)?;
    validate_freshness_receipt_path(receipt_path).map_err(AuditError::Authority)?;
    validate_mirror_public_key_path(public_key_path).map_err(AuditError::Authority)?;
    if !bundle_path.is_absolute()
        || artifact_fs::verify_regular_file(
            bundle_path,
            "advisory mirror bundle",
            UnixModePolicy::StrictNoExecute,
        )
        .is_err()
    {
        return Err(AuditError::BundleInputInvalid);
    }
    let bundle_path = fs::canonicalize(bundle_path).map_err(|_| AuditError::BundleInputInvalid)?;
    Ok((database_name, bundle_path))
}

fn run_in_temporary<R: CommandRunner + ?Sized, C: Clock + ?Sized>(
    request: &AdvisoryAuditRequest<'_>,
    runner: &R,
    clock: &C,
    run_root: &Path,
    bundle_path: &Path,
    database_name: &str,
) -> Result<Vec<u8>, AuditError> {
    let database_root = run_root.join("database");
    let config_root = run_root.join("config");
    let minisign_root = run_root.join("minisign");
    let verify_root = run_root.join("bundle-verify");
    let template_root = run_root.join("git-template");
    for directory in [
        &database_root,
        &config_root,
        &minisign_root,
        &verify_root,
        &template_root,
    ] {
        fs::create_dir(directory).map_err(|_| AuditError::ScratchInvalid)?;
    }
    let gitconfig = run_root.join("gitconfig");
    write_new_file(&gitconfig, b"").map_err(|_| AuditError::ScratchInvalid)?;
    let git_env = git_environment(&gitconfig, &template_root)?;

    let minisign_version = runner
        .run(request.programs.minisign, &["-v".to_owned()], None, None)
        .map_err(|_| AuditError::MinisignVersionUnsupported)?;
    if !minisign_version_is_supported(&minisign_version) {
        return Err(AuditError::MinisignVersionUnsupported);
    }
    let cargo_deny_version = runner
        .run(
            request.programs.cargo_deny,
            &["--version".to_owned()],
            None,
            None,
        )
        .map_err(|_| AuditError::CargoDenyVersionUnsupported)?;
    if cargo_deny_version.status != 0
        || String::from_utf8_lossy(&cargo_deny_version.stdout).trim()
            != format!("cargo-deny {CARGO_DENY_VERSION}")
    {
        return Err(AuditError::CargoDenyVersionUnsupported);
    }

    let mirror = MirrorPacketInputs {
        locator: request.locator,
        receipt_path: request.receipt_path,
        public_key_path: request.public_key_path,
        minisign_program: request.programs.minisign,
        expected_public_key_sha256: request.trust.public_key_sha256,
        expected_public_key_id: Some(request.trust.public_key_id),
    };
    let scratch = AdvisoryVerificationScratch {
        containment_root: run_root,
        parent_relative: "minisign",
        root_label: "advisory audit run",
        parent_label: "advisory audit minisign scratch parent",
    };
    let freshness = verify_mirror_freshness(&database_root, &scratch, &mirror, runner, clock)
        .map_err(AuditError::Authority)?;

    git_success(
        runner,
        request.programs.git,
        vec![
            "-C".to_owned(),
            path_text(&verify_root)?,
            "init".to_owned(),
            "--initial-branch=main".to_owned(),
        ],
        &git_env,
        AuditError::BundleVerificationFailed,
    )?;
    git_success(
        runner,
        request.programs.git,
        vec![
            "-C".to_owned(),
            path_text(&verify_root)?,
            "bundle".to_owned(),
            "verify".to_owned(),
            path_text(bundle_path)?,
        ],
        &git_env,
        AuditError::BundleVerificationFailed,
    )?;
    let heads = git_output(
        runner,
        request.programs.git,
        vec![
            "-C".to_owned(),
            path_text(&verify_root)?,
            "bundle".to_owned(),
            "list-heads".to_owned(),
            path_text(bundle_path)?,
        ],
        &git_env,
        AuditError::BundleHeadsInspectionFailed,
    )?;
    verify_bundle_heads(&heads.stdout, &freshness.synced_commit)?;

    let database_checkout = database_root.join(database_name);
    git_success(
        runner,
        request.programs.git,
        vec![
            "clone".to_owned(),
            "--no-checkout".to_owned(),
            "--no-tags".to_owned(),
            path_text(bundle_path)?,
            path_text(&database_checkout)?,
        ],
        &git_env,
        AuditError::DatabaseMaterializationFailed,
    )?;
    git_success(
        runner,
        request.programs.git,
        vec![
            "-C".to_owned(),
            path_text(&database_checkout)?,
            "checkout".to_owned(),
            "--detach".to_owned(),
            "--force".to_owned(),
            freshness.synced_commit.clone(),
        ],
        &git_env,
        AuditError::RepositoryCheckoutFailed,
    )?;
    let head = git_output(
        runner,
        request.programs.git,
        vec![
            "-C".to_owned(),
            path_text(&database_checkout)?,
            "rev-parse".to_owned(),
            "--verify".to_owned(),
            "HEAD^{commit}".to_owned(),
        ],
        &git_env,
        AuditError::RepositoryCommitMismatch,
    )?;
    if strict_line(&head.stdout) != Some(freshness.synced_commit.as_str()) {
        return Err(AuditError::RepositoryCommitMismatch);
    }
    let shallow = git_output(
        runner,
        request.programs.git,
        vec![
            "-C".to_owned(),
            path_text(&database_checkout)?,
            "rev-parse".to_owned(),
            "--is-shallow-repository".to_owned(),
        ],
        &git_env,
        AuditError::RepositoryShallow,
    )?;
    if strict_line(&shallow.stdout) != Some("false") {
        return Err(AuditError::RepositoryShallow);
    }
    let status = git_output(
        runner,
        request.programs.git,
        vec![
            "-C".to_owned(),
            path_text(&database_checkout)?,
            "status".to_owned(),
            "--porcelain=v1".to_owned(),
            "--untracked-files=all".to_owned(),
        ],
        &git_env,
        AuditError::RepositoryDirty,
    )?;
    if !status.stdout.is_empty() {
        return Err(AuditError::RepositoryDirty);
    }

    let checkout = ContainedRoot::new(
        request.checkout_root,
        "advisory audit checkout",
        UnixModePolicy::AllowExecute,
    )
    .map_err(|_| AuditError::PolicyMaterializationFailed)?;
    let deny_toml = checkout
        .read("deny.toml", "deny.toml")
        .map_err(|_| AuditError::PolicyMaterializationFailed)?;
    let config_bytes = render_advisory_config(&deny_toml, &database_root, request.locator)
        .map_err(|_| AuditError::PolicyMaterializationFailed)?;
    let config_path = config_root.join("deny.toml");
    write_new_file(&config_path, &config_bytes)
        .map_err(|_| AuditError::PolicyMaterializationFailed)?;

    let cargo_lock = checkout
        .read("Cargo.lock", "Cargo.lock")
        .map_err(|_| AuditError::CargoLockUnavailable)?;
    let cargo_lock_sha256 = sha256_hex(&cargo_lock);
    let mut cargo_env = git_env;
    cargo_env.insert("CARGO_NET_OFFLINE".to_owned(), "true".to_owned());
    let cargo_deny = runner
        .run(
            request.programs.cargo_deny,
            &[
                "--manifest-path".to_owned(),
                path_text(&checkout.canonical_path().join("Cargo.toml"))?,
                "--locked".to_owned(),
                "--offline".to_owned(),
                "--config".to_owned(),
                path_text(&config_path)?,
                "check".to_owned(),
                "advisories".to_owned(),
            ],
            None,
            Some(&cargo_env),
        )
        .map_err(|_| AuditError::CargoDenyInvocationFailed)?;
    if cargo_deny.status != 0 {
        return Err(AuditError::CargoDenyRejected);
    }
    let checked_at = clock
        .now()
        .map_err(|_| AuditError::ClockUnavailable)?
        .as_str()
        .to_owned();
    let witness = AdvisoryAuditWitness {
        schema: ADVISORY_AUDIT_SCHEMA.to_owned(),
        product: PRODUCT.to_owned(),
        source_cohort: MIRROR_COHORT_ID.to_owned(),
        synced_commit: freshness.synced_commit,
        receipt_utc: freshness.utc.as_str().to_owned(),
        max_age: freshness.max_age,
        checked_at,
        cargo_lock_sha256,
        cargo_deny_version: CARGO_DENY_VERSION.to_owned(),
        verdict: "pass".to_owned(),
    };
    render_canonical_json(&witness).map_err(|_| AuditError::WitnessSerializationFailed)
}

fn derive_database_name(locator: &str) -> Result<String, AuditError> {
    let first = Url::parse(locator).map_err(|_| AuditError::LocatorNotUrl)?;
    if !matches!(first.scheme(), "https" | "ssh")
        || first.cannot_be_a_base()
        || first.domain().is_none()
    {
        return Err(AuditError::LocatorNotUrl);
    }
    let second =
        Url::parse(&first.as_str().to_lowercase()).map_err(|_| AuditError::LocatorNotUrl)?;
    let last = second
        .path_segments()
        .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
        .ok_or(AuditError::LocatorNotUrl)?;
    let hash = XxHash64::oneshot(CACHE_HASH_SEED, second.as_str().as_bytes());
    Ok(format!("{last}-{hash:016x}"))
}

fn git_environment(
    gitconfig: &Path,
    template_root: &Path,
) -> Result<BTreeMap<String, String>, AuditError> {
    Ok(BTreeMap::from([
        ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
        ("GIT_CONFIG_GLOBAL".to_owned(), path_text(gitconfig)?),
        ("GIT_CONFIG_COUNT".to_owned(), "0".to_owned()),
        ("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned()),
        ("GIT_ALLOW_PROTOCOL".to_owned(), "file".to_owned()),
        ("GIT_PROTOCOL_FROM_USER".to_owned(), "0".to_owned()),
        ("GIT_TEMPLATE_DIR".to_owned(), path_text(template_root)?),
        ("LC_ALL".to_owned(), "C".to_owned()),
    ]))
}

fn git_success<R: CommandRunner + ?Sized>(
    runner: &R,
    program: &Path,
    args: Vec<String>,
    env: &BTreeMap<String, String>,
    error: AuditError,
) -> Result<(), AuditError> {
    git_output(runner, program, args, env, error).map(|_| ())
}

fn git_output<R: CommandRunner + ?Sized>(
    runner: &R,
    program: &Path,
    args: Vec<String>,
    env: &BTreeMap<String, String>,
    error: AuditError,
) -> Result<crate::release_exec::CommandOutput, AuditError> {
    let output = runner
        .run(program, &args, None, Some(env))
        .map_err(|_| error.clone())?;
    if output.status != 0 {
        return Err(error);
    }
    Ok(output)
}

fn verify_bundle_heads(bytes: &[u8], commit: &str) -> Result<(), AuditError> {
    let text = std::str::from_utf8(bytes).map_err(|_| AuditError::BundleHeadsMismatch)?;
    let mut heads = BTreeSet::new();
    for line in text.strip_suffix('\n').unwrap_or(text).split('\n') {
        let Some((oid, name)) = line.split_once(' ') else {
            return Err(AuditError::BundleHeadsMismatch);
        };
        if oid != commit || name.contains(char::is_whitespace) || !heads.insert(name) {
            return Err(AuditError::BundleHeadsMismatch);
        }
    }
    if heads != BTreeSet::from(["HEAD", "refs/heads/main"]) {
        return Err(AuditError::BundleHeadsMismatch);
    }
    Ok(())
}

fn strict_line(bytes: &[u8]) -> Option<&str> {
    let text = std::str::from_utf8(bytes).ok()?;
    let line = text.strip_suffix('\n')?;
    if line.is_empty() || line.contains(['\n', '\r']) {
        return None;
    }
    Some(line)
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), ()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| ())?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| ())
}

fn path_text(path: &Path) -> Result<String, AuditError> {
    child_process_path_text(path).ok_or(AuditError::ScratchInvalid)
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
