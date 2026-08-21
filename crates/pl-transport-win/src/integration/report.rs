// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The one result shape every integration operation emits.
//!
//! Exactly one of these is serialized to stdout per attempted operation; every
//! progress line and diagnostic goes to stderr. Reasons are stable tokens — for
//! transport faults they come from [`transport_error_code`](crate::transport_error_code),
//! the codebase's single sanitizer, so no second redaction policy exists here.

use serde::Serialize;

use crate::observe::DialCounts;
use crate::{transport_error_code, RelayError, TransportError};
use observer_pl::ingest::SegmentFileStatus;

/// The envelope's schema version. One constant, one place.
pub const SCHEMA_VERSION: u32 = 1;

/// Upper bound on the guidance string, so a reflected value can never grow the
/// envelope without limit.
const GUIDANCE_MAX_CHARS: usize = 240;

/// Exit statuses. The operator distinguishes an assertion failure from a
/// dependency/execution error from a bounded timeout without parsing stdout.
pub const EXIT_PASS: u8 = 0;
pub const EXIT_ASSERTION_FAILED: u8 = 1;
pub const EXIT_ERROR: u8 = 2;
pub const EXIT_DEADLINE: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Verdict {
    Pass,
    Fail,
    Error,
}

/// Where an operation got to. Path values elsewhere in the envelope reuse
/// [`TransportPath::as_str`](observer_model::TransportPath::as_str) rather than
/// inventing a parallel dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Validate,
    Precondition,
    Pair,
    Register,
    Persist,
    Heartbeat,
    ListSegments,
    BridgeStart,
    BridgeFetch,
    Ingest,
    Reconcile,
    Assert,
    Complete,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Validate => "validate",
            Self::Precondition => "precondition",
            Self::Pair => "pair",
            Self::Register => "register",
            Self::Persist => "persist",
            Self::Heartbeat => "heartbeat",
            Self::ListSegments => "list_segments",
            Self::BridgeStart => "bridge_start",
            Self::BridgeFetch => "bridge_fetch",
            Self::Ingest => "ingest",
            Self::Reconcile => "reconcile",
            Self::Assert => "assert",
            Self::Complete => "complete",
        }
    }
}

impl Serialize for Phase {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// How an operation ended, short of success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// An expectation the operator asserted did not hold. Not a product fault.
    Assertion {
        phase: Phase,
        reason: String,
        guidance: String,
    },
    /// A precondition, dependency, or execution fault.
    Error {
        phase: Phase,
        reason: String,
        guidance: String,
        retryable: bool,
    },
    /// The caller's fixed deadline elapsed first.
    Deadline { phase: Phase },
}

impl Failure {
    pub fn assertion(phase: Phase, reason: &str, guidance: &str) -> Self {
        Self::Assertion {
            phase,
            reason: reason.to_string(),
            guidance: bounded(guidance),
        }
    }

    pub fn error(phase: Phase, reason: &str, guidance: &str) -> Self {
        Self::Error {
            phase,
            reason: reason.to_string(),
            guidance: bounded(guidance),
            retryable: false,
        }
    }

    /// A transport fault, sanitized through the one existing error-code mapper.
    pub fn transport(phase: Phase, error: &TransportError, guidance: &str) -> Self {
        Self::Error {
            phase,
            reason: transport_error_code(error),
            guidance: bounded(guidance),
            retryable: transport_error_is_retryable(error),
        }
    }

    pub fn phase(&self) -> Phase {
        match self {
            Self::Assertion { phase, .. } | Self::Error { phase, .. } => *phase,
            Self::Deadline { phase } => *phase,
        }
    }

