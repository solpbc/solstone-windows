// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Closed signing policy and fail-closed SignTool verification.

use std::fmt;
use std::path::Path;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::artifact_fs::{child_process_path_text, ContainedRoot, UnixModePolicy};
use crate::release_exec::{CommandOutput, CommandRunner};
use crate::release_selection::SelectedAction;

pub const SIGNING_POLICY_SCHEMA: &str = "solstone.signing-policy.v1";
pub const SIGNED_VERIFIED_MODE: &str = "signed-verified";
pub const UNSIGNED_MODE: &str = "unsigned";
const ARTIFACT_LEAF_SUBJECT: &str = "sol pbc";
const ARTIFACT_ROOT_SUBJECT: &str =
    "Microsoft Identity Verification Root Certificate Authority 2020";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SigningPolicy {
    pub schema: String,
    pub authenticode: AuthenticodePolicy,
    /// When present, the setup carries exactly one nested signature after the
    /// primary one: a Microsoft Artifact Signing signature whose leaf rotates,
    /// so it is admitted by its chain's subjects rather than a thumbprint.
    #[serde(default)]
    pub nested: Option<NestedSignerPolicy>,
    /// When present, every executable Velopack signs inside the packages (the
    /// app, its execution stub and the updater) carries exactly one Microsoft
    /// Artifact Signing signature and no other, admitted by its chain's subjects.
    #[serde(default)]
    pub internal: Option<InternalSignerPolicy>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct InternalSignerPolicy {
    pub leaf_subject: String,
    pub root_subject: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NestedSignerPolicy {
    pub leaf_subject: String,
    pub root_subject: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AuthenticodePolicy {
    pub leaf_sha1: String,
    pub require_trusted_chain: bool,
    pub timestamp_protocol: String,
    pub require_timestamp: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SigningVerification {
    pub signing_mode: &'static str,
    pub setup_sha256: Option<String>,
}

pub enum SigningVerificationRequest<'a> {
    Unsigned,
    Signed {
        policy: &'a SigningPolicy,
        candidate_root: &'a Path,
        setup_relative: &'a str,
        selected_signtool: &'a Path,
        action: &'a SelectedAction,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SigningGrammarStage {
    StdoutEncoding,
    StderrEncoding,
    VerifyingLine,
    PrimarySignatureLine,
    NestedSignatureLine,
    FileHashLine,
    FileHashValue,
    SigningChainHeader,
    SigningCertificateIssuedTo,
    SigningCertificateIssuedBy,
    SigningCertificateExpiration,
    SigningCertificateThumbprint,
    SigningCertificateFields,
    SigningCertificateChain,
    TimestampChainHeader,
    TimestampCertificateIssuedTo,
    TimestampCertificateIssuedBy,
    TimestampCertificateExpiration,
    TimestampCertificateThumbprint,
    TimestampCertificateFields,
    TimestampCertificateChain,
    SuccessfullyVerifiedLine,
    SuccessCountLine,
    SuccessCountValue,
    WarningCountLine,
    ErrorCountLine,
    TrailingOutput,
}

impl fmt::Display for SigningGrammarStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::StdoutEncoding => "stdout encoding",
            Self::StderrEncoding => "stderr encoding",
            Self::VerifyingLine => "verifying line",
            Self::PrimarySignatureLine => "primary-signature line",
            Self::NestedSignatureLine => "nested-signature line",
            Self::FileHashLine => "file-hash line",
            Self::FileHashValue => "file-hash value",
            Self::SigningChainHeader => "signing-chain header",
            Self::SigningCertificateIssuedTo => "signing-certificate subject line",
            Self::SigningCertificateIssuedBy => "signing-certificate issuer line",
            Self::SigningCertificateExpiration => "signing-certificate expiration line",
            Self::SigningCertificateThumbprint => "signing-certificate thumbprint line",
            Self::SigningCertificateFields => "signing-certificate field values",
            Self::SigningCertificateChain => "signing-certificate chain",
            Self::TimestampChainHeader => "timestamp-chain header",
            Self::TimestampCertificateIssuedTo => "timestamp-certificate subject line",
            Self::TimestampCertificateIssuedBy => "timestamp-certificate issuer line",
            Self::TimestampCertificateExpiration => "timestamp-certificate expiration line",
            Self::TimestampCertificateThumbprint => "timestamp-certificate thumbprint line",
            Self::TimestampCertificateFields => "timestamp-certificate field values",
            Self::TimestampCertificateChain => "timestamp-certificate chain",
            Self::SuccessfullyVerifiedLine => "successfully-verified line",
            Self::SuccessCountLine => "success-count line",
            Self::SuccessCountValue => "success-count value",
            Self::WarningCountLine => "warning-count line",
            Self::ErrorCountLine => "error-count line",
            Self::TrailingOutput => "trailing output",
        };
        formatter.write_str(label)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SigningError {
    PolicyUnavailable,
    PolicyMalformed,
    PolicyMismatch,
    WrongSelectedSignTool,
    SignToolActionInvalid,
    SetupContainment,
    SetupReadFailed,
    SetupMutated,
    SignToolInvocationFailed,
    MissingOutput,
    Unsigned,
    DuplicateSignature,
    MissingSignature,
    WrongLeaf,
    WrongNestedSigner,
    InternalUnsigned,
    WrongInternalSigner,
    UntrustedChain,
    NonzeroExit,
    MissingTimestamp,
    // SignTool verification errors retain only typed determinations and parser stages; raw stdout/stderr is never retained because even grammar-drift output can contain certificate material.
    GrammarDrift { stage: SigningGrammarStage },
}

impl fmt::Display for SigningError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PolicyUnavailable => write!(
                formatter,
                "signing policy could not be read from the contained checkout; restore packaging/signing-policy.json and retry"
            ),
            Self::PolicyMalformed => write!(
                formatter,
                "signing policy JSON is malformed or has an unknown field; restore packaging/signing-policy.json and retry"
            ),
            Self::PolicyMismatch => write!(
                formatter,
                "signing policy differs from the approved Authenticode identity or requirements; restore the reviewed public policy"
            ),
            Self::WrongSelectedSignTool => write!(
                formatter,
                "SignTool verify action does not use the selected SignTool path; rerun preflight and pass its record unchanged"
            ),
            Self::SignToolActionInvalid => write!(
                formatter,
                "SignTool verify argv is not exactly verify /pa /all /v with one file placeholder; rerun current signed preflight"
            ),
            Self::SetupContainment => write!(
                formatter,
                "setup executable containment failed; restore one regular contained versioned setup file and retry"
            ),
            Self::SetupReadFailed => write!(
                formatter,
                "setup executable could not be stable-read for signing verification; restore immutable candidate bytes and retry"
            ),
            Self::SetupMutated => write!(
                formatter,
                "setup executable changed during SignTool verification; discard the candidate and rebuild it in one transaction"
            ),
            Self::SignToolInvocationFailed => write!(
                formatter,
                "selected SignTool could not complete verification; restore the selected executable and retry"
            ),
            Self::MissingOutput => write!(
                formatter,
                "SignTool produced no verification grammar; verify with the selected /pa /all /v action and retry"
            ),
            Self::Unsigned => write!(
                formatter,
                "setup executable has no Authenticode signature; rebuild the candidate in signed mode"
            ),
            Self::DuplicateSignature => write!(
                formatter,
                "setup executable has multiple or duplicate Authenticode signature blocks; rebuild it with exactly one signature"
            ),
            Self::MissingSignature => write!(
                formatter,
                "SignTool output has no complete Authenticode signature block; rebuild and verify the signed setup"
            ),
            Self::WrongLeaf => write!(
                formatter,
                "Authenticode leaf certificate does not match the approved public thumbprint; sign with the approved release identity"
            ),
            Self::WrongNestedSigner => write!(
                formatter,
                "nested Authenticode signature does not chain from the approved root to the approved publisher subject; re-append it with the approved Artifact Signing profile"
            ),
            Self::InternalUnsigned => write!(
                formatter,
                "packaged executable has no Authenticode signature; check that Velopack's sign template ran the Artifact signer in replace mode and rebuild the candidate"
            ),
            Self::WrongInternalSigner => write!(
                formatter,
                "packaged executable is not signed by Artifact Signing alone; it needs exactly one timestamped signature chaining from the approved root to the approved publisher subject, so re-sign it through the Artifact sign template and rebuild the candidate"
            ),
            Self::UntrustedChain => write!(
                formatter,
                "SignTool /pa chain-policy verification failed; restore host trust and rebuild or reverify the candidate"
            ),
            Self::NonzeroExit => write!(
                formatter,
                "SignTool verification exited nonzero without a trusted-chain result; inspect the selected tool output and retry"
            ),
            Self::MissingTimestamp => write!(
                formatter,
                "Authenticode signature has no single verified timestamp chain; rebuild with the required RFC 3161 timestamp"
            ),
            Self::GrammarDrift { stage } => write!(
                formatter,
                "SignTool verbose output grammar drifted at the {stage}; inspect the selected pinned SignTool behavior and retry"
            ),
        }
    }
}

