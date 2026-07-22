// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Operator-driven publication of validated release transparency evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::de::DeserializeOwned;

use crate::release_clock::{Clock, UtcTimestamp};
use crate::release_exec::{CommandRunner, CommandRunnerError};
use crate::release_receipt::{
    render_windows_native_proof_receipt, WindowsNativeProofReceipt, WINDOWS_NATIVE_PROOF_FILENAME,
};
use crate::rust_release_manifest::{
    companion_basename, validate_release_dir_with_facts_detailed, CheckoutFacts, Manifest, PRODUCT,
};
use crate::transparency_format::{
    build_transparency_entry, build_transparency_pointer, canonicalize_transparency_json,
    format_entry_trusted_comment, format_latest_trusted_comment, render_transparency_entry,
    render_transparency_latest, require_entry_trusted_comment_matches_body,
    require_latest_trusted_comment_matches_body, transparency_sha256_hex,
    validate_transparency_entry_value, validate_transparency_latest_value, TransparencyHeadLogRow,
    TransparencyLatestV1, TransparencyLedgerEntryV1, TransparencyNamedDigest,
    TransparencyTipIdentity,
};
use crate::transparency_stage::{
    render_staging_manifest_v1, verify_staging_manifest_v1, StagingManifestV1,
};
use crate::transparency_transport::{
    ObservedHttpResponse, TransparencyCachePolicy, TransparencyFetchPolicy,
    TransparencyListDestination, TransparencyObjectDestination, TransparencyObjectTransport,
    TransparencyPlane,
};

pub const STEP_1_PREFLIGHT: &str = "transparency.step-1.preflight";
pub const STEP_2_FETCH_CHAIN: &str = "transparency.step-2.fetch-chain";
pub const STEP_3_BUILD_SIGN: &str = "transparency.step-3.build-sign";
pub const STEP_4_SNAPSHOT_STAGE: &str = "transparency.step-4.snapshot-stage";
pub const STEP_5_ARCHIVE: &str = "transparency.step-5.archive";
pub const STEP_6_IMMUTABLE_UPLOAD: &str = "transparency.step-6.immutable-upload";
pub const STEP_7_PUBLIC_VERIFY: &str = "transparency.step-7.public-verify";
pub const STEP_8_MUTABLE_COMMIT: &str = "transparency.step-8.mutable-commit";
pub const STEP_9_HEAD_LOG: &str = "transparency.step-9.head-log";
pub const STEP_10_SUMMARY: &str = "transparency.step-10.summary";

pub const TRANSPARENCY_ENV_NAMES: [&str; 9] = [
    "TRANSPARENCY_BASE_URL",
    "TRANSPARENCY_S3_ENDPOINT",
    "TRANSPARENCY_BUCKET",
    "TRANSPARENCY_S3_ACCESS_KEY_ID",
    "TRANSPARENCY_S3_SECRET_ACCESS_KEY",
    "TRANSPARENCY_MINISIGN_KEY",
    "TRANSPARENCY_MINISIGN_PUB",
    "TRANSPARENCY_ARCHIVE_CHANNEL",
    "TRANSPARENCY_GENESIS",
];

pub const DEFAULT_TRANSPARENCY_BASE_URL: &str = "https://transparency.solstone.app";
pub const TRANSPARENCY_STAGE_ROOT: &str = "target/release-transparency-stage";
pub const TRANSPARENCY_HEAD_LOG: &str = "transparency-head-log.jsonl";

pub struct TransparencyEnvironment {
    pub base_url: String,
    pub s3_endpoint: String,
    pub bucket: String,
    pub s3_access_key_id: String,
    pub s3_secret_access_key: String,
    pub minisign_secret_key: PathBuf,
    pub minisign_public_key: PathBuf,
    pub archive_channel: PathBuf,
    pub genesis: bool,
}

pub fn resolve_transparency_environment_with<F>(
    mut lookup: F,
) -> Result<TransparencyEnvironment, TransparencyPublishError>
where
    F: FnMut(&str) -> Option<String>,
{
    let required = |lookup: &mut F, name| {
        lookup(name)
            .filter(|value| !value.is_empty())
            .ok_or(TransparencyPublishError::EnvironmentMissing { name })
    };
    let base_url = lookup("TRANSPARENCY_BASE_URL")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_TRANSPARENCY_BASE_URL.to_owned());
    let s3_endpoint = required(&mut lookup, "TRANSPARENCY_S3_ENDPOINT")?;
    let bucket = required(&mut lookup, "TRANSPARENCY_BUCKET")?;
    let s3_access_key_id = required(&mut lookup, "TRANSPARENCY_S3_ACCESS_KEY_ID")?;
    let s3_secret_access_key = required(&mut lookup, "TRANSPARENCY_S3_SECRET_ACCESS_KEY")?;
    let minisign_secret_key = PathBuf::from(required(&mut lookup, "TRANSPARENCY_MINISIGN_KEY")?);
    let minisign_public_key = PathBuf::from(required(&mut lookup, "TRANSPARENCY_MINISIGN_PUB")?);
    let archive_channel = PathBuf::from(required(&mut lookup, "TRANSPARENCY_ARCHIVE_CHANNEL")?);
    let genesis = match lookup("TRANSPARENCY_GENESIS") {
        None => false,
        Some(value) if value == "1" => true,
        Some(_) => return Err(TransparencyPublishError::GenesisValueInvalid),
    };
    if !minisign_secret_key.is_absolute()
        || !minisign_public_key.is_absolute()
        || !archive_channel.is_absolute()
    {
        return Err(TransparencyPublishError::EnvironmentPathInvalid);
    }
    Ok(TransparencyEnvironment {
        base_url,
        s3_endpoint,
        bucket,
        s3_access_key_id,
        s3_secret_access_key,
        minisign_secret_key,
        minisign_public_key,
        archive_channel,
        genesis,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransparencyPublishError {
    EnvironmentMissing {
        name: &'static str,
    },
    GenesisValueInvalid,
    EnvironmentPathInvalid,
    ToolUnavailable {
        tool: &'static str,
    },
    CandidateInvalid,
    CandidateChanged,
    ProofMissing,
    ProofInvalid,
    ChainFetch {
        observed: String,
        expected: String,
    },
    ChainInvalid {
        observed: String,
        expected: String,
    },
    GenesisNotAuthorized,
    GenesisNotEmpty,
    Rollback {
        observed: u64,
        expected: u64,
    },
    VersionPoisoned {
        version: String,
        source_commit: String,
        seq: u64,
        sha256: String,
    },
    StageInvalid,
    StageConflict,
    SignatureFailed,
    ArchiveFailed {
        observed: String,
        expected: String,
    },
    ArchiveReceiptInvalid {
        observed: String,
        expected: String,
    },
    ImmutableWrite {
        observed: u16,
        expected: String,
    },
    ImmutableConflict,
    ImmutableVerification,
    AdoptedRemoteEntry,
    ConcurrentPublish,
    MutableWrite {
        observed: u16,
        expected: String,
    },
    MutableVerification,
    HeadLogInvalid,
    HeadLogFork,
    HeadLogWrite,
    Process,
}

impl fmt::Display for TransparencyPublishError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EnvironmentMissing { name } => write!(formatter, "terminal transparency configuration: observed {name} missing, expected all required publisher variables; restore the environment and retry"),
            Self::GenesisValueInvalid => formatter.write_str("terminal transparency configuration: observed invalid genesis selector, expected absent or exactly 1; correct the environment and retry"),
            Self::EnvironmentPathInvalid => formatter.write_str("terminal transparency configuration: observed a relative executable or key location, expected absolute operator-supplied locations; correct the environment and retry"),
            Self::ToolUnavailable { tool } => write!(formatter, "terminal transparency preflight: observed unsupported or missing {tool}, expected a pinned supported version; install the required tool and retry"),
            Self::CandidateInvalid => formatter.write_str("terminal transparency candidate: observed invalid retained release bytes, expected a complete validated candidate; rebuild the candidate and retry"),
            Self::CandidateChanged => formatter.write_str("terminal transparency candidate: observed snapshot bytes differ from preflight, expected an unchanged validated candidate; rebuild the candidate and retry"),
            Self::ProofMissing => formatter.write_str("terminal transparency proof: observed required native proof missing, expected proof for the signed candidate; prove the candidate and retry"),
            Self::ProofInvalid => formatter.write_str("terminal transparency proof: observed stale or invalid binding, expected product version source commit and manifest digest equality; prove the candidate and retry"),
            Self::ChainFetch { observed, expected } => write!(formatter, "retryable transparency chain fetch: observed {observed}, expected {expected}; retry publication after the surface recovers"),
            Self::ChainInvalid { observed, expected } => write!(formatter, "terminal transparency chain state: observed {observed}, expected {expected}; audit the locked entries before retrying"),
            Self::GenesisNotAuthorized => formatter.write_str("terminal transparency genesis: observed missing authorization, expected TRANSPARENCY_GENESIS=1 for an empty chain; approve genesis and retry"),
            Self::GenesisNotEmpty => formatter.write_str("terminal transparency genesis: observed an existing version object, expected an empty product prefix; audit the existing chain and retry"),
            Self::Rollback { observed, expected } => write!(formatter, "terminal transparency rollback: observed chain length {observed}, expected at least local head {expected}; audit the split view before retrying"),
            Self::VersionPoisoned { version, source_commit, seq, sha256 } => write!(formatter, "terminal transparency version: observed version {version} permanently recorded at source_commit={source_commit} seq={seq} sha256={sha256}, expected the current chain position; cut the next version"),
            Self::StageInvalid => formatter.write_str("terminal transparency staging: observed malformed or changed staged bytes, expected the persisted staging-manifest v1 record; discard staging only after confirming the remote version prefix is empty"),
            Self::StageConflict => formatter.write_str("terminal transparency staging: observed a conflicting local version stage, expected one byte-stable publication attempt; discard staging only after confirming no remote version object and no archive receipt exist"),
            Self::SignatureFailed => formatter.write_str("terminal transparency signature: observed signing or verification failure, expected a locally verified body and trusted comment; restore the signing tools and retry"),
            Self::ArchiveFailed { observed, expected } => write!(formatter, "retryable transparency archive: observed {observed}, expected {expected}; retry after restoring the archive channel"),
            Self::ArchiveReceiptInvalid { observed, expected } => write!(formatter, "retryable transparency archive receipt: observed {observed}, expected {expected}; retry after correcting the archive channel"),
            Self::ImmutableWrite { observed, expected } => write!(formatter, "retryable transparency immutable write: observed HTTP {observed}, expected {expected}; retry after querying the remote object"),
            Self::ImmutableConflict => formatter.write_str("terminal transparency immutable conflict: observed different or unverifiable remote bytes, expected the staged signed evidence; cut the next version"),
            Self::ImmutableVerification => formatter.write_str("retryable transparency immutable verification: observed a public digest mismatch, expected the staged digest; retry after the public surface converges"),
            Self::AdoptedRemoteEntry => formatter.write_str("retryable transparency adoption: observed a valid own entry created by a racing attempt, expected the next invocation to archive the adopted bytes first; retry publication"),
            Self::ConcurrentPublish => formatter.write_str("retryable transparency concurrency guard: observed the pointer tip changed, expected the preflight chain state; retry from the new tip"),
            Self::MutableWrite { observed, expected } => write!(formatter, "retryable transparency mutable write: observed HTTP {observed}, expected {expected}; retry publication from the current pointer"),
            Self::MutableVerification => formatter.write_str("retryable transparency mutable verification: observed remote bytes or signature differ, expected the staged mutable value; retry after the public surface converges"),
            Self::HeadLogInvalid => formatter.write_str("terminal transparency head log: observed malformed or rewritten rows, expected canonical append-only rows; restore the tracked log and audit before retrying"),
            Self::HeadLogFork => formatter.write_str("terminal transparency head log: observed a product sequence fork, expected one entry digest per sequence; audit the chain before retrying"),
            Self::HeadLogWrite => formatter.write_str("retryable transparency head log: observed append failure, expected a durable local witness row; restore repository permissions and retry"),
            Self::Process => formatter.write_str("retryable transparency process: observed an unavailable child result, expected a complete bounded invocation; restore the local tool and retry"),
        }
    }
}