    pub fn verdict(&self) -> Verdict {
        match self {
            // A deadline is an expectation the caller set, so exceeding it is an
            // assertion failure — with its own exit status so it stays legible.
            Self::Assertion { .. } | Self::Deadline { .. } => Verdict::Fail,
            Self::Error { .. } => Verdict::Error,
        }
    }

    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Assertion { .. } => EXIT_ASSERTION_FAILED,
            Self::Error { .. } => EXIT_ERROR,
            Self::Deadline { .. } => EXIT_DEADLINE,
        }
    }

    fn reason(&self) -> String {
        match self {
            Self::Assertion { reason, .. } | Self::Error { reason, .. } => reason.clone(),
            Self::Deadline { .. } => "deadline_exceeded".to_string(),
        }
    }

    fn retryable(&self) -> bool {
        match self {
            Self::Assertion { .. } => false,
            Self::Error { retryable, .. } => *retryable,
            // The work may simply need longer, or a healthier network.
            Self::Deadline { .. } => true,
        }
    }

    fn guidance(&self) -> String {
        match self {
            Self::Assertion { guidance, .. } | Self::Error { guidance, .. } => guidance.clone(),
            Self::Deadline { .. } => {
                bounded("the operation's awaited async work spent its one --deadline-secs budget; raise the deadline or check relay reachability. Blocking local I/O, hashing, and envelope serialization are outside it and need the caller's process timeout")
            }
        }
    }
}

/// Connection-class faults are worth another run; a rejection or a malformed
/// response is deterministic.
fn transport_error_is_retryable(error: &TransportError) -> bool {
    match error {
        TransportError::Io(_) | TransportError::Tls(_) | TransportError::NoEndpoint => true,
        TransportError::Relay(relay) => matches!(
            relay,
            RelayError::HomeOffline
                | RelayError::Abnormal
                | RelayError::Overflow
                | RelayError::Stalled
        ),
        TransportError::Crypto(_)
        | TransportError::Mux(_)
        | TransportError::Http(_)
        | TransportError::Json(_)
        | TransportError::PairLink(_)
        | TransportError::Pairing(_)
        | TransportError::Ingest(_)
        | TransportError::Rejected { .. }
        | TransportError::RelayControlRejected { .. }
        | TransportError::NotPaired
        | TransportError::LocalOffset => false,
    }
}

fn bounded(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(GUIDANCE_MAX_CHARS)
        .collect()
}

/// The identity of the executable that ran the operation. Every field is honest
/// or absent — never a placeholder.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Artifact {
    /// Full lowercase 40-hex commit, or `null` when the build was not stamped.
    pub source_commit: Option<String>,
    pub app_version: String,
    /// SHA-256 of the running executable's bytes, or `null` when unreadable.
    pub executable_sha256: Option<String>,
}

/// Per-operation dial accounting, including failed and replaced legs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Dials {
    pub total: u64,
    pub direct_successes: u64,
    pub relay_successes: u64,
    /// The operator's asserted ceiling, when one was given.
    pub max_allowed: Option<u64>,
}

impl Dials {
    pub fn from_counts(counts: DialCounts, max_allowed: Option<u64>) -> Self {
        Self {
            total: counts.dial_attempts,
            direct_successes: counts.direct_successes,
            relay_successes: counts.relay_successes,
            max_allowed,
        }
    }
}

/// Per-operation evidence. One shape, with only the fields an operation earned.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Evidence {
    // pair
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registered: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_written: Option<bool>,
    /// Whether the local profile was left with no loadable credential.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_residue_cleared: Option<bool>,
    /// Residue that exists off this machine and cannot be cleaned locally.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_residue: Option<RemoteResidue>,

    // roundtrip
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heartbeat_ok: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segments_listed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segment_count: Option<u64>,

    // fetch
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bridge_contacted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_sha256: Option<String>,

    // upload
    #[serde(skip_serializing_if = "Option::is_none")]
    pub day: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segment: Option<String>,
    /// The server segment key the journal reported, which may differ from `segment`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_segment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_sent_before_close: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close_completed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmed: Option<bool>,

    // upload carrier and custody witness
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_carrier: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_carrier: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_submitted_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_custody_status: Option<SegmentFileStatus>,
}