impl From<SigningGrammarStage> for SigningError {
    fn from(stage: SigningGrammarStage) -> Self {
        Self::GrammarDrift { stage }
    }
}

impl std::error::Error for SigningError {}

impl SigningPolicy {
    pub fn parse(bytes: &[u8]) -> Result<Self, SigningError> {
        let policy: Self =
            serde_json::from_slice(bytes).map_err(|_| SigningError::PolicyMalformed)?;
        if policy.schema != SIGNING_POLICY_SCHEMA
            || !is_lower_hex(&policy.authenticode.leaf_sha1, 40)
            || policy.authenticode.leaf_sha1 != "ac5472d41d5f63e339468e41f7b4438126e84860"
            || !policy.authenticode.require_trusted_chain
            || policy.authenticode.timestamp_protocol != "rfc3161"
            || !policy.authenticode.require_timestamp
            || policy.nested.as_ref().is_some_and(|nested| {
                nested.leaf_subject != ARTIFACT_LEAF_SUBJECT
                    || nested.root_subject != ARTIFACT_ROOT_SUBJECT
            })
            || policy.internal.as_ref().is_some_and(|internal| {
                internal.leaf_subject != ARTIFACT_LEAF_SUBJECT
                    || internal.root_subject != ARTIFACT_ROOT_SUBJECT
            })
        {
            return Err(SigningError::PolicyMismatch);
        }
        Ok(policy)
    }
}