impl std::error::Error for TransparencyPublishError {}

impl From<CommandRunnerError> for TransparencyPublishError {
    fn from(_: CommandRunnerError) -> Self {
        Self::Process
    }
}

pub struct TransparencyPublishRequest<'a> {
    pub checkout_root: &'a Path,
    pub release_dir: &'a Path,
    pub evidence_dir: &'a Path,
    pub checkout_facts: &'a CheckoutFacts,
    pub environment: &'a TransparencyEnvironment,
    pub minisign_program: &'a Path,
    pub curl_program: &'a Path,
    pub signing_passphrase: &'a [u8],
}

pub struct TransparencyResignRequest<'a> {
    pub checkout_root: &'a Path,
    pub environment: &'a TransparencyEnvironment,
    pub minisign_program: &'a Path,
    pub curl_program: &'a Path,
    pub signing_passphrase: &'a [u8],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransparencyPublication {
    pub product: String,
    pub version: String,
    pub seq: u64,
    pub entry_sha256: String,
    pub archive_sha256: String,
    pub already_published: bool,
    pub pointer_requires_resign: bool,
    pub elapsed_ms: u128,
}

#[derive(Clone, Debug)]
struct ChainEntry {
    model: TransparencyLedgerEntryV1,
    bytes: Vec<u8>,
    signature: Option<Vec<u8>>,
}

#[derive(Clone, Debug)]
struct ChainState {
    pointer: Option<TransparencyLatestV1>,
    pointer_bytes: Option<Vec<u8>>,
    pointer_etag: Option<String>,
    entries: Vec<ChainEntry>,
    genesis_recovery: bool,
}

#[derive(Clone, Copy)]
struct AccessContext<'a> {
    checkout_root: &'a Path,
    environment: &'a TransparencyEnvironment,
    minisign_program: &'a Path,
}

#[derive(Clone, Copy)]
struct SigningMaterial<'a> {
    minisign_program: &'a Path,
    secret_key: &'a Path,
    public_key: &'a Path,
    passphrase: &'a [u8],
}

#[derive(Clone, Debug)]
struct StagedPublication {
    root: PathBuf,
    archive: PathBuf,
    version_prefix: String,
    entry: TransparencyLedgerEntryV1,
    entry_bytes: Vec<u8>,
    entry_signature: Vec<u8>,
    pointer: TransparencyLatestV1,
    pointer_bytes: Vec<u8>,
    pointer_signature: Vec<u8>,
    ledger_bytes: Vec<u8>,
    immutable_names: Vec<String>,
    manifest: StagingManifestV1,
}