/// What is known about state that exists off this machine.
///
/// A failed ceremony genuinely may not know how far it got, and saying "none"
/// there would be a fabricated fact. `Possible` is the honest third answer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Residue {
    /// Nothing was minted: the operation failed before the ceremony began.
    #[default]
    None,
    /// The ceremony began and failed; residue may or may not exist.
    Possible,
    /// The ceremony completed, so this residue definitely exists.
    Present,
}

impl Residue {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Possible => "possible",
            Self::Present => "present",
        }
    }
}

impl Serialize for Residue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// Residue a failed operation left somewhere this process cannot reach. Making
/// it explicit is how a partial failure avoids reading as a product failure.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct RemoteResidue {
    /// A pairing identity minted by the journal.
    pub journal_pairing_identity: Residue,
    /// A device enrollment minted by the relay.
    pub relay_device_enrollment: Residue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Envelope {
    pub schema_version: u32,
    pub operation: &'static str,
    pub verdict: Verdict,
    pub phase: Phase,
    pub reason: Option<String>,
    pub retryable: bool,
    pub guidance: String,
    pub elapsed_ms: u64,
    pub artifact: Artifact,
    pub dials: Dials,
    pub evidence: Evidence,
}

/// What the runner hands back: the single stdout object plus the exit status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub envelope: Envelope,
    pub exit_code: u8,
}

impl Outcome {
    pub fn json(&self) -> String {
        // The envelope is plain data with no map keys that can fail to serialize;
        // a fallback object keeps the exactly-one-object contract regardless.
        serde_json::to_string(&self.envelope).unwrap_or_else(|_| {
            format!(
                r#"{{"schema_version":{SCHEMA_VERSION},"operation":"{}","verdict":"ERROR","phase":"validate","reason":"envelope_serialize_failed","retryable":false,"guidance":"","elapsed_ms":0}}"#,
                self.envelope.operation
            )
        })
    }
}