pub fn verify_release_signing<R: CommandRunner + ?Sized>(
    request: SigningVerificationRequest<'_>,
    runner: &R,
) -> Result<SigningVerification, SigningError> {
    match request {
        SigningVerificationRequest::Unsigned => Ok(SigningVerification {
            signing_mode: UNSIGNED_MODE,
            setup_sha256: None,
        }),
        SigningVerificationRequest::Signed {
            policy,
            candidate_root,
            setup_relative,
            selected_signtool,
            action,
        } => verify_signed_setup(
            policy,
            candidate_root,
            setup_relative,
            selected_signtool,
            action,
            runner,
        ),
    }
}

fn verify_signed_setup<R: CommandRunner + ?Sized>(
    policy: &SigningPolicy,
    candidate_root: &Path,
    setup_relative: &str,
    selected_signtool: &Path,
    action: &SelectedAction,
    runner: &R,
) -> Result<SigningVerification, SigningError> {
    let (output, before_sha256) = run_selected_signtool(
        candidate_root,
        setup_relative,
        selected_signtool,
        action,
        runner,
    )?;
    parse_signtool_output(output.status, &output.stdout, &output.stderr, policy)?;
    Ok(SigningVerification {
        signing_mode: SIGNED_VERIFIED_MODE,
        setup_sha256: Some(before_sha256),
    })
}

/// Verifies one executable extracted from a package: exactly one Authenticode
/// signature, timestamped, whose chain runs from the policy's internal root to
/// its publisher subject and carries no certificate of the primary release
/// identity. The policy must name an internal signer.
pub fn verify_internal_signature<R: CommandRunner + ?Sized>(
    policy: &SigningPolicy,
    root: &Path,
    relative: &str,
    selected_signtool: &Path,
    action: &SelectedAction,
    runner: &R,
) -> Result<(), SigningError> {
    let internal = policy
        .internal
        .as_ref()
        .ok_or(SigningError::PolicyMismatch)?;
    let (output, _) = run_selected_signtool(root, relative, selected_signtool, action, runner)?;
    let chains = parse_signature_chains(output.status, &output.stdout, &output.stderr, 1).map_err(
        |error| match error {
            SigningError::Unsigned => SigningError::InternalUnsigned,
            SigningError::DuplicateSignature => SigningError::WrongInternalSigner,
            other => other,
        },
    )?;
    if !chain_matches_subjects(
        &chains[0],
        &internal.root_subject,
        &internal.leaf_subject,
        &policy.authenticode.leaf_sha1,
    ) {
        return Err(SigningError::WrongInternalSigner);
    }
    Ok(())
}