pub fn publish_transparency<T, R, C>(
    request: &TransparencyPublishRequest<'_>,
    transport: &T,
    runner: &R,
    clock: &C,
) -> Result<TransparencyPublication, TransparencyPublishError>
where
    T: TransparencyObjectTransport,
    R: CommandRunner + ?Sized,
    C: Clock + ?Sized,
{
    let started = Instant::now();
    runner.record_phase(STEP_1_PREFLIGHT)?;
    verify_tool_versions(request.minisign_program, request.curl_program, runner)?;
    fs::create_dir_all(request.checkout_root.join(TRANSPARENCY_STAGE_ROOT))
        .map_err(|_| TransparencyPublishError::StageInvalid)?;
    let (_, live_manifest) =
        validate_release_dir_with_facts_detailed(request.release_dir, request.checkout_facts)
            .map_err(|_| TransparencyPublishError::CandidateInvalid)?;

    runner.record_phase(STEP_2_FETCH_CHAIN)?;
    let access = AccessContext {
        checkout_root: request.checkout_root,
        environment: request.environment,
        minisign_program: request.minisign_program,
    };
    let local_stage = request
        .checkout_root
        .join(TRANSPARENCY_STAGE_ROOT)
        .join(PRODUCT)
        .join(&live_manifest.version);
    let mut chain = fetch_chain_state(
        &access,
        transport,
        runner,
        local_stage
            .is_dir()
            .then_some(live_manifest.version.as_str()),
    )?;
    check_head_log_floor(request.checkout_root, &chain)?;

    let version_probe = probe_version_entry(
        &live_manifest,
        request,
        transport,
        runner,
        chain.entries.last(),
    )?;
    if chain.genesis_recovery && version_probe.is_none() {
        return Err(TransparencyPublishError::GenesisNotEmpty);
    }
    if let Some(remote) = &version_probe {
        if chain
            .entries
            .last()
            .is_some_and(|tip| tip.bytes == remote.bytes)
        {
            runner.record_phase(STEP_9_HEAD_LOG)?;
            append_head_log(request.checkout_root, &remote.model, &remote.bytes)?;
            let archive_sha256 = existing_stage_receipt(request, &live_manifest)
                .map(|manifest| manifest.sha256)
                .unwrap_or_default();
            return Ok(TransparencyPublication {
                product: PRODUCT.to_owned(),
                version: remote.model.version.clone(),
                seq: remote.model.seq,
                entry_sha256: transparency_sha256_hex(&remote.bytes),
                archive_sha256,
                already_published: true,
                pointer_requires_resign: pointer_is_expired(chain.pointer.as_ref(), clock)?,
                elapsed_ms: started.elapsed().as_micros() / 1_000,
            });
        }
    }

    runner.record_phase(STEP_4_SNAPSHOT_STAGE)?;
    let mut staged = load_or_build_stage(
        request,
        runner,
        clock,
        &live_manifest,
        &chain,
        version_probe.as_ref(),
    )?;

    runner.record_phase(STEP_5_ARCHIVE)?;
    archive_stage(request, runner, &staged)?;

    runner.record_phase(STEP_6_IMMUTABLE_UPLOAD)?;
    for name in &staged.immutable_names {
        let key = format!("{}/{}", staged.version_prefix, name);
        let bytes = fs::read(staged.archive.join(&key))
            .map_err(|_| TransparencyPublishError::StageInvalid)?;
        let response = match transport.create_only_put(
            &object(TransparencyPlane::S3, &key),
            &bytes,
            TransparencyCachePolicy::Immutable,
        ) {
            Ok(response) => response,
            Err(_) => {
                let observed = transport
                    .get(
                        &object(TransparencyPlane::S3, &key),
                        TransparencyFetchPolicy::Bypass,
                    )
                    .map_err(|_| TransparencyPublishError::ImmutableWrite {
                        observed: 0,
                        expected: "a queryable byte-identical object after an ambiguous transfer"
                            .to_owned(),
                    })?;
                if observed.status == 200 && observed.body == bytes {
                    continue;
                }
                return Err(TransparencyPublishError::ImmutableWrite {
                    observed: observed.status,
                    expected: "a byte-identical object after an ambiguous transfer".to_owned(),
                });
            }
        };
        if response.status == 412 {
            let remote = fetch_required(
                transport,
                object(TransparencyPlane::S3, &key),
                TransparencyFetchPolicy::Bypass,
            )?;
            if remote.body != bytes {
                if name.starts_with("ledger-entry.json") {
                    adopt_racing_entry(request, transport, runner, &chain, &mut staged)?;
                    return Err(TransparencyPublishError::AdoptedRemoteEntry);
                }
                return Err(TransparencyPublishError::ImmutableConflict);
            }
        } else if !(200..300).contains(&response.status) {
            return Err(TransparencyPublishError::ImmutableWrite {
                observed: response.status,
                expected: "HTTP 200..299 or byte-identical HTTP 412".to_owned(),
            });
        }
    }

    runner.record_phase(STEP_7_PUBLIC_VERIFY)?;
    for name in &staged.immutable_names {
        let key = format!("{}/{}", staged.version_prefix, name);
        let local = fs::read(staged.archive.join(&key))
            .map_err(|_| TransparencyPublishError::StageInvalid)?;
        let remote = transport
            .get(
                &object(TransparencyPlane::Public, &key),
                TransparencyFetchPolicy::Bypass,
            )
            .map_err(|_| TransparencyPublishError::ImmutableVerification)?;
        if remote.status != 200
            || transparency_sha256_hex(&remote.body) != transparency_sha256_hex(&local)
        {
            return Err(TransparencyPublishError::ImmutableVerification);
        }
    }

    runner.record_phase(STEP_8_MUTABLE_COMMIT)?;
    require_pointer_unchanged(transport, &chain)?;
    put_mutable_and_verify(
        transport,
        &format!("releases/{PRODUCT}/ledger.jsonl"),
        &staged.ledger_bytes,
        None,
    )?;
    put_mutable_and_verify(
        transport,
        &format!("releases/{PRODUCT}/latest.json.minisig"),
        &staged.pointer_signature,
        None,
    )?;
    put_mutable_and_verify(
        transport,
        &format!("releases/{PRODUCT}/latest.json"),
        &staged.pointer_bytes,
        chain.pointer_etag.as_deref(),
    )?;
    let final_pointer = fetch_required(
        transport,
        object(
            TransparencyPlane::Public,
            &format!("releases/{PRODUCT}/latest.json"),
        ),
        TransparencyFetchPolicy::Bypass,
    )?;
    let final_signature = fetch_required(
        transport,
        object(
            TransparencyPlane::Public,
            &format!("releases/{PRODUCT}/latest.json.minisig"),
        ),
        TransparencyFetchPolicy::Bypass,
    )?;
    if final_pointer.body != staged.pointer_bytes
        || final_signature.body != staged.pointer_signature
    {
        return Err(TransparencyPublishError::MutableVerification);
    }
    verify_signature_bytes(
        runner,
        request.minisign_program,
        &request.environment.minisign_public_key,
        &final_pointer.body,
        &final_signature.body,
        &staged.root.join("verify-final-pointer"),
    )?;
    let comment = trusted_comment(&final_signature.body)?;
    require_latest_trusted_comment_matches_body(&staged.pointer, comment)
        .map_err(|_| TransparencyPublishError::MutableVerification)?;

    runner.record_phase(STEP_9_HEAD_LOG)?;
    append_head_log(request.checkout_root, &staged.entry, &staged.entry_bytes)?;
    chain.entries.push(ChainEntry {
        model: staged.entry.clone(),
        bytes: staged.entry_bytes.clone(),
        signature: Some(staged.entry_signature.clone()),
    });

    runner.record_phase(STEP_10_SUMMARY)?;
    Ok(TransparencyPublication {
        product: PRODUCT.to_owned(),
        version: staged.entry.version.clone(),
        seq: staged.entry.seq,
        entry_sha256: transparency_sha256_hex(&staged.entry_bytes),
        archive_sha256: staged.manifest.sha256,
        already_published: false,
        pointer_requires_resign: pointer_is_expired(Some(&staged.pointer), clock)?,
        elapsed_ms: started.elapsed().as_micros() / 1_000,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransparencyPointerResign {
    pub product: String,
    pub version: String,
    pub chain_length: u64,
    pub tip_sha256: String,
    pub valid_until: String,
}

pub fn resign_transparency_pointer<T, R, C>(
    request: &TransparencyResignRequest<'_>,
    transport: &T,
    runner: &R,
    clock: &C,
) -> Result<TransparencyPointerResign, TransparencyPublishError>
where
    T: TransparencyObjectTransport,
    R: CommandRunner + ?Sized,
    C: Clock + ?Sized,
{
    verify_tool_versions(request.minisign_program, request.curl_program, runner)?;
    fs::create_dir_all(request.checkout_root.join(TRANSPARENCY_STAGE_ROOT))
        .map_err(|_| TransparencyPublishError::StageInvalid)?;
    let access = AccessContext {
        checkout_root: request.checkout_root,
        environment: request.environment,
        minisign_program: request.minisign_program,
    };
    let chain = fetch_chain_state(&access, transport, runner, None)?;
    check_head_log_floor(request.checkout_root, &chain)?;
    let tip = chain
        .entries
        .last()
        .ok_or(TransparencyPublishError::GenesisNotAuthorized)?;
    let identity = chain_tip(tip);
    let signed_at = clock
        .now()
        .map_err(|_| TransparencyPublishError::StageInvalid)?;
    let pointer = build_transparency_pointer(&identity, &signed_at)
        .map_err(|_| TransparencyPublishError::StageInvalid)?;
    let body =
        render_transparency_latest(&pointer).map_err(|_| TransparencyPublishError::StageInvalid)?;
    let signature = sign_bytes(
        runner,
        &SigningMaterial {
            minisign_program: request.minisign_program,
            secret_key: &request.environment.minisign_secret_key,
            public_key: &request.environment.minisign_public_key,
            passphrase: request.signing_passphrase,
        },
        &body,
        &format_latest_trusted_comment(&pointer),
        &request
            .checkout_root
            .join(TRANSPARENCY_STAGE_ROOT)
            .join(".resign-pointer"),
    )?;
    require_pointer_unchanged(transport, &chain)?;
    put_mutable_and_verify(
        transport,
        &format!("releases/{PRODUCT}/latest.json.minisig"),
        &signature,
        None,
    )?;
    put_mutable_and_verify(
        transport,
        &format!("releases/{PRODUCT}/latest.json"),
        &body,
        chain.pointer_etag.as_deref(),
    )?;
    let final_body = fetch_required(
        transport,
        object(
            TransparencyPlane::Public,
            &format!("releases/{PRODUCT}/latest.json"),
        ),
        TransparencyFetchPolicy::Bypass,
    )?;
    let final_signature = fetch_required(
        transport,
        object(
            TransparencyPlane::Public,
            &format!("releases/{PRODUCT}/latest.json.minisig"),
        ),
        TransparencyFetchPolicy::Bypass,
    )?;
    if final_body.body != body || final_signature.body != signature {
        return Err(TransparencyPublishError::MutableVerification);
    }
    verify_signature_bytes(
        runner,
        request.minisign_program,
        &request.environment.minisign_public_key,
        &final_body.body,
        &final_signature.body,
        &request
            .checkout_root
            .join(TRANSPARENCY_STAGE_ROOT)
            .join(".resign-final-verify"),
    )?;
    require_latest_trusted_comment_matches_body(&pointer, trusted_comment(&final_signature.body)?)
        .map_err(|_| TransparencyPublishError::MutableVerification)?;
    Ok(TransparencyPointerResign {
        product: PRODUCT.to_owned(),
        version: pointer.version,
        chain_length: pointer.chain_length,
        tip_sha256: pointer.tip_sha256,
        valid_until: pointer.valid_until,
    })
}

fn verify_tool_versions<R: CommandRunner + ?Sized>(
    minisign_program: &Path,
    curl_program: &Path,
    runner: &R,
) -> Result<(), TransparencyPublishError> {
    let minisign = runner.run(minisign_program, &["-v".to_owned()], None, None)?;
    let minisign_text = String::from_utf8_lossy(&minisign.stdout);
    let minisign_error = String::from_utf8_lossy(&minisign.stderr);
    let observed_version = if minisign_text.trim().is_empty() {
        minisign_error.trim()
    } else {
        minisign_text.trim()
    };
    if minisign.status != 0 || !matches!(observed_version, "minisign 0.11" | "minisign 0.12") {
        return Err(TransparencyPublishError::ToolUnavailable { tool: "minisign" });
    }
    let curl = runner.run(curl_program, &["--version".to_owned()], None, None)?;
    let curl_text = String::from_utf8_lossy(&curl.stdout);
    if curl.status != 0 || !curl_supports_sigv4(&curl_text) {
        return Err(TransparencyPublishError::ToolUnavailable { tool: "curl" });
    }
    Ok(())
}

fn curl_supports_sigv4(version_output: &str) -> bool {
    let Some(version) = version_output
        .lines()
        .next()
        .and_then(|line| line.split_ascii_whitespace().nth(1))
    else {
        return false;
    };
    let mut components = version.split('.');
    let major = components
        .next()
        .and_then(|value| value.parse::<u64>().ok());
    let minor = components
        .next()
        .and_then(|value| value.parse::<u64>().ok());
    matches!((major, minor), (Some(major), Some(minor)) if major > 7 || (major == 7 && minor >= 75))
}

fn load_or_build_stage<R: CommandRunner + ?Sized, C: Clock + ?Sized>(
    request: &TransparencyPublishRequest<'_>,
    runner: &R,
    clock: &C,
    live_manifest: &Manifest,
    chain: &ChainState,
    adopted: Option<&ChainEntry>,
) -> Result<StagedPublication, TransparencyPublishError> {
    let stage_root = request
        .checkout_root
        .join(TRANSPARENCY_STAGE_ROOT)
        .join(PRODUCT)
        .join(&live_manifest.version);
    if stage_root.exists() {
        return load_stage(request, runner, live_manifest, chain, &stage_root);
    }
    build_stage(
        request,
        runner,
        clock,
        live_manifest,
        chain,
        adopted,
        &stage_root,
    )
}

fn build_stage<R: CommandRunner + ?Sized, C: Clock + ?Sized>(
    request: &TransparencyPublishRequest<'_>,
    runner: &R,
    clock: &C,
    live_manifest: &Manifest,
    chain: &ChainState,
    adopted: Option<&ChainEntry>,
    final_root: &Path,
) -> Result<StagedPublication, TransparencyPublishError> {
    runner.record_phase(STEP_3_BUILD_SIGN)?;
    let parent = final_root
        .parent()
        .ok_or(TransparencyPublishError::StageInvalid)?;
    fs::create_dir_all(parent).map_err(|_| TransparencyPublishError::StageInvalid)?;
    let work_root = parent.join(format!(
        ".{}.stage-{}",
        live_manifest.version,
        std::process::id()
    ));
    fs::create_dir(&work_root).map_err(|_| TransparencyPublishError::StageConflict)?;
    let result = build_stage_in(
        request,
        runner,
        clock,
        live_manifest,
        chain,
        adopted,
        &work_root,
    );
    let staged = match result {
        Ok(staged) => staged,
        Err(error) => {
            let _ = fs::remove_dir_all(&work_root);
            return Err(error);
        }
    };
    fs::rename(&work_root, final_root).map_err(|_| TransparencyPublishError::StageConflict)?;
    load_stage(request, runner, live_manifest, chain, final_root).map(|loaded| StagedPublication {
        manifest: staged.manifest,
        ..loaded
    })
}

fn build_stage_in<R: CommandRunner + ?Sized, C: Clock + ?Sized>(
    request: &TransparencyPublishRequest<'_>,
    runner: &R,
    clock: &C,
    live_manifest: &Manifest,
    chain: &ChainState,
    adopted: Option<&ChainEntry>,
    work_root: &Path,
) -> Result<StagedPublication, TransparencyPublishError> {
    let snapshot = work_root.join("candidate-snapshot");
    snapshot_candidate(request.release_dir, &snapshot, live_manifest)?;
    let (_, snapshot_manifest) =
        validate_release_dir_with_facts_detailed(&snapshot, request.checkout_facts)
            .map_err(|_| TransparencyPublishError::CandidateInvalid)?;
    if &snapshot_manifest != live_manifest {
        return Err(TransparencyPublishError::CandidateChanged);
    }
    let companion_name = companion_basename();
    let companion_bytes = fs::read(snapshot.join(&companion_name))
        .map_err(|_| TransparencyPublishError::CandidateInvalid)?;
    let companion = TransparencyNamedDigest {
        name: companion_name.clone(),
        sha256: transparency_sha256_hex(&companion_bytes),
    };
    let proof = snapshot_proof(request, &snapshot_manifest, &companion, work_root)?;
    let proofs = proof
        .as_ref()
        .map(|(_, digest)| vec![digest.clone()])
        .unwrap_or_default();

    let published = clock
        .now()
        .map_err(|_| TransparencyPublishError::StageInvalid)?;
    let previous = chain.entries.last().map(chain_tip);
    let (entry, entry_bytes, entry_signature) = if let Some(adopted) = adopted {
        let signature = adopted
            .signature
            .clone()
            .ok_or(TransparencyPublishError::ImmutableConflict)?;
        (adopted.model.clone(), adopted.bytes.clone(), signature)
    } else {
        let entry = build_transparency_entry(
            &snapshot_manifest,
            &companion,
            &proofs,
            previous.as_ref(),
            &published,
        )
        .map_err(|_| TransparencyPublishError::CandidateInvalid)?;
        let bytes = render_transparency_entry(&entry)
            .map_err(|_| TransparencyPublishError::CandidateInvalid)?;
        let comment = format_entry_trusted_comment(&entry, &bytes);
        let signature = sign_bytes(
            runner,
            &publish_signing_material(request),
            &bytes,
            &comment,
            &work_root.join("entry-signing"),
        )?;
        (entry, bytes, signature)
    };
    assert_entry_matches_candidate(&entry, &snapshot_manifest, &companion, &proofs)?;

    let tip = chain_tip(&ChainEntry {
        model: entry.clone(),
        bytes: entry_bytes.clone(),
        signature: Some(entry_signature.clone()),
    });
    let pointer = build_transparency_pointer(&tip, &published)
        .map_err(|_| TransparencyPublishError::StageInvalid)?;
    let pointer_bytes =
        render_transparency_latest(&pointer).map_err(|_| TransparencyPublishError::StageInvalid)?;
    let pointer_comment = format_latest_trusted_comment(&pointer);
    let pointer_signature = sign_bytes(
        runner,
        &publish_signing_material(request),
        &pointer_bytes,
        &pointer_comment,
        &work_root.join("pointer-signing"),
    )?;

    let mut ledger_bytes = Vec::new();
    for existing in &chain.entries {
        ledger_bytes.extend_from_slice(&existing.bytes);
    }
    ledger_bytes.extend_from_slice(&entry_bytes);

    let archive = work_root.join("archive");
    let version_prefix = format!("releases/{PRODUCT}/v/{}", entry.version);
    let version_dir = archive.join(&version_prefix);
    let product_dir = archive.join(format!("releases/{PRODUCT}"));
    fs::create_dir_all(&version_dir).map_err(|_| TransparencyPublishError::StageInvalid)?;
    copy_regular(
        &snapshot.join(&companion_name),
        &version_dir.join(&companion_name),
    )?;
    for artifact in &snapshot_manifest.artifacts {
        copy_regular(
            &snapshot.join(&artifact.path),
            &version_dir.join(&artifact.path),
        )?;
    }
    if let Some((proof_path, digest)) = proof {
        copy_regular(&proof_path, &version_dir.join(&digest.name))?;
        fs::remove_file(proof_path).map_err(|_| TransparencyPublishError::StageInvalid)?;
    }
    fs::write(version_dir.join("ledger-entry.json"), &entry_bytes)
        .map_err(|_| TransparencyPublishError::StageInvalid)?;
    fs::write(
        version_dir.join("ledger-entry.json.minisig"),
        &entry_signature,
    )
    .map_err(|_| TransparencyPublishError::StageInvalid)?;
    fs::write(product_dir.join("ledger.jsonl"), &ledger_bytes)
        .map_err(|_| TransparencyPublishError::StageInvalid)?;
    fs::write(product_dir.join("latest.json"), &pointer_bytes)
        .map_err(|_| TransparencyPublishError::StageInvalid)?;
    fs::write(product_dir.join("latest.json.minisig"), &pointer_signature)
        .map_err(|_| TransparencyPublishError::StageInvalid)?;
    fs::remove_dir_all(&snapshot).map_err(|_| TransparencyPublishError::StageInvalid)?;

    let manifest =
        render_staging_manifest_v1(&archive).map_err(|_| TransparencyPublishError::StageInvalid)?;
    fs::write(work_root.join("stage-manifest.v1"), &manifest.bytes)
        .map_err(|_| TransparencyPublishError::StageInvalid)?;
    let immutable_names = immutable_names(&snapshot_manifest, !proofs.is_empty())?;
    Ok(StagedPublication {
        root: work_root.to_path_buf(),
        archive,
        version_prefix,
        entry,
        entry_bytes,
        entry_signature,
        pointer,
        pointer_bytes,
        pointer_signature,
        ledger_bytes,
        immutable_names,
        manifest,
    })
}

fn load_stage<R: CommandRunner + ?Sized>(
    request: &TransparencyPublishRequest<'_>,
    runner: &R,
    live_manifest: &Manifest,
    chain: &ChainState,
    root: &Path,
) -> Result<StagedPublication, TransparencyPublishError> {
    let archive = root.join("archive");
    let retry_record = fs::read(root.join("stage-manifest.v1"))
        .map_err(|_| TransparencyPublishError::StageInvalid)?;
    let manifest = verify_staging_manifest_v1(&archive, &retry_record)
        .map_err(|_| TransparencyPublishError::StageInvalid)?;
    let version_prefix = format!("releases/{PRODUCT}/v/{}", live_manifest.version);
    let version_dir = archive.join(&version_prefix);
    let companion_name = companion_basename();

    let validation = root.join(format!(".retry-validate-{}", std::process::id()));
    fs::create_dir(&validation).map_err(|_| TransparencyPublishError::StageConflict)?;
    let validation_result = (|| {
        copy_regular(
            &version_dir.join(&companion_name),
            &validation.join(&companion_name),
        )?;
        for artifact in &live_manifest.artifacts {
            copy_regular(
                &version_dir.join(&artifact.path),
                &validation.join(&artifact.path),
            )?;
        }
        let (_, staged_manifest) =
            validate_release_dir_with_facts_detailed(&validation, request.checkout_facts)
                .map_err(|_| TransparencyPublishError::StageInvalid)?;
        if &staged_manifest != live_manifest {
            return Err(TransparencyPublishError::CandidateChanged);
        }
        Ok(())
    })();
    let _ = fs::remove_dir_all(&validation);
    validation_result?;

    let entry_bytes = fs::read(version_dir.join("ledger-entry.json"))
        .map_err(|_| TransparencyPublishError::StageInvalid)?;
    let entry = parse_entry(&entry_bytes)?;
    let entry_signature = fs::read(version_dir.join("ledger-entry.json.minisig"))
        .map_err(|_| TransparencyPublishError::StageInvalid)?;
    verify_signature_bytes(
        runner,
        request.minisign_program,
        &request.environment.minisign_public_key,
        &entry_bytes,
        &entry_signature,
        &root.join("retry-entry-verify"),
    )?;
    require_entry_trusted_comment_matches_body(
        &entry,
        &entry_bytes,
        trusted_comment(&entry_signature)?,
    )
    .map_err(|_| TransparencyPublishError::StageInvalid)?;

    let companion_bytes = fs::read(version_dir.join(&companion_name))
        .map_err(|_| TransparencyPublishError::StageInvalid)?;
    let companion = TransparencyNamedDigest {
        name: companion_name,
        sha256: transparency_sha256_hex(&companion_bytes),
    };
    let proofs = staged_proofs(&version_dir, live_manifest, &companion)?;
    assert_entry_matches_candidate(&entry, live_manifest, &companion, &proofs)?;
    require_entry_fits_chain(&entry, &entry_bytes, chain)?;

    let product_dir = archive.join(format!("releases/{PRODUCT}"));
    let pointer_bytes = fs::read(product_dir.join("latest.json"))
        .map_err(|_| TransparencyPublishError::StageInvalid)?;
    let pointer = parse_pointer(&pointer_bytes)?;
    let pointer_signature = fs::read(product_dir.join("latest.json.minisig"))
        .map_err(|_| TransparencyPublishError::StageInvalid)?;
    verify_signature_bytes(
        runner,
        request.minisign_program,
        &request.environment.minisign_public_key,
        &pointer_bytes,
        &pointer_signature,
        &root.join("retry-pointer-verify"),
    )?;
    require_latest_trusted_comment_matches_body(&pointer, trusted_comment(&pointer_signature)?)
        .map_err(|_| TransparencyPublishError::StageInvalid)?;
    if pointer.chain_length != entry.seq
        || pointer.tip_sha256 != transparency_sha256_hex(&entry_bytes)
        || pointer.version != entry.version
    {
        return Err(TransparencyPublishError::StageInvalid);
    }
    let ledger_bytes = fs::read(product_dir.join("ledger.jsonl"))
        .map_err(|_| TransparencyPublishError::StageInvalid)?;
    let ledger = validate_ledger_bytes(&ledger_bytes)?;
    if ledger.last().is_none_or(|tip| tip.bytes != entry_bytes) {
        return Err(TransparencyPublishError::StageInvalid);
    }
    let immutable_names = immutable_names(live_manifest, !proofs.is_empty())?;
    Ok(StagedPublication {
        root: root.to_path_buf(),
        archive,
        version_prefix,
        entry,
        entry_bytes,
        entry_signature,
        pointer,
        pointer_bytes,
        pointer_signature,
        ledger_bytes,
        immutable_names,
        manifest,
    })
}

fn snapshot_candidate(
    release_dir: &Path,
    snapshot: &Path,
    manifest: &Manifest,
) -> Result<(), TransparencyPublishError> {
    fs::create_dir(snapshot).map_err(|_| TransparencyPublishError::StageInvalid)?;
    let companion = companion_basename();
    copy_regular(&release_dir.join(&companion), &snapshot.join(&companion))?;
    for artifact in &manifest.artifacts {
        copy_regular(
            &release_dir.join(&artifact.path),
            &snapshot.join(&artifact.path),
        )?;
    }
    Ok(())
}

fn snapshot_proof(
    request: &TransparencyPublishRequest<'_>,
    manifest: &Manifest,
    companion: &TransparencyNamedDigest,
    work_root: &Path,
) -> Result<Option<(PathBuf, TransparencyNamedDigest)>, TransparencyPublishError> {
    let required = manifest
        .native_tools
        .get("signing_mode")
        .is_none_or(|mode| mode != "unsigned");
    let source = request.evidence_dir.join(WINDOWS_NATIVE_PROOF_FILENAME);
    if !source.exists() {
        return if required {
            Err(TransparencyPublishError::ProofMissing)
        } else {
            Ok(None)
        };
    }
    let target = work_root.join("proof-snapshot.json");
    copy_regular(&source, &target)?;
    let bytes = fs::read(&target).map_err(|_| TransparencyPublishError::ProofInvalid)?;
    let receipt: WindowsNativeProofReceipt =
        serde_json::from_slice(&bytes).map_err(|_| TransparencyPublishError::ProofInvalid)?;
    let canonical = render_windows_native_proof_receipt(&receipt)
        .map_err(|_| TransparencyPublishError::ProofInvalid)?;
    if canonical != bytes
        || receipt.product != PRODUCT
        || receipt.version != manifest.version
        || receipt.source_commit != manifest.source_commit
        || receipt.companion_manifest.filename != companion.name
        || receipt.companion_manifest.sha256 != companion.sha256
    {
        return Err(TransparencyPublishError::ProofInvalid);
    }
    Ok(Some((
        target,
        TransparencyNamedDigest {
            name: WINDOWS_NATIVE_PROOF_FILENAME.to_owned(),
            sha256: transparency_sha256_hex(&bytes),
        },
    )))
}

fn staged_proofs(
    version_dir: &Path,
    manifest: &Manifest,
    companion: &TransparencyNamedDigest,
) -> Result<Vec<TransparencyNamedDigest>, TransparencyPublishError> {
    let path = version_dir.join(WINDOWS_NATIVE_PROOF_FILENAME);
    let required = manifest
        .native_tools
        .get("signing_mode")
        .is_none_or(|mode| mode != "unsigned");
    if !path.exists() {
        return if required {
            Err(TransparencyPublishError::ProofMissing)
        } else {
            Ok(Vec::new())
        };
    }
    let bytes = fs::read(&path).map_err(|_| TransparencyPublishError::ProofInvalid)?;
    let receipt: WindowsNativeProofReceipt =
        serde_json::from_slice(&bytes).map_err(|_| TransparencyPublishError::ProofInvalid)?;
    if render_windows_native_proof_receipt(&receipt)
        .map_err(|_| TransparencyPublishError::ProofInvalid)?
        != bytes
        || receipt.product != PRODUCT
        || receipt.version != manifest.version
        || receipt.source_commit != manifest.source_commit
        || receipt.companion_manifest.filename != companion.name
        || receipt.companion_manifest.sha256 != companion.sha256
    {
        return Err(TransparencyPublishError::ProofInvalid);
    }
    Ok(vec![TransparencyNamedDigest {
        name: WINDOWS_NATIVE_PROOF_FILENAME.to_owned(),
        sha256: transparency_sha256_hex(&bytes),
    }])
}

fn assert_entry_matches_candidate(
    entry: &TransparencyLedgerEntryV1,
    manifest: &Manifest,
    companion: &TransparencyNamedDigest,
    proofs: &[TransparencyNamedDigest],
) -> Result<(), TransparencyPublishError> {
    let expected_artifacts: BTreeSet<_> = manifest
        .artifacts
        .iter()
        .map(|artifact| {
            (
                artifact.path.as_str(),
                artifact.sha256.as_str(),
                artifact.bytes,
            )
        })
        .collect();
    let observed_artifacts: BTreeSet<_> = entry
        .artifacts
        .iter()
        .map(|artifact| {
            (
                artifact.name.as_str(),
                artifact.sha256.as_str(),
                artifact.bytes,
            )
        })
        .collect();
    let expected_proofs: BTreeSet<_> = proofs
        .iter()
        .map(|proof| (proof.name.as_str(), proof.sha256.as_str()))
        .collect();
    let observed_proofs: BTreeSet<_> = entry
        .proofs
        .iter()
        .map(|proof| (proof.name.as_str(), proof.sha256.as_str()))
        .collect();
    if entry.product != PRODUCT
        || entry.version != manifest.version
        || entry.source_commit != manifest.source_commit
        || entry.manifests != [companion.clone()]
        || observed_artifacts != expected_artifacts
        || observed_proofs != expected_proofs
    {
        return Err(TransparencyPublishError::ImmutableConflict);
    }
    Ok(())
}

fn immutable_names(
    manifest: &Manifest,
    has_proof: bool,
) -> Result<Vec<String>, TransparencyPublishError> {
    let mut names = vec![
        "ledger-entry.json".to_owned(),
        "ledger-entry.json.minisig".to_owned(),
        companion_basename(),
    ];
    if has_proof {
        names.push(WINDOWS_NATIVE_PROOF_FILENAME.to_owned());
    }
    let artifacts: BTreeSet<_> = manifest
        .artifacts
        .iter()
        .map(|artifact| artifact.path.as_str())
        .collect();
    if names.iter().any(|name| artifacts.contains(name.as_str())) {
        return Err(TransparencyPublishError::StageInvalid);
    }
    Ok(names)
}

fn copy_regular(source: &Path, target: &Path) -> Result<(), TransparencyPublishError> {
    let metadata =
        fs::symlink_metadata(source).map_err(|_| TransparencyPublishError::CandidateInvalid)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(TransparencyPublishError::CandidateInvalid);
    }
    let bytes = fs::read(source).map_err(|_| TransparencyPublishError::CandidateInvalid)?;
    fs::write(target, bytes).map_err(|_| TransparencyPublishError::StageInvalid)
}

fn sign_bytes<R: CommandRunner + ?Sized>(
    runner: &R,
    signing: &SigningMaterial<'_>,
    body: &[u8],
    comment: &str,
    scratch: &Path,
) -> Result<Vec<u8>, TransparencyPublishError> {
    fs::create_dir(scratch).map_err(|_| TransparencyPublishError::SignatureFailed)?;
    let body_path = scratch.join("body");
    let signature_path = scratch.join("body.minisig");
    fs::write(&body_path, body).map_err(|_| TransparencyPublishError::SignatureFailed)?;
    let mut stdin = signing.passphrase.to_vec();
    if !stdin.ends_with(b"\n") {
        stdin.push(b'\n');
    }
    let output = runner.run(
        signing.minisign_program,
        &[
            "-S".to_owned(),
            "-s".to_owned(),
            path_text(signing.secret_key)?,
            "-m".to_owned(),
            path_text(&body_path)?,
            "-x".to_owned(),
            path_text(&signature_path)?,
            "-t".to_owned(),
            comment.to_owned(),
        ],
        Some(&stdin),
        None,
    )?;
    if output.status != 0 {
        return Err(TransparencyPublishError::SignatureFailed);
    }
    let signature =
        fs::read(&signature_path).map_err(|_| TransparencyPublishError::SignatureFailed)?;
    verify_signature_bytes(
        runner,
        signing.minisign_program,
        signing.public_key,
        body,
        &signature,
        &scratch.join("verify"),
    )?;
    let _ = fs::remove_dir_all(scratch);
    Ok(signature)
}

fn verify_signature_bytes<R: CommandRunner + ?Sized>(
    runner: &R,
    minisign_program: &Path,
    public_key: &Path,
    body: &[u8],
    signature: &[u8],
    scratch: &Path,
) -> Result<(), TransparencyPublishError> {
    fs::create_dir(scratch).map_err(|_| TransparencyPublishError::SignatureFailed)?;
    let body_path = scratch.join("body");
    let signature_path = scratch.join("body.minisig");
    fs::write(&body_path, body).map_err(|_| TransparencyPublishError::SignatureFailed)?;
    fs::write(&signature_path, signature).map_err(|_| TransparencyPublishError::SignatureFailed)?;
    let output = runner.run(
        minisign_program,
        &[
            "-V".to_owned(),
            "-p".to_owned(),
            path_text(public_key)?,
            "-m".to_owned(),
            path_text(&body_path)?,
            "-x".to_owned(),
            path_text(&signature_path)?,
        ],
        None,
        None,
    )?;
    let _ = fs::remove_dir_all(scratch);
    if output.status == 0 {
        Ok(())
    } else {
        Err(TransparencyPublishError::SignatureFailed)
    }
}

fn trusted_comment(signature: &[u8]) -> Result<&str, TransparencyPublishError> {
    let text =
        std::str::from_utf8(signature).map_err(|_| TransparencyPublishError::SignatureFailed)?;
    text.lines()
        .find_map(|line| line.strip_prefix("trusted comment: "))
        .ok_or(TransparencyPublishError::SignatureFailed)
}

fn path_text(path: &Path) -> Result<String, TransparencyPublishError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or(TransparencyPublishError::Process)
}

fn publish_signing_material<'a>(
    request: &'a TransparencyPublishRequest<'a>,
) -> SigningMaterial<'a> {
    SigningMaterial {
        minisign_program: request.minisign_program,
        secret_key: &request.environment.minisign_secret_key,
        public_key: &request.environment.minisign_public_key,
        passphrase: request.signing_passphrase,
    }
}