/// Assemble the terminal envelope for an operation.
pub fn finish(
    operation: &'static str,
    failure: Option<Failure>,
    elapsed_ms: u64,
    artifact: Artifact,
    dials: Dials,
    evidence: Evidence,
) -> Outcome {
    let (verdict, phase, reason, retryable, guidance, exit_code) = match &failure {
        None => (
            Verdict::Pass,
            Phase::Complete,
            None,
            false,
            String::new(),
            EXIT_PASS,
        ),
        Some(failure) => (
            failure.verdict(),
            failure.phase(),
            Some(failure.reason()),
            failure.retryable(),
            failure.guidance(),
            failure.exit_code(),
        ),
    };

    Outcome {
        envelope: Envelope {
            schema_version: SCHEMA_VERSION,
            operation,
            verdict,
            phase,
            reason,
            retryable,
            guidance,
            elapsed_ms,
            artifact,
            dials,
            evidence,
        },
        exit_code,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use observer_pl::http::HttpError;

    fn artifact() -> Artifact {
        Artifact {
            source_commit: None,
            app_version: "1.2.3".to_string(),
            executable_sha256: None,
        }
    }

    #[test]
    fn pass_serializes_one_object_with_exit_zero() {
        let outcome = finish(
            "roundtrip",
            None,
            Default::default(),
            artifact(),
            Dials::default(),
            Evidence::default(),
        );
        assert_eq!(outcome.exit_code, EXIT_PASS);
        let value: serde_json::Value = serde_json::from_str(&outcome.json()).unwrap();
        assert_eq!(value["schema_version"], SCHEMA_VERSION);
        assert_eq!(value["verdict"], "PASS");
        assert_eq!(value["phase"], "complete");
        assert!(value["reason"].is_null());
        assert_eq!(value["operation"], "roundtrip");
    }

    #[test]
    fn each_failure_class_maps_to_its_own_exit_status() {
        let assertion = Failure::assertion(Phase::Assert, "path_not_relay", "use a relay journal");
        assert_eq!(assertion.verdict(), Verdict::Fail);
        assert_eq!(assertion.exit_code(), EXIT_ASSERTION_FAILED);

        let error = Failure::error(
            Phase::Precondition,
            "profile_not_empty",
            "clear the profile",
        );
        assert_eq!(error.verdict(), Verdict::Error);
        assert_eq!(error.exit_code(), EXIT_ERROR);

        let deadline = Failure::Deadline {
            phase: Phase::Heartbeat,
        };
        assert_eq!(deadline.verdict(), Verdict::Fail);
        assert_eq!(deadline.exit_code(), EXIT_DEADLINE);
        assert!(deadline.retryable());
    }

    #[test]
    fn transport_failures_reuse_the_one_sanitizer_and_leak_nothing() {
        let cases = [
            TransportError::Io(std::io::Error::other("C:\\Users\\me\\pairing.json")),
            TransportError::Tls("10.0.0.5:7657".into()),
            TransportError::PairLink("token=abc".into()),
            TransportError::Rejected {
                status: 503,
                body: "SECRET https://x/y?token=abc".into(),
            },
            TransportError::Http(HttpError::BadStatusLine("HTTP/1.1 SECRET".into())),
            TransportError::Relay(RelayError::HomeOffline),
        ];
        for error in cases {
            let failure = Failure::transport(Phase::Pair, &error, "check the journal");
            let outcome = finish(
                "pair",
                Some(failure),
                7,
                artifact(),
                Dials::default(),
                Evidence::default(),
            );
            let json = outcome.json();
            assert_eq!(json.matches("\"schema_version\"").count(), 1);
            for secret in [
                "SECRET",
                "token=",
                "Users",
                "https://",
                "10.0.0.5",
                "pairing.json",
            ] {
                assert!(!json.contains(secret), "{secret} leaked into {json}");
            }
        }
    }

    #[test]
    fn retryability_splits_connection_faults_from_deterministic_ones() {
        assert!(transport_error_is_retryable(&TransportError::NoEndpoint));
        assert!(transport_error_is_retryable(&TransportError::Relay(
            RelayError::Abnormal
        )));
        assert!(!transport_error_is_retryable(&TransportError::Relay(
            RelayError::Unpaid
        )));
        assert!(!transport_error_is_retryable(&TransportError::Rejected {
            status: 401,
            body: String::new(),
        }));
        assert!(!transport_error_is_retryable(&TransportError::NotPaired));
    }

    #[test]
    fn guidance_is_bounded_and_single_line() {
        let failure = Failure::error(Phase::Validate, "arg_bad_value", &"word ".repeat(400));
        let outcome = finish(
            "fetch",
            Some(failure),
            0,
            artifact(),
            Dials::default(),
            Evidence::default(),
        );
        let guidance = &outcome.envelope.guidance;
        assert!(guidance.chars().count() <= GUIDANCE_MAX_CHARS);
        assert!(!guidance.contains('\n'));
    }

    #[test]
    fn absent_evidence_fields_stay_out_of_the_object() {
        let outcome = finish(
            "pair",
            None,
            1,
            artifact(),
            Dials::default(),
            Evidence {
                registered: Some(true),
                ..Default::default()
            },
        );
        let value: serde_json::Value = serde_json::from_str(&outcome.json()).unwrap();
        assert_eq!(value["evidence"]["registered"], true);
        assert!(value["evidence"].get("http_status").is_none());
        assert!(value["evidence"].get("day").is_none());
    }

    #[test]
    fn artifact_identity_is_null_rather_than_a_placeholder() {
        let outcome = finish(
            "upload",
            None,
            0,
            artifact(),
            Dials::default(),
            Evidence::default(),
        );
        let value: serde_json::Value = serde_json::from_str(&outcome.json()).unwrap();
        assert!(value["artifact"]["source_commit"].is_null());
        assert!(value["artifact"]["executable_sha256"].is_null());
        assert_eq!(value["artifact"]["app_version"], "1.2.3");
    }
}