/// Runs the selected SignTool verify action on one contained file and proves the
/// file is byte-identical afterwards. Returns SignTool's output and the file's
/// SHA-256.
fn run_selected_signtool<R: CommandRunner + ?Sized>(
    candidate_root: &Path,
    setup_relative: &str,
    selected_signtool: &Path,
    action: &SelectedAction,
    runner: &R,
) -> Result<(CommandOutput, String), SigningError> {
    if action.program != selected_signtool {
        return Err(SigningError::WrongSelectedSignTool);
    }
    let expected = ["verify", "/pa", "/all", "/v", "{file}"];
    if action.argv.iter().map(String::as_str).ne(expected) {
        return Err(SigningError::SignToolActionInvalid);
    }

    let candidate = ContainedRoot::new(
        candidate_root,
        "release candidate",
        UnixModePolicy::AllowExecute,
    )
    .map_err(|_| SigningError::SetupContainment)?;
    let resolved = candidate
        .resolve(setup_relative, "versioned setup executable")
        .map_err(|_| SigningError::SetupContainment)?;
    let setup_path = candidate.canonical_path().join(setup_relative);
    let before = resolved.read().map_err(|_| SigningError::SetupReadFailed)?;
    let before_sha256 = sha256_hex(&before);
    let setup_arg = child_process_path_text(&setup_path).ok_or(SigningError::SetupContainment)?;
    let args: Vec<String> = action
        .argv
        .iter()
        .map(|arg| {
            if arg == "{file}" {
                setup_arg.clone()
            } else {
                arg.clone()
            }
        })
        .collect();
    let output = runner
        .run(&action.program, &args, None, None)
        .map_err(|_| SigningError::SignToolInvocationFailed)?;
    let after = candidate
        .read(setup_relative, "versioned setup executable")
        .map_err(|_| SigningError::SetupReadFailed)?;
    if before_sha256 != sha256_hex(&after) || before != after {
        return Err(SigningError::SetupMutated);
    }
    Ok((output, before_sha256))
}

fn parse_signtool_output(
    status: i32,
    stdout: &[u8],
    stderr: &[u8],
    policy: &SigningPolicy,
) -> Result<(), SigningError> {
    let signatures = if policy.nested.is_some() { 2 } else { 1 };
    let signing_chains = parse_signature_chains(status, stdout, stderr, signatures)?;
    let expected_leaf = &policy.authenticode.leaf_sha1;
    let primary = &signing_chains[0];
    if primary.last().map(|(_, thumbprint)| thumbprint) != Some(expected_leaf)
        || primary
            .iter()
            .filter(|(_, thumbprint)| thumbprint == expected_leaf)
            .count()
            != 1
    {
        return Err(SigningError::WrongLeaf);
    }
    if let Some(nested) = &policy.nested {
        if !chain_matches_subjects(
            &signing_chains[1],
            &nested.root_subject,
            &nested.leaf_subject,
            expected_leaf,
        ) {
            return Err(SigningError::WrongNestedSigner);
        }
    }
    Ok(())
}

/// A rotating Artifact Signing chain is admitted by its subjects: at least a root
/// and a leaf, the approved root first, the approved publisher last, and no
/// certificate of the pinned primary identity anywhere in it.
fn chain_matches_subjects(
    chain: &[(String, String)],
    root_subject: &str,
    leaf_subject: &str,
    primary_leaf_sha1: &str,
) -> bool {
    chain.len() >= 2
        && chain.first().map(|(subject, _)| subject.as_str()) == Some(root_subject)
        && chain.last().map(|(subject, _)| subject.as_str()) == Some(leaf_subject)
        && !chain
            .iter()
            .any(|(_, thumbprint)| thumbprint == primary_leaf_sha1)
}