fn fetch_chain_state<T: TransparencyObjectTransport, R: CommandRunner + ?Sized>(
    access: &AccessContext<'_>,
    transport: &T,
    runner: &R,
    genesis_recovery_version: Option<&str>,
) -> Result<ChainState, TransparencyPublishError> {
    let pointer_key = format!("releases/{PRODUCT}/latest.json");
    let signature_key = format!("releases/{PRODUCT}/latest.json.minisig");
    let pointer_response = transport
        .get(
            &object(TransparencyPlane::S3, &pointer_key),
            TransparencyFetchPolicy::Bypass,
        )
        .map_err(|_| TransparencyPublishError::ChainFetch {
            observed: "pointer transport failure".to_owned(),
            expected: "HTTP 200 or empty-chain HTTP 404".to_owned(),
        })?;
    let signature_response = transport
        .get(
            &object(TransparencyPlane::S3, &signature_key),
            TransparencyFetchPolicy::Bypass,
        )
        .map_err(|_| TransparencyPublishError::ChainFetch {
            observed: "pointer signature transport failure".to_owned(),
            expected: "HTTP 200 or empty-chain HTTP 404".to_owned(),
        })?;
    if pointer_response.status == 404 && signature_response.status == 404 {
        if !access.environment.genesis {
            return Err(TransparencyPublishError::GenesisNotAuthorized);
        }
        let listing = transport
            .list(&TransparencyListDestination {
                prefix: format!("releases/{PRODUCT}/v/"),
            })
            .map_err(|_| TransparencyPublishError::ChainFetch {
                observed: "genesis listing failure".to_owned(),
                expected: "HTTP 200 empty prefix".to_owned(),
            })?;
        if listing.status != 200 {
            return Err(TransparencyPublishError::ChainFetch {
                observed: format!("genesis listing HTTP {}", listing.status),
                expected: "HTTP 200 empty prefix".to_owned(),
            });
        }
        let keys = listed_keys(&listing.body)?;
        let genesis_recovery = if keys.is_empty() {
            false
        } else if let Some(version) = genesis_recovery_version {
            let allowed = format!("releases/{PRODUCT}/v/{version}/");
            if !keys.iter().all(|key| key.starts_with(&allowed)) {
                return Err(TransparencyPublishError::GenesisNotEmpty);
            }
            true
        } else {
            return Err(TransparencyPublishError::GenesisNotEmpty);
        };
        return Ok(ChainState {
            pointer: None,
            pointer_bytes: None,
            pointer_etag: None,
            entries: Vec::new(),
            genesis_recovery,
        });
    }
    if pointer_response.status != 200 || signature_response.status != 200 {
        return Err(TransparencyPublishError::ChainFetch {
            observed: format!(
                "pointer HTTP {} and signature HTTP {}",
                pointer_response.status, signature_response.status
            ),
            expected: "both HTTP 200".to_owned(),
        });
    }
    let pointer = parse_pointer(&pointer_response.body)?;
    verify_signature_bytes(
        runner,
        access.minisign_program,
        &access.environment.minisign_public_key,
        &pointer_response.body,
        &signature_response.body,
        &access
            .checkout_root
            .join(TRANSPARENCY_STAGE_ROOT)
            .join(".fetch-pointer-verify"),
    )?;
    require_latest_trusted_comment_matches_body(
        &pointer,
        trusted_comment(&signature_response.body)?,
    )
    .map_err(|_| TransparencyPublishError::ChainInvalid {
        observed: "pointer trusted comment mismatch".to_owned(),
        expected: "comment fields equal canonical body".to_owned(),
    })?;

    let tip = fetch_signed_entry(access, transport, runner, &pointer.version)?;
    if transparency_sha256_hex(&tip.bytes) != pointer.tip_sha256
        || tip.model.seq != pointer.chain_length
    {
        return Err(TransparencyPublishError::ChainInvalid {
            observed: "pointer identity differs from the locked tip".to_owned(),
            expected: "tip digest and chain length equality".to_owned(),
        });
    }
    let ledger_key = format!("releases/{PRODUCT}/ledger.jsonl");
    let ledger_response = transport
        .get(
            &object(TransparencyPlane::S3, &ledger_key),
            TransparencyFetchPolicy::Bypass,
        )
        .map_err(|_| TransparencyPublishError::ChainFetch {
            observed: "ledger transport failure".to_owned(),
            expected: "readable derived ledger or recoverable absence".to_owned(),
        })?;
    let fast = if ledger_response.status == 200 {
        validate_ledger_bytes(&ledger_response.body)
            .ok()
            .filter(|entries| {
                entries.last().is_some_and(|entry| {
                    transparency_sha256_hex(&entry.bytes) == pointer.tip_sha256
                        && entry.model == tip.model
                        && entry.bytes == tip.bytes
                })
            })
    } else {
        None
    };
    let entries = if let Some(entries) = fast {
        entries
    } else {
        let walked = walk_locked_chain(access, transport, runner, tip)?;
        if ledger_response.status == 200 {
            reject_ledger_contradictions(&ledger_response.body, &walked)?;
        } else if ledger_response.status != 404 {
            return Err(TransparencyPublishError::ChainFetch {
                observed: format!("ledger HTTP {}", ledger_response.status),
                expected: "HTTP 200 or 404".to_owned(),
            });
        }
        walked
    };
    Ok(ChainState {
        pointer: Some(pointer),
        pointer_bytes: Some(pointer_response.body),
        pointer_etag: pointer_response.etag,
        entries,
        genesis_recovery: false,
    })
}

