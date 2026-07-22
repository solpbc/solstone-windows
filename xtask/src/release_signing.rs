// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Closed signing policy and fail-closed SignTool verification.

use std::fmt;
use std::path::Path;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::artifact_fs::{child_process_path_text, ContainedRoot, UnixModePolicy};
use crate::release_exec::CommandRunner;
use crate::release_selection::SelectedAction;

pub const SIGNING_POLICY_SCHEMA: &str = "solstone.signing-policy.v1";
pub const SIGNED_VERIFIED_MODE: &str = "signed-verified";
pub const UNSIGNED_MODE: &str = "unsigned";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SigningPolicy {
    pub schema: String,
    pub authenticode: AuthenticodePolicy,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SigningError {
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
    UntrustedChain,
    NonzeroExit,
    MissingTimestamp,
    GrammarDrift,
}

impl fmt::Display for SigningError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
            Self::GrammarDrift => write!(
                formatter,
                "SignTool verbose output grammar is malformed, ambiguous, or unconsumed; inspect the selected pinned SignTool output before retrying"
            ),
        }
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
    parse_signtool_output(
        output.status,
        &output.stdout,
        &output.stderr,
        &policy.authenticode,
    )?;
    Ok(SigningVerification {
        signing_mode: SIGNED_VERIFIED_MODE,
        setup_sha256: Some(before_sha256),
    })
}

fn parse_signtool_output(
    status: i32,
    stdout: &[u8],
    stderr: &[u8],
    policy: &AuthenticodePolicy,
) -> Result<(), SigningError> {
    if stdout.is_empty() && stderr.is_empty() {
        return Err(SigningError::MissingOutput);
    }
    let stdout = std::str::from_utf8(stdout).map_err(|_| SigningError::GrammarDrift)?;
    let stderr = std::str::from_utf8(stderr).map_err(|_| SigningError::GrammarDrift)?;
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
    let signature_indexes: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|line| line.starts_with("Signature Index:"))
        .collect();
    if signature_indexes.len() > 1 {
        return Err(SigningError::DuplicateSignature);
    }
    if signature_indexes.as_slice() != ["Signature Index: 0 (Primary Signature)"] {
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
    if timestamp_statements != 1 || timestamp_chains != 1 {
        return Err(SigningError::MissingTimestamp);
    }

    let mut cursor = 0usize;
    expect_prefix(&lines, &mut cursor, "Verifying: ")?;
    expect_exact(
        &lines,
        &mut cursor,
        "Signature Index: 0 (Primary Signature)",
    )?;
    let file_hash = take_prefix(&lines, &mut cursor, "Hash of file (sha256): ")?;
    if !is_upper_or_lower_hex(file_hash, 64) {
        return Err(SigningError::GrammarDrift);
    }
    expect_exact(&lines, &mut cursor, "Signing Certificate Chain:")?;
    let signing_thumbprints =
        parse_certificate_chain(&lines, &mut cursor, "The signature is timestamped: ")?;
    let timestamp = take_prefix(&lines, &mut cursor, "The signature is timestamped: ")?;
    if timestamp.trim().is_empty() {
        return Err(SigningError::GrammarDrift);
    }
    expect_exact(&lines, &mut cursor, "Timestamp Verified by:")?;
    let timestamp_thumbprints =
        parse_certificate_chain(&lines, &mut cursor, "Successfully verified: ")?;
    let verified = take_prefix(&lines, &mut cursor, "Successfully verified: ")?;
    if verified.trim().is_empty() {
        return Err(SigningError::GrammarDrift);
    }
    let count = lines.get(cursor).ok_or(SigningError::GrammarDrift)?;
    if *count != "Number of signatures successfully Verified: 1"
        && *count != "Number of files successfully Verified: 1"
    {
        return Err(SigningError::GrammarDrift);
    }
    cursor += 1;
    expect_exact(&lines, &mut cursor, "Number of warnings: 0")?;
    expect_exact(&lines, &mut cursor, "Number of errors: 0")?;
    if cursor != lines.len() || timestamp_thumbprints.is_empty() {
        return Err(SigningError::GrammarDrift);
    }

    let expected_leaf = &policy.leaf_sha1;
    if signing_thumbprints.last() != Some(expected_leaf)
        || signing_thumbprints
            .iter()
            .filter(|thumbprint| *thumbprint == expected_leaf)
            .count()
            != 1
    {
        return Err(SigningError::WrongLeaf);
    }
    Ok(())
}

fn parse_certificate_chain(
    lines: &[&str],
    cursor: &mut usize,
    terminator: &str,
) -> Result<Vec<String>, SigningError> {
    let mut thumbprints = Vec::new();
    while lines
        .get(*cursor)
        .is_some_and(|line| !line.starts_with(terminator))
    {
        let issued_to = take_prefix(lines, cursor, "Issued to: ")?;
        let issued_by = take_prefix(lines, cursor, "Issued by: ")?;
        let expires = take_prefix(lines, cursor, "Expires: ")?;
        let thumbprint = take_prefix(lines, cursor, "SHA1 hash: ")?;
        if issued_to.trim().is_empty()
            || issued_by.trim().is_empty()
            || expires.trim().is_empty()
            || !is_upper_or_lower_hex(thumbprint, 40)
        {
            return Err(SigningError::GrammarDrift);
        }
        thumbprints.push(thumbprint.to_ascii_lowercase());
    }
    if thumbprints.is_empty() {
        return Err(SigningError::GrammarDrift);
    }
    Ok(thumbprints)
}

fn expect_exact(lines: &[&str], cursor: &mut usize, expected: &str) -> Result<(), SigningError> {
    if lines.get(*cursor) != Some(&expected) {
        return Err(SigningError::GrammarDrift);
    }
    *cursor += 1;
    Ok(())
}

fn expect_prefix(lines: &[&str], cursor: &mut usize, prefix: &str) -> Result<(), SigningError> {
    let value = take_prefix(lines, cursor, prefix)?;
    if value.trim().is_empty() {
        return Err(SigningError::GrammarDrift);
    }
    Ok(())
}

fn take_prefix<'a>(
    lines: &'a [&str],
    cursor: &mut usize,
    prefix: &str,
) -> Result<&'a str, SigningError> {
    let value = lines
        .get(*cursor)
        .and_then(|line| line.strip_prefix(prefix))
        .ok_or(SigningError::GrammarDrift)?;
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