/// Parses SignTool's `/pa /all /v` output as exactly `signatures` signature
/// blocks (one or two) and returns each block's signing chain, root first.
fn parse_signature_chains(
    status: i32,
    stdout: &[u8],
    stderr: &[u8],
    signatures: usize,
) -> Result<Vec<Vec<(String, String)>>, SigningError> {
    if stdout.is_empty() && stderr.is_empty() {
        return Err(SigningError::MissingOutput);
    }
    let stdout = std::str::from_utf8(stdout).map_err(|_| SigningGrammarStage::StdoutEncoding)?;
    let stderr = std::str::from_utf8(stderr).map_err(|_| SigningGrammarStage::StderrEncoding)?;
    let complete = if stderr.is_empty() {
        stdout.to_owned()
    } else if stdout.is_empty() {
        stderr.to_owned()
    } else {
        format!("{stdout}\n{stderr}")
    };
    let lower = complete.to_ascii_lowercase();
    if lower.contains("no signature") || lower.contains("not signed") || lower.contains("unsigned")
    {
        return Err(SigningError::Unsigned);
    }
    if status != 0 {
        return if lower.contains("winverifytrust")
            || lower.contains("untrusted chain")
            || lower.contains("chain could not be built")
        {
            Err(SigningError::UntrustedChain)
        } else {
            Err(SigningError::NonzeroExit)
        };
    }

    let lines: Vec<&str> = complete
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let expected_indexes: &[&str] = if signatures == 2 {
        &[PRIMARY_INDEX_LINE, NESTED_INDEX_LINE]
    } else {
        &[PRIMARY_INDEX_LINE]
    };
    let signature_indexes: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|line| line.starts_with("Signature Index:"))
        .collect();
    if signature_indexes.len() > expected_indexes.len() {
        return Err(SigningError::DuplicateSignature);
    }
    if signature_indexes.as_slice() != expected_indexes {
        return Err(SigningError::MissingSignature);
    }

    // The closed policy pins the timestamp requirement, so verification is unconditional.
    let timestamp_statements = lines
        .iter()
        .filter(|line| line.starts_with("The signature is timestamped: "))
        .count();
    let timestamp_chains = lines
        .iter()
        .filter(|line| **line == "Timestamp Verified by:")
        .count();
    if timestamp_statements != expected_indexes.len() || timestamp_chains != expected_indexes.len()
    {
        return Err(SigningError::MissingTimestamp);
    }

    let mut cursor = 0usize;
    expect_prefix(
        &lines,
        &mut cursor,
        "Verifying: ",
        SigningGrammarStage::VerifyingLine,
    )?;
    let mut signing_chains = Vec::with_capacity(expected_indexes.len());
    for (position, index_line) in expected_indexes.iter().enumerate() {
        expect_exact(
            &lines,
            &mut cursor,
            index_line,
            if position == 0 {
                SigningGrammarStage::PrimarySignatureLine
            } else {
                SigningGrammarStage::NestedSignatureLine
            },
        )?;
        signing_chains.push(parse_signature_block(&lines, &mut cursor)?);
    }
    take_prefix(
        &lines,
        &mut cursor,
        "Successfully verified: ",
        SigningGrammarStage::SuccessfullyVerifiedLine,
    )?;
    let count = lines
        .get(cursor)
        .ok_or(SigningGrammarStage::SuccessCountLine)?;
    let accepted_counts: &[&str] = if signatures == 2 {
        &["Number of signatures successfully Verified: 2"]
    } else {
        &[
            "Number of signatures successfully Verified: 1",
            "Number of files successfully Verified: 1",
        ]
    };
    if !accepted_counts.contains(count) {
        return Err(SigningGrammarStage::SuccessCountValue.into());
    }
    cursor += 1;
    expect_exact(
        &lines,
        &mut cursor,
        "Number of warnings: 0",
        SigningGrammarStage::WarningCountLine,
    )?;
    expect_exact(
        &lines,
        &mut cursor,
        "Number of errors: 0",
        SigningGrammarStage::ErrorCountLine,
    )?;
    if cursor != lines.len() {
        return Err(SigningGrammarStage::TrailingOutput.into());
    }
    Ok(signing_chains)
}

const PRIMARY_INDEX_LINE: &str = "Signature Index: 0 (Primary Signature)";
const NESTED_INDEX_LINE: &str = "Signature Index: 1";