fn listed_keys(bytes: &[u8]) -> Result<Vec<String>, TransparencyPublishError> {
    let text = std::str::from_utf8(bytes).map_err(|_| TransparencyPublishError::ChainInvalid {
        observed: "non-UTF-8 genesis listing".to_owned(),
        expected: "an XML object listing".to_owned(),
    })?;
    let mut remaining = text;
    let mut keys = Vec::new();
    while let Some(start) = remaining.find("<Key>") {
        remaining = &remaining[start + "<Key>".len()..];
        let end =
            remaining
                .find("</Key>")
                .ok_or_else(|| TransparencyPublishError::ChainInvalid {
                    observed: "malformed genesis listing".to_owned(),
                    expected: "closed Key elements".to_owned(),
                })?;
        let key = &remaining[..end];
        if key.is_empty() || !key.is_ascii() {
            return Err(TransparencyPublishError::ChainInvalid {
                observed: "invalid genesis listing key".to_owned(),
                expected: "nonempty ASCII object keys".to_owned(),
            });
        }
        keys.push(key.to_owned());
        remaining = &remaining[end + "</Key>".len()..];
    }
    Ok(keys)
}

fn fetch_signed_entry<T: TransparencyObjectTransport, R: CommandRunner + ?Sized>(
    access: &AccessContext<'_>,
    transport: &T,
    runner: &R,
    version: &str,
) -> Result<ChainEntry, TransparencyPublishError> {
    let prefix = format!("releases/{PRODUCT}/v/{version}");
    let body = fetch_required(
        transport,
        object(
            TransparencyPlane::S3,
            &format!("{prefix}/ledger-entry.json"),
        ),
        TransparencyFetchPolicy::Bypass,
    )?;
    let signature = fetch_required(
        transport,
        object(
            TransparencyPlane::S3,
            &format!("{prefix}/ledger-entry.json.minisig"),
        ),
        TransparencyFetchPolicy::Bypass,
    )?;
    let model = parse_entry(&body.body)?;
    verify_signature_bytes(
        runner,
        access.minisign_program,
        &access.environment.minisign_public_key,
        &body.body,
        &signature.body,
        &access
            .checkout_root
            .join(TRANSPARENCY_STAGE_ROOT)
            .join(".fetch-entry-verify"),
    )?;
    require_entry_trusted_comment_matches_body(
        &model,
        &body.body,
        trusted_comment(&signature.body)?,
    )
    .map_err(|_| TransparencyPublishError::ChainInvalid {
        observed: "entry trusted comment mismatch".to_owned(),
        expected: "comment fields equal canonical body".to_owned(),
    })?;
    Ok(ChainEntry {
        model,
        bytes: body.body,
        signature: Some(signature.body),
    })
}

fn walk_locked_chain<T: TransparencyObjectTransport, R: CommandRunner + ?Sized>(
    access: &AccessContext<'_>,
    transport: &T,
    runner: &R,
    tip: ChainEntry,
) -> Result<Vec<ChainEntry>, TransparencyPublishError> {
    let mut reversed = vec![tip];
    loop {
        let current = reversed.last().expect("walk always has current entry");
        if current.model.seq == 1 {
            if current.model.prev_version.is_empty() && current.model.prev_sha256 == "0".repeat(64)
            {
                break;
            }
            return Err(TransparencyPublishError::ChainInvalid {
                observed: "invalid genesis linkage".to_owned(),
                expected: "sequence 1 with empty previous version and zero digest".to_owned(),
            });
        }
        if current.model.prev_version.is_empty() {
            return Err(TransparencyPublishError::ChainInvalid {
                observed: "missing previous version".to_owned(),
                expected: "self-navigating backward link".to_owned(),
            });
        }
        let previous = fetch_signed_entry(access, transport, runner, &current.model.prev_version)?;
        if previous.model.seq.checked_add(1) != Some(current.model.seq)
            || transparency_sha256_hex(&previous.bytes) != current.model.prev_sha256
            || previous.model.published_utc >= current.model.published_utc
        {
            return Err(TransparencyPublishError::ChainInvalid {
                observed: "broken previous entry linkage".to_owned(),
                expected: "contiguous sequence digest and increasing time".to_owned(),
            });
        }
        reversed.push(previous);
    }
    reversed.reverse();
    Ok(reversed)
}

fn validate_ledger_bytes(bytes: &[u8]) -> Result<Vec<ChainEntry>, TransparencyPublishError> {
    if bytes.is_empty() || !bytes.ends_with(b"\n") {
        return Err(TransparencyPublishError::ChainInvalid {
            observed: "non-canonical derived ledger framing".to_owned(),
            expected: "one canonical newline-terminated object per line".to_owned(),
        });
    }
    let mut entries = Vec::new();
    for raw in bytes.split_inclusive(|byte| *byte == b'\n') {
        let model = parse_entry(raw)?;
        if let Some(previous) = entries.last() {
            let previous: &ChainEntry = previous;
            if previous.model.seq.checked_add(1) != Some(model.seq)
                || model.prev_sha256 != transparency_sha256_hex(&previous.bytes)
                || model.prev_version != previous.model.version
                || model.published_utc <= previous.model.published_utc
            {
                return Err(TransparencyPublishError::ChainInvalid {
                    observed: "derived ledger has broken linkage".to_owned(),
                    expected: "contiguous sequence digest version and increasing time".to_owned(),
                });
            }
        } else if model.seq != 1
            || !model.prev_version.is_empty()
            || model.prev_sha256 != "0".repeat(64)
        {
            return Err(TransparencyPublishError::ChainInvalid {
                observed: "derived ledger has invalid genesis".to_owned(),
                expected: "explicit sequence 1 genesis".to_owned(),
            });
        }
        entries.push(ChainEntry {
            model,
            bytes: raw.to_vec(),
            signature: None,
        });
    }
    Ok(entries)
}

fn reject_ledger_contradictions(
    bytes: &[u8],
    locked: &[ChainEntry],
) -> Result<(), TransparencyPublishError> {
    if bytes.is_empty() || !bytes.ends_with(b"\n") {
        return Ok(());
    }
    let locked_by_version: BTreeMap<_, _> = locked
        .iter()
        .map(|entry| (entry.model.version.as_str(), entry.bytes.as_slice()))
        .collect();
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        let Ok(entry) = parse_entry(line) else {
            continue;
        };
        if locked_by_version
            .get(entry.version.as_str())
            .is_some_and(|locked_bytes| *locked_bytes != line)
        {
            return Err(TransparencyPublishError::ChainInvalid {
                observed: "derived ledger contradicts a locked entry".to_owned(),
                expected: "byte identity for every overlapping version".to_owned(),
            });
        }
    }
    Ok(())
}

fn parse_entry(bytes: &[u8]) -> Result<TransparencyLedgerEntryV1, TransparencyPublishError> {
    let entry = parse_canonical(
        bytes,
        validate_transparency_entry_value,
        render_transparency_entry,
    )?;
    if entry.product != PRODUCT {
        return Err(TransparencyPublishError::ChainInvalid {
            observed: "foreign entry product".to_owned(),
            expected: PRODUCT.to_owned(),
        });
    }
    Ok(entry)
}

fn parse_pointer(bytes: &[u8]) -> Result<TransparencyLatestV1, TransparencyPublishError> {
    let pointer = parse_canonical(
        bytes,
        validate_transparency_latest_value,
        render_transparency_latest,
    )?;
    if pointer.product != PRODUCT {
        return Err(TransparencyPublishError::ChainInvalid {
            observed: "foreign pointer product".to_owned(),
            expected: PRODUCT.to_owned(),
        });
    }
    Ok(pointer)
}