/// One signature block after its index line: file hash, signing chain and its
/// verified timestamp chain. Returns the signing chain, root first.
fn parse_signature_block(
    lines: &[&str],
    cursor: &mut usize,
) -> Result<Vec<(String, String)>, SigningError> {
    let file_hash = take_prefix(
        lines,
        cursor,
        "Hash of file (sha256): ",
        SigningGrammarStage::FileHashLine,
    )?;
    if !is_upper_or_lower_hex(file_hash, 64) {
        return Err(SigningGrammarStage::FileHashValue.into());
    }
    expect_exact(
        lines,
        cursor,
        "Signing Certificate Chain:",
        SigningGrammarStage::SigningChainHeader,
    )?;
    let signing_chain = parse_certificate_chain(
        lines,
        cursor,
        &["The signature is timestamped: "],
        SIGNING_CERTIFICATE_STAGES,
    )?;
    lines
        .get(*cursor)
        .and_then(|line| line.strip_prefix("The signature is timestamped: "))
        .ok_or(SigningError::MissingTimestamp)?;
    *cursor += 1;
    expect_exact(
        lines,
        cursor,
        "Timestamp Verified by:",
        SigningGrammarStage::TimestampChainHeader,
    )?;
    parse_certificate_chain(
        lines,
        cursor,
        &["Successfully verified: ", "Signature Index: "],
        TIMESTAMP_CERTIFICATE_STAGES,
    )?;
    Ok(signing_chain)
}

#[derive(Clone, Copy)]
struct CertificateGrammarStages {
    issued_to: SigningGrammarStage,
    issued_by: SigningGrammarStage,
    expiration: SigningGrammarStage,
    thumbprint: SigningGrammarStage,
    fields: SigningGrammarStage,
    chain: SigningGrammarStage,
}

const SIGNING_CERTIFICATE_STAGES: CertificateGrammarStages = CertificateGrammarStages {
    issued_to: SigningGrammarStage::SigningCertificateIssuedTo,
    issued_by: SigningGrammarStage::SigningCertificateIssuedBy,
    expiration: SigningGrammarStage::SigningCertificateExpiration,
    thumbprint: SigningGrammarStage::SigningCertificateThumbprint,
    fields: SigningGrammarStage::SigningCertificateFields,
    chain: SigningGrammarStage::SigningCertificateChain,
};

const TIMESTAMP_CERTIFICATE_STAGES: CertificateGrammarStages = CertificateGrammarStages {
    issued_to: SigningGrammarStage::TimestampCertificateIssuedTo,
    issued_by: SigningGrammarStage::TimestampCertificateIssuedBy,
    expiration: SigningGrammarStage::TimestampCertificateExpiration,
    thumbprint: SigningGrammarStage::TimestampCertificateThumbprint,
    fields: SigningGrammarStage::TimestampCertificateFields,
    chain: SigningGrammarStage::TimestampCertificateChain,
};

fn parse_certificate_chain(
    lines: &[&str],
    cursor: &mut usize,
    terminators: &[&str],
    stages: CertificateGrammarStages,
) -> Result<Vec<(String, String)>, SigningGrammarStage> {
    let mut thumbprints = Vec::new();
    while lines.get(*cursor).is_some_and(|line| {
        !terminators
            .iter()
            .any(|terminator| line.starts_with(terminator))
    }) {
        let subject = take_prefix(lines, cursor, "Issued to: ", stages.issued_to)?;
        take_prefix(lines, cursor, "Issued by: ", stages.issued_by)?;
        take_prefix(lines, cursor, "Expires: ", stages.expiration)?;
        let thumbprint = take_prefix(lines, cursor, "SHA1 hash: ", stages.thumbprint)?;
        if !is_upper_or_lower_hex(thumbprint, 40) {
            return Err(stages.fields);
        }
        thumbprints.push((subject.to_owned(), thumbprint.to_ascii_lowercase()));
    }
    if thumbprints.is_empty() {
        return Err(stages.chain);
    }
    Ok(thumbprints)
}

fn expect_exact(
    lines: &[&str],
    cursor: &mut usize,
    expected: &str,
    stage: SigningGrammarStage,
) -> Result<(), SigningGrammarStage> {
    if lines.get(*cursor) != Some(&expected) {
        return Err(stage);
    }
    *cursor += 1;
    Ok(())
}

fn expect_prefix(
    lines: &[&str],
    cursor: &mut usize,
    prefix: &str,
    line_stage: SigningGrammarStage,
) -> Result<(), SigningGrammarStage> {
    take_prefix(lines, cursor, prefix, line_stage)?;
    Ok(())
}

fn take_prefix<'a>(
    lines: &'a [&str],
    cursor: &mut usize,
    prefix: &str,
    stage: SigningGrammarStage,
) -> Result<&'a str, SigningGrammarStage> {
    let value = lines
        .get(*cursor)
        .and_then(|line| line.strip_prefix(prefix))
        .ok_or(stage)?;
    *cursor += 1;
    Ok(value)
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_upper_or_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