fn parse_canonical<T>(
    bytes: &[u8],
    validate: fn(
        &serde_json::Value,
    ) -> Result<(), crate::transparency_format::TransparencyFormatError>,
    render: fn(&T) -> Result<Vec<u8>, crate::transparency_format::TransparencyFormatError>,
) -> Result<T, TransparencyPublishError>
where
    T: DeserializeOwned,
{
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| TransparencyPublishError::ChainInvalid {
            observed: "malformed JSON".to_owned(),
            expected: "schema-valid canonical JSON".to_owned(),
        })?;
    validate(&value).map_err(|_| TransparencyPublishError::ChainInvalid {
        observed: "schema-invalid JSON".to_owned(),
        expected: "runtime schema validation".to_owned(),
    })?;
    let model: T =
        serde_json::from_value(value).map_err(|_| TransparencyPublishError::ChainInvalid {
            observed: "untyped JSON shape".to_owned(),
            expected: "the exact typed transparency model".to_owned(),
        })?;
    if render(&model).map_err(|_| TransparencyPublishError::ChainInvalid {
        observed: "unrenderable transparency value".to_owned(),
        expected: "ASCII canonical JSON".to_owned(),
    })? != bytes
    {
        return Err(TransparencyPublishError::ChainInvalid {
            observed: "non-canonical JSON bytes".to_owned(),
            expected: "bytewise sorted compact JSON with one newline".to_owned(),
        });
    }
    Ok(model)
}

fn probe_version_entry<T: TransparencyObjectTransport, R: CommandRunner + ?Sized>(
    manifest: &Manifest,
    request: &TransparencyPublishRequest<'_>,
    transport: &T,
    runner: &R,
    current_tip: Option<&ChainEntry>,
) -> Result<Option<ChainEntry>, TransparencyPublishError> {
    let prefix = format!("releases/{PRODUCT}/v/{}", manifest.version);
    let entry_response = transport
        .get(
            &object(
                TransparencyPlane::S3,
                &format!("{prefix}/ledger-entry.json"),
            ),
            TransparencyFetchPolicy::Bypass,
        )
        .map_err(|_| TransparencyPublishError::ChainFetch {
            observed: "version preflight transport failure".to_owned(),
            expected: "HTTP 200 or 404".to_owned(),
        })?;
    let signature_response = transport
        .get(
            &object(
                TransparencyPlane::S3,
                &format!("{prefix}/ledger-entry.json.minisig"),
            ),
            TransparencyFetchPolicy::Bypass,
        )
        .map_err(|_| TransparencyPublishError::ChainFetch {
            observed: "version signature preflight failure".to_owned(),
            expected: "HTTP 200 or 404".to_owned(),
        })?;
    if entry_response.status == 404 && signature_response.status == 404 {
        return Ok(None);
    }
    if entry_response.status != 200 || signature_response.status != 200 {
        return Err(poisoned_error(
            manifest,
            current_tip.map_or(0, |tip| tip.model.seq.saturating_add(1)),
            "unknown",
        ));
    }
    let entry = parse_entry(&entry_response.body)?;
    verify_signature_bytes(
        runner,
        request.minisign_program,
        &request.environment.minisign_public_key,
        &entry_response.body,
        &signature_response.body,
        &request
            .checkout_root
            .join(TRANSPARENCY_STAGE_ROOT)
            .join(".probe-version-verify"),
    )?;
    require_entry_trusted_comment_matches_body(
        &entry,
        &entry_response.body,
        trusted_comment(&signature_response.body)?,
    )
    .map_err(|_| {
        poisoned_error(
            manifest,
            entry.seq,
            &transparency_sha256_hex(&entry_response.body),
        )
    })?;

    let companion_bytes = fs::read(request.release_dir.join(companion_basename()))
        .map_err(|_| TransparencyPublishError::CandidateInvalid)?;
    let companion = TransparencyNamedDigest {
        name: companion_basename(),
        sha256: transparency_sha256_hex(&companion_bytes),
    };
    let proofs = live_proof_digests(request, manifest, &companion)?;
    assert_entry_matches_candidate(&entry, manifest, &companion, &proofs).map_err(|_| {
        poisoned_error(
            manifest,
            entry.seq,
            &transparency_sha256_hex(&entry_response.body),
        )
    })?;
    let candidate = ChainEntry {
        model: entry,
        bytes: entry_response.body,
        signature: Some(signature_response.body),
    };
    if current_tip.is_some_and(|tip| tip.bytes == candidate.bytes) {
        return Ok(Some(candidate));
    }
    let expected_seq = current_tip.map_or(1, |tip| tip.model.seq.saturating_add(1));
    let expected_previous = current_tip
        .map(|tip| transparency_sha256_hex(&tip.bytes))
        .unwrap_or_else(|| "0".repeat(64));
    let expected_version = current_tip
        .map(|tip| tip.model.version.as_str())
        .unwrap_or("");
    if candidate.model.seq != expected_seq
        || candidate.model.prev_sha256 != expected_previous
        || candidate.model.prev_version != expected_version
    {
        return Err(poisoned_error(
            manifest,
            candidate.model.seq,
            &transparency_sha256_hex(&candidate.bytes),
        ));
    }
    Ok(Some(candidate))
}

fn live_proof_digests(
    request: &TransparencyPublishRequest<'_>,
    manifest: &Manifest,
    companion: &TransparencyNamedDigest,
) -> Result<Vec<TransparencyNamedDigest>, TransparencyPublishError> {
    let required = manifest
        .native_tools
        .get("signing_mode")
        .is_none_or(|mode| mode != "unsigned");
    let path = request.evidence_dir.join(WINDOWS_NATIVE_PROOF_FILENAME);
    if !path.exists() {
        return if required {
            Err(TransparencyPublishError::ProofMissing)
        } else {
            Ok(Vec::new())
        };
    }
    let metadata =
        fs::symlink_metadata(&path).map_err(|_| TransparencyPublishError::ProofInvalid)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(TransparencyPublishError::ProofInvalid);
    }
    let bytes = fs::read(path).map_err(|_| TransparencyPublishError::ProofInvalid)?;
    let receipt: WindowsNativeProofReceipt =
        serde_json::from_slice(&bytes).map_err(|_| TransparencyPublishError::ProofInvalid)?;
    if render_windows_native_proof_receipt(&receipt)
        .map_err(|_| TransparencyPublishError::ProofInvalid)?
        != bytes
        || receipt.product != PRODUCT
        || receipt.version != manifest.version
        || receipt.source_commit != manifest.source_commit
        || receipt.companion_manifest.filename != companion.name
        || receipt.companion_manifest.sha256 != companion.sha256
    {
        return Err(TransparencyPublishError::ProofInvalid);
    }
    Ok(vec![TransparencyNamedDigest {
        name: WINDOWS_NATIVE_PROOF_FILENAME.to_owned(),
        sha256: transparency_sha256_hex(&bytes),
    }])
}

fn poisoned_error(manifest: &Manifest, seq: u64, sha256: &str) -> TransparencyPublishError {
    TransparencyPublishError::VersionPoisoned {
        version: manifest.version.clone(),
        source_commit: manifest.source_commit.clone(),
        seq,
        sha256: sha256.to_owned(),
    }
}

fn require_entry_fits_chain(
    entry: &TransparencyLedgerEntryV1,
    bytes: &[u8],
    chain: &ChainState,
) -> Result<(), TransparencyPublishError> {
    if chain.entries.last().is_some_and(|tip| tip.bytes == bytes) {
        return Ok(());
    }
    let expected_seq = chain.entries.last().map_or(1, |tip| tip.model.seq + 1);
    let expected_previous = chain
        .entries
        .last()
        .map(|tip| transparency_sha256_hex(&tip.bytes))
        .unwrap_or_else(|| "0".repeat(64));
    let expected_version = chain
        .entries
        .last()
        .map(|tip| tip.model.version.as_str())
        .unwrap_or("");
    if entry.seq != expected_seq
        || entry.prev_sha256 != expected_previous
        || entry.prev_version != expected_version
    {
        return Err(TransparencyPublishError::StageConflict);
    }
    Ok(())
}

fn archive_stage<R: CommandRunner + ?Sized>(
    request: &TransparencyPublishRequest<'_>,
    runner: &R,
    staged: &StagedPublication,
) -> Result<(), TransparencyPublishError> {
    let output = runner.run(
        &request.environment.archive_channel,
        &[path_text(&staged.archive)?],
        None,
        None,
    )?;
    if output.status != 0 {
        return Err(TransparencyPublishError::ArchiveFailed {
            observed: format!("exit status {}", output.status),
            expected: format!("exit 0 and ARCHIVED {}", staged.manifest.sha256),
        });
    }
    let observed = parse_archive_receipt(&output.stdout)?;
    if observed != staged.manifest.sha256 {
        return Err(TransparencyPublishError::ArchiveReceiptInvalid {
            observed,
            expected: staged.manifest.sha256.clone(),
        });
    }
    Ok(())
}

pub fn parse_archive_receipt(bytes: &[u8]) -> Result<String, TransparencyPublishError> {
    let text = std::str::from_utf8(bytes).map_err(|_| {
        TransparencyPublishError::ArchiveReceiptInvalid {
            observed: "non-UTF-8 output".to_owned(),
            expected: "final ARCHIVED digest line".to_owned(),
        }
    })?;
    let without_final_newline = text.strip_suffix('\n').unwrap_or(text);
    if without_final_newline.ends_with('\n') || without_final_newline.ends_with('\r') {
        return Err(TransparencyPublishError::ArchiveReceiptInvalid {
            observed: "trailing blank or carriage-return data".to_owned(),
            expected: "one final ARCHIVED digest line".to_owned(),
        });
    }
    let final_line = without_final_newline.rsplit('\n').next().unwrap_or("");
    let digest = final_line.strip_prefix("ARCHIVED ").filter(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    digest
        .map(str::to_owned)
        .ok_or_else(|| TransparencyPublishError::ArchiveReceiptInvalid {
            observed: "missing or malformed final receipt".to_owned(),
            expected: "ARCHIVED followed by one lowercase SHA-256".to_owned(),
        })
}

fn require_pointer_unchanged<T: TransparencyObjectTransport>(
    transport: &T,
    chain: &ChainState,
) -> Result<(), TransparencyPublishError> {
    let response = transport
        .get(
            &object(
                TransparencyPlane::S3,
                &format!("releases/{PRODUCT}/latest.json"),
            ),
            TransparencyFetchPolicy::Bypass,
        )
        .map_err(|_| TransparencyPublishError::ConcurrentPublish)?;
    match &chain.pointer_bytes {
        Some(expected) if response.status == 200 && &response.body == expected => Ok(()),
        None if response.status == 404 => Ok(()),
        _ => Err(TransparencyPublishError::ConcurrentPublish),
    }
}

fn put_mutable_and_verify<T: TransparencyObjectTransport>(
    transport: &T,
    key: &str,
    bytes: &[u8],
    if_match: Option<&str>,
) -> Result<(), TransparencyPublishError> {
    let response = transport
        .mutable_put(
            &object(TransparencyPlane::S3, key),
            bytes,
            TransparencyCachePolicy::NoCache,
            if_match,
        )
        .map_err(|_| TransparencyPublishError::MutableWrite {
            observed: 0,
            expected: "HTTP 200..299".to_owned(),
        })?;
    if !(200..300).contains(&response.status) {
        return Err(TransparencyPublishError::MutableWrite {
            observed: response.status,
            expected: "HTTP 200..299".to_owned(),
        });
    }
    let fetched = transport
        .get(
            &object(TransparencyPlane::Public, key),
            TransparencyFetchPolicy::Bypass,
        )
        .map_err(|_| TransparencyPublishError::MutableVerification)?;
    if fetched.status != 200 || fetched.body != bytes {
        return Err(TransparencyPublishError::MutableVerification);
    }
    Ok(())
}

fn fetch_required<T: TransparencyObjectTransport>(
    transport: &T,
    destination: TransparencyObjectDestination,
    cache: TransparencyFetchPolicy,
) -> Result<ObservedHttpResponse, TransparencyPublishError> {
    let response =
        transport
            .get(&destination, cache)
            .map_err(|_| TransparencyPublishError::ChainFetch {
                observed: "transport failure".to_owned(),
                expected: "HTTP 200".to_owned(),
            })?;
    if response.status == 200 {
        Ok(response)
    } else {
        Err(TransparencyPublishError::ChainFetch {
            observed: format!("HTTP {}", response.status),
            expected: "HTTP 200".to_owned(),
        })
    }
}

fn object(plane: TransparencyPlane, key: &str) -> TransparencyObjectDestination {
    TransparencyObjectDestination {
        plane,
        key: key.to_owned(),
    }
}

fn chain_tip(entry: &ChainEntry) -> TransparencyTipIdentity {
    TransparencyTipIdentity {
        seq: entry.model.seq,
        version: entry.model.version.clone(),
        sha256: transparency_sha256_hex(&entry.bytes),
        published_utc: entry.model.published_utc.clone(),
    }
}

fn adopt_racing_entry<T: TransparencyObjectTransport, R: CommandRunner + ?Sized>(
    request: &TransparencyPublishRequest<'_>,
    transport: &T,
    runner: &R,
    chain: &ChainState,
    staged: &mut StagedPublication,
) -> Result<(), TransparencyPublishError> {
    let entry_response = fetch_required(
        transport,
        object(
            TransparencyPlane::S3,
            &format!("{}/ledger-entry.json", staged.version_prefix),
        ),
        TransparencyFetchPolicy::Bypass,
    )?;
    let signature_response = fetch_required(
        transport,
        object(
            TransparencyPlane::S3,
            &format!("{}/ledger-entry.json.minisig", staged.version_prefix),
        ),
        TransparencyFetchPolicy::Bypass,
    )?;
    let entry = parse_entry(&entry_response.body)?;
    verify_signature_bytes(
        runner,
        request.minisign_program,
        &request.environment.minisign_public_key,
        &entry_response.body,
        &signature_response.body,
        &staged.root.join("adopted-entry-verify"),
    )?;
    require_entry_trusted_comment_matches_body(
        &entry,
        &entry_response.body,
        trusted_comment(&signature_response.body)?,
    )
    .map_err(|_| TransparencyPublishError::ImmutableConflict)?;
    if !same_attempt_semantics(&entry, &staged.entry)
        || require_entry_fits_chain(&entry, &entry_response.body, chain).is_err()
    {
        return Err(TransparencyPublishError::ImmutableConflict);
    }

    let signed_at = UtcTimestamp::parse(&staged.pointer.signed_at)
        .map_err(|_| TransparencyPublishError::StageInvalid)?;
    let tip = chain_tip(&ChainEntry {
        model: entry.clone(),
        bytes: entry_response.body.clone(),
        signature: Some(signature_response.body.clone()),
    });
    let pointer = build_transparency_pointer(&tip, &signed_at)
        .map_err(|_| TransparencyPublishError::StageInvalid)?;
    let pointer_bytes =
        render_transparency_latest(&pointer).map_err(|_| TransparencyPublishError::StageInvalid)?;
    let pointer_signature = sign_bytes(
        runner,
        &publish_signing_material(request),
        &pointer_bytes,
        &format_latest_trusted_comment(&pointer),
        &staged.root.join("adopted-pointer-signing"),
    )?;
    let mut ledger_bytes = Vec::new();
    for prior in &chain.entries {
        ledger_bytes.extend_from_slice(&prior.bytes);
    }
    ledger_bytes.extend_from_slice(&entry_response.body);
    let version_dir = staged.archive.join(&staged.version_prefix);
    let product_dir = staged.archive.join(format!("releases/{PRODUCT}"));
    fs::write(version_dir.join("ledger-entry.json"), &entry_response.body)
        .map_err(|_| TransparencyPublishError::StageInvalid)?;
    fs::write(
        version_dir.join("ledger-entry.json.minisig"),
        &signature_response.body,
    )
    .map_err(|_| TransparencyPublishError::StageInvalid)?;
    fs::write(product_dir.join("ledger.jsonl"), &ledger_bytes)
        .map_err(|_| TransparencyPublishError::StageInvalid)?;
    fs::write(product_dir.join("latest.json"), &pointer_bytes)
        .map_err(|_| TransparencyPublishError::StageInvalid)?;
    fs::write(product_dir.join("latest.json.minisig"), &pointer_signature)
        .map_err(|_| TransparencyPublishError::StageInvalid)?;
    let manifest = render_staging_manifest_v1(&staged.archive)
        .map_err(|_| TransparencyPublishError::StageInvalid)?;
    fs::write(staged.root.join("stage-manifest.v1"), &manifest.bytes)
        .map_err(|_| TransparencyPublishError::StageInvalid)?;
    staged.entry = entry;
    staged.entry_bytes = entry_response.body;
    staged.entry_signature = signature_response.body;
    staged.pointer = pointer;
    staged.pointer_bytes = pointer_bytes;
    staged.pointer_signature = pointer_signature;
    staged.ledger_bytes = ledger_bytes;
    staged.manifest = manifest;
    Ok(())
}

fn same_attempt_semantics(
    left: &TransparencyLedgerEntryV1,
    right: &TransparencyLedgerEntryV1,
) -> bool {
    left.artifacts == right.artifacts
        && left.manifests == right.manifests
        && left.prev_sha256 == right.prev_sha256
        && left.prev_version == right.prev_version
        && left.product == right.product
        && left.proofs == right.proofs
        && left.schema == right.schema
        && left.seq == right.seq
        && left.source_commit == right.source_commit
        && left.version == right.version
}

fn check_head_log_floor(
    checkout_root: &Path,
    chain: &ChainState,
) -> Result<(), TransparencyPublishError> {
    let rows = read_head_log(&checkout_root.join(TRANSPARENCY_HEAD_LOG))?;
    let highest = rows
        .iter()
        .filter(|row| row.product == PRODUCT)
        .map(|row| row.seq)
        .max()
        .unwrap_or(0);
    let observed = chain
        .pointer
        .as_ref()
        .map_or(0, |pointer| pointer.chain_length);
    if observed < highest {
        return Err(TransparencyPublishError::Rollback {
            observed,
            expected: highest,
        });
    }
    Ok(())
}

fn append_head_log(
    checkout_root: &Path,
    entry: &TransparencyLedgerEntryV1,
    bytes: &[u8],
) -> Result<(), TransparencyPublishError> {
    let path = checkout_root.join(TRANSPARENCY_HEAD_LOG);
    let rows = read_head_log(&path)?;
    let digest = transparency_sha256_hex(bytes);
    if let Some(existing) = rows
        .iter()
        .find(|row| row.product == PRODUCT && row.seq == entry.seq)
    {
        return if existing.entry_sha256 == digest {
            Ok(())
        } else {
            Err(TransparencyPublishError::HeadLogFork)
        };
    }
    let row = TransparencyHeadLogRow {
        entry_sha256: digest,
        product: PRODUCT.to_owned(),
        published_utc: entry.published_utc.clone(),
        seq: entry.seq,
        version: entry.version.clone(),
    };
    let rendered = canonicalize_transparency_json(&row)
        .map_err(|_| TransparencyPublishError::HeadLogInvalid)?;
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|_| TransparencyPublishError::HeadLogWrite)?;
    file.write_all(&rendered)
        .and_then(|()| file.sync_all())
        .map_err(|_| TransparencyPublishError::HeadLogWrite)
}

fn read_head_log(path: &Path) -> Result<Vec<TransparencyHeadLogRow>, TransparencyPublishError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(TransparencyPublishError::HeadLogInvalid),
    };
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    if !bytes.ends_with(b"\n") {
        return Err(TransparencyPublishError::HeadLogInvalid);
    }
    let mut rows = Vec::new();
    let mut identities = BTreeMap::new();
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        let row: TransparencyHeadLogRow =
            serde_json::from_slice(line).map_err(|_| TransparencyPublishError::HeadLogInvalid)?;
        if row.product != PRODUCT
            || canonicalize_transparency_json(&row)
                .map_err(|_| TransparencyPublishError::HeadLogInvalid)?
                != line
        {
            return Err(TransparencyPublishError::HeadLogInvalid);
        }
        if let Some(previous) =
            identities.insert((row.product.clone(), row.seq), row.entry_sha256.clone())
        {
            if previous != row.entry_sha256 {
                return Err(TransparencyPublishError::HeadLogFork);
            }
        }
        rows.push(row);
    }
    Ok(rows)
}

fn pointer_is_expired<C: Clock + ?Sized>(
    pointer: Option<&TransparencyLatestV1>,
    clock: &C,
) -> Result<bool, TransparencyPublishError> {
    let Some(pointer) = pointer else {
        return Ok(false);
    };
    let now = clock
        .now()
        .map_err(|_| TransparencyPublishError::StageInvalid)?;
    let valid_until = UtcTimestamp::parse(&pointer.valid_until).map_err(|_| {
        TransparencyPublishError::ChainInvalid {
            observed: "invalid pointer validity time".to_owned(),
            expected: "canonical UTC".to_owned(),
        }
    })?;
    Ok(now.system_time() > valid_until.system_time())
}

fn existing_stage_receipt(
    request: &TransparencyPublishRequest<'_>,
    manifest: &Manifest,
) -> Option<StagingManifestV1> {
    let root = request
        .checkout_root
        .join(TRANSPARENCY_STAGE_ROOT)
        .join(PRODUCT)
        .join(&manifest.version);
    let record = fs::read(root.join("stage-manifest.v1")).ok()?;
    verify_staging_manifest_v1(&root.join("archive"), &record).ok()
}
