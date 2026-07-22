// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use xtask::artifact_fs::child_process_path_text;
use xtask::release_exec::test_support::{FakeCommand, FakeCommandRunner};
use xtask::release_exec::{CommandOutput, CommandRunner, CommandRunnerError};
use xtask::release_finalizer::FinalizeError;
use xtask::release_selection::SelectedAction;
use xtask::release_signing::{
    verify_release_signing, AuthenticodePolicy, SigningError, SigningGrammarStage, SigningPolicy,
    SigningVerificationRequest, SIGNED_VERIFIED_MODE, UNSIGNED_MODE,
};

const SETUP: &str = "solstone-setup-0.2.11.exe";
#[cfg(not(windows))]
const SIGNTOOL: &str = "/selected/signtool.exe";
#[cfg(windows)]
const SIGNTOOL: &str = r"C:\selected\signtool.exe";
#[cfg(not(windows))]
const OTHER_SIGNTOOL: &str = "/other/signtool.exe";
#[cfg(windows)]
const OTHER_SIGNTOOL: &str = r"C:\other\signtool.exe";
const PUBLIC_LEAF: &str = "ac5472d41d5f63e339468e41f7b4438126e84860";
const PUBLIC_LEAF_UPPER: &str = "AC5472D41D5F63E339468E41F7B4438126E84860";
const SYNTHETIC_SIGNING_LEAF: &str = "1111111111111111111111111111111111111111";
const SYNTHETIC_TIMESTAMP_LEAF: &str = "2222222222222222222222222222222222222222";

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct Candidate {
    root: PathBuf,
}

impl Candidate {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "solstone-release-signing-{label}-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("create candidate root");
        fs::write(root.join(SETUP), b"inert signed setup bytes").expect("write setup fixture");
        Self { root }
    }

    fn setup_path(&self) -> PathBuf {
        let canonical =
            fs::canonicalize(self.root.join(SETUP)).expect("canonicalize setup fixture");
        PathBuf::from(child_process_path_text(&canonical).expect("child-process setup path"))
    }
}

impl Drop for Candidate {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove candidate root");
    }
}

fn policy_bytes() -> &'static [u8] {
    include_bytes!("../../packaging/signing-policy.json")
}

fn policy() -> SigningPolicy {
    SigningPolicy::parse(policy_bytes()).expect("parse committed signing policy")
}

fn action(program: &str) -> SelectedAction {
    SelectedAction {
        program: PathBuf::from(program),
        argv: ["verify", "/pa", "/all", "/v", "{file}"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
    }
}

fn accepted_grammar() -> String {
    include_str!("fixtures/signtool/verify-signed-setup.txt").to_owned()
}

fn synthetic_policy() -> SigningPolicy {
    SigningPolicy {
        schema: "synthetic-policy".to_owned(),
        authenticode: AuthenticodePolicy {
            leaf_sha1: SYNTHETIC_SIGNING_LEAF.to_owned(),
            require_trusted_chain: true,
            timestamp_protocol: "synthetic-protocol".to_owned(),
            require_timestamp: true,
        },
    }
}

fn synthetic_accepted_grammar() -> String {
    format!(
        concat!(
            "Verifying: synthetic-setup.exe\n",
            "Signature Index: 0 (Primary Signature)\n",
            "Hash of file (sha256): {}\n",
            "Signing Certificate Chain:\n",
            "Issued to: synthetic-signing\n",
            "Issued by: synthetic-signing\n",
            "Expires: synthetic-signing-expiration\n",
            "SHA1 hash: {}\n",
            "The signature is timestamped: synthetic-time\n",
            "Timestamp Verified by:\n",
            "Issued to: synthetic-timestamp\n",
            "Issued by: synthetic-timestamp\n",
            "Expires: synthetic-timestamp-expiration\n",
            "SHA1 hash: {}\n",
            "Successfully verified: synthetic-setup.exe\n",
            "Number of signatures successfully Verified: 1\n",
            "Number of warnings: 0\n",
            "Number of errors: 0\n",
        ),
        "A".repeat(64),
        SYNTHETIC_SIGNING_LEAF,
        SYNTHETIC_TIMESTAMP_LEAF,
    )
}

fn command(candidate: &Candidate, status: i32, stdout: &[u8], stderr: &[u8]) -> FakeCommand {
    FakeCommand {
        invocation: xtask::release_exec::test_support::CommandInvocation {
            program: PathBuf::from(SIGNTOOL),
            args: vec![
                "verify".to_owned(),
                "/pa".to_owned(),
                "/all".to_owned(),
                "/v".to_owned(),
                candidate
                    .setup_path()
                    .to_str()
                    .expect("utf8 setup path")
                    .to_owned(),
            ],
            stdin: None,
            env: None,
        },
        result: Ok(CommandOutput {
            status,
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        }),
    }
}

fn verify_with(
    candidate: &Candidate,
    runner: &impl CommandRunner,
    selected_signtool: &Path,
    selected_action: &SelectedAction,
) -> Result<xtask::release_signing::SigningVerification, SigningError> {
    let policy = policy();
    verify_release_signing(
        SigningVerificationRequest::Signed {
            policy: &policy,
            candidate_root: &candidate.root,
            setup_relative: SETUP,
            selected_signtool,
            action: selected_action,
        },
        runner,
    )
}

fn verify_with_policy(
    candidate: &Candidate,
    runner: &impl CommandRunner,
    signing_policy: &SigningPolicy,
) -> Result<xtask::release_signing::SigningVerification, SigningError> {
    verify_release_signing(
        SigningVerificationRequest::Signed {
            policy: signing_policy,
            candidate_root: &candidate.root,
            setup_relative: SETUP,
            selected_signtool: Path::new(SIGNTOOL),
            action: &action(SIGNTOOL),
        },
        runner,
    )
}

fn replace_synthetic_once(baseline: &str, from: &str, to: &str) -> String {
    assert!(
        baseline.contains(from),
        "synthetic mutation source must exist"
    );
    baseline.replacen(from, to, 1)
}

fn synthetic_grammar_mutation(stage: SigningGrammarStage) -> (Vec<u8>, Vec<u8>) {
    let baseline = synthetic_accepted_grammar();
    let stdout = match stage {
        SigningGrammarStage::StdoutEncoding => return (vec![0xff], Vec::new()),
        SigningGrammarStage::StderrEncoding => {
            return (baseline.into_bytes(), vec![0xff]);
        }
        SigningGrammarStage::VerifyingLine => replace_synthetic_once(
            &baseline,
            "Verifying: synthetic-setup.exe\n",
            "Verifier: synthetic-setup.exe\n",
        ),
        SigningGrammarStage::PrimarySignatureLine => replace_synthetic_once(
            &baseline,
            "Signature Index: 0 (Primary Signature)\n",
            concat!(
                "unexpected primary-signature position\n",
                "Signature Index: 0 (Primary Signature)\n",
            ),
        ),
        SigningGrammarStage::FileHashLine => replace_synthetic_once(
            &baseline,
            "Hash of file (sha256): ",
            "Unexpected file hash: ",
        ),
        SigningGrammarStage::FileHashValue => {
            replace_synthetic_once(&baseline, &"A".repeat(64), "synthetic-non-hex-file-hash")
        }
        SigningGrammarStage::SigningChainHeader => replace_synthetic_once(
            &baseline,
            "Signing Certificate Chain:\n",
            "Unexpected Signing Chain:\n",
        ),
        SigningGrammarStage::SigningCertificateIssuedTo => replace_synthetic_once(
            &baseline,
            "Issued to: synthetic-signing\n",
            "Unexpected signing subject\n",
        ),
        SigningGrammarStage::SigningCertificateIssuedBy => replace_synthetic_once(
            &baseline,
            "Issued by: synthetic-signing\n",
            "Unexpected signing issuer\n",
        ),
        SigningGrammarStage::SigningCertificateExpiration => replace_synthetic_once(
            &baseline,
            "Expires: synthetic-signing-expiration\n",
            "Unexpected signing expiration\n",
        ),
        SigningGrammarStage::SigningCertificateThumbprint => replace_synthetic_once(
            &baseline,
            "SHA1 hash: 1111111111111111111111111111111111111111\n",
            "Unexpected signing thumbprint\n",
        ),
        SigningGrammarStage::SigningCertificateFields => replace_synthetic_once(
            &baseline,
            SYNTHETIC_SIGNING_LEAF,
            "synthetic-non-hex-signing-thumbprint",
        ),
        SigningGrammarStage::SigningCertificateChain => replace_synthetic_once(
            &baseline,
            concat!(
                "Issued to: synthetic-signing\n",
                "Issued by: synthetic-signing\n",
                "Expires: synthetic-signing-expiration\n",
                "SHA1 hash: 1111111111111111111111111111111111111111\n",
            ),
            "",
        ),
        SigningGrammarStage::TimestampChainHeader => replace_synthetic_once(
            &baseline,
            "Timestamp Verified by:\n",
            concat!("Unexpected Timestamp Chain:\n", "Timestamp Verified by:\n",),
        ),
        SigningGrammarStage::TimestampCertificateIssuedTo => replace_synthetic_once(
            &baseline,
            "Issued to: synthetic-timestamp\n",
            "Unexpected timestamp subject\n",
        ),
        SigningGrammarStage::TimestampCertificateIssuedBy => replace_synthetic_once(
            &baseline,
            "Issued by: synthetic-timestamp\n",
            "Unexpected timestamp issuer\n",
        ),
        SigningGrammarStage::TimestampCertificateExpiration => replace_synthetic_once(
            &baseline,
            "Expires: synthetic-timestamp-expiration\n",
            "Unexpected timestamp expiration\n",
        ),
        SigningGrammarStage::TimestampCertificateThumbprint => replace_synthetic_once(
            &baseline,
            "SHA1 hash: 2222222222222222222222222222222222222222\n",
            "Unexpected timestamp thumbprint\n",
        ),
        SigningGrammarStage::TimestampCertificateFields => replace_synthetic_once(
            &baseline,
            SYNTHETIC_TIMESTAMP_LEAF,
            "synthetic-non-hex-timestamp-thumbprint",
        ),
        SigningGrammarStage::TimestampCertificateChain => replace_synthetic_once(
            &baseline,
            concat!(
                "Issued to: synthetic-timestamp\n",
                "Issued by: synthetic-timestamp\n",
                "Expires: synthetic-timestamp-expiration\n",
                "SHA1 hash: 2222222222222222222222222222222222222222\n",
            ),
            "",
        ),
        SigningGrammarStage::SuccessfullyVerifiedLine => baseline
            .split_once("Successfully verified: synthetic-setup.exe\n")
            .expect("synthetic success line")
            .0
            .to_owned(),
        SigningGrammarStage::SuccessCountLine => baseline
            .split_once("Number of signatures successfully Verified: 1\n")
            .expect("synthetic success count")
            .0
            .to_owned(),
        SigningGrammarStage::SuccessCountValue => replace_synthetic_once(
            &baseline,
            "Number of signatures successfully Verified: 1\n",
            "Number of signatures successfully Verified: 2\n",
        ),
        SigningGrammarStage::WarningCountLine => baseline
            .split_once("Number of warnings: 0\n")
            .expect("synthetic warning count")
            .0
            .to_owned(),
        SigningGrammarStage::ErrorCountLine => baseline
            .split_once("Number of errors: 0\n")
            .expect("synthetic error count")
            .0
            .to_owned(),
        SigningGrammarStage::TrailingOutput => {
            format!("{baseline}synthetic trailing output\n")
        }
    };
    (stdout.into_bytes(), Vec::new())
}

#[test]
fn every_signing_grammar_stage_is_reachable_through_real_parser() {
    let stages = [
        SigningGrammarStage::StdoutEncoding,
        SigningGrammarStage::StderrEncoding,
        SigningGrammarStage::VerifyingLine,
        SigningGrammarStage::PrimarySignatureLine,
        SigningGrammarStage::FileHashLine,
        SigningGrammarStage::FileHashValue,
        SigningGrammarStage::SigningChainHeader,
        SigningGrammarStage::SigningCertificateIssuedTo,
        SigningGrammarStage::SigningCertificateIssuedBy,
        SigningGrammarStage::SigningCertificateExpiration,
        SigningGrammarStage::SigningCertificateThumbprint,
        SigningGrammarStage::SigningCertificateFields,
        SigningGrammarStage::SigningCertificateChain,
        SigningGrammarStage::TimestampChainHeader,
        SigningGrammarStage::TimestampCertificateIssuedTo,
        SigningGrammarStage::TimestampCertificateIssuedBy,
        SigningGrammarStage::TimestampCertificateExpiration,
        SigningGrammarStage::TimestampCertificateThumbprint,
        SigningGrammarStage::TimestampCertificateFields,
        SigningGrammarStage::TimestampCertificateChain,
        SigningGrammarStage::SuccessfullyVerifiedLine,
        SigningGrammarStage::SuccessCountLine,
        SigningGrammarStage::SuccessCountValue,
        SigningGrammarStage::WarningCountLine,
        SigningGrammarStage::ErrorCountLine,
        SigningGrammarStage::TrailingOutput,
    ];
    let signing_policy = synthetic_policy();
    for stage in stages {
        let candidate = Candidate::new(&format!("grammar-stage-{stage:?}"));
        let (stdout, stderr) = synthetic_grammar_mutation(stage);
        let runner = FakeCommandRunner::new(vec![command(&candidate, 0, &stdout, &stderr)]);
        assert_eq!(
            verify_with_policy(&candidate, &runner, &signing_policy)
                .expect_err("synthetic mutation must reach its grammar stage"),
            SigningError::GrammarDrift { stage },
            "synthetic mutation did not reach {stage:?}",
        );
    }
}

#[test]
fn committed_policy_is_exact_closed_and_has_no_source_header() {
    let exact = concat!(
        "{\"schema\":\"solstone.signing-policy.v1\",\"authenticode\":{",
        "\"leaf_sha1\":\"ac5472d41d5f63e339468e41f7b4438126e84860\",",
        "\"require_trusted_chain\":true,\"timestamp_protocol\":\"rfc3161\",",
        "\"require_timestamp\":true}}\n"
    )
    .as_bytes();
    assert_eq!(policy_bytes(), exact);
    assert!(!std::str::from_utf8(policy_bytes())
        .expect("utf8 policy")
        .contains("SPDX"));
    let parsed = SigningPolicy::parse(policy_bytes()).expect("parse exact policy");
    assert_eq!(parsed.authenticode.leaf_sha1, PUBLIC_LEAF);

    let mut unknown: serde_json::Value =
        serde_json::from_slice(policy_bytes()).expect("parse policy value");
    unknown["authenticode"]["credential"] = serde_json::json!("private");
    assert_eq!(
        SigningPolicy::parse(&serde_json::to_vec(&unknown).expect("render mutation"))
            .expect_err("unknown policy field must fail"),
        SigningError::PolicyMalformed
    );
}

#[test]
fn unsigned_mode_provably_invokes_no_signer() {
    let runner = FakeCommandRunner::new(Vec::new());
    let verified = verify_release_signing(SigningVerificationRequest::Unsigned, &runner)
        .expect("unsigned mode needs no signer");
    assert_eq!(verified.signing_mode, UNSIGNED_MODE);
    assert_eq!(verified.setup_sha256, None);
    assert!(runner.witness().expect("read witness").is_empty());
}

#[test]
fn accepted_verbose_grammar_uses_selected_tool_and_yields_signed_verified() {
    let candidate = Candidate::new("accepted");
    let grammar = accepted_grammar();
    assert_eq!(grammar.matches(PUBLIC_LEAF_UPPER).count(), 1);
    let runner = FakeCommandRunner::new(vec![command(&candidate, 0, grammar.as_bytes(), b"")]);
    let verified = verify_with(&candidate, &runner, Path::new(SIGNTOOL), &action(SIGNTOOL))
        .expect("accept complete selected SignTool grammar");

    assert_eq!(verified.signing_mode, SIGNED_VERIFIED_MODE);
    assert_eq!(verified.setup_sha256.as_deref().map(str::len), Some(64));
    assert_eq!(runner.remaining().expect("read fake queue"), 0);
    // The real certificate and signing exercise is box-only post-ship. CI witnesses
    // this fail-closed verification grammar and selected-path call.
}

#[test]
fn selected_path_and_fixed_argv_are_enforced_before_invocation() {
    let candidate = Candidate::new("selected-path");
    let runner = FakeCommandRunner::new(Vec::new());
    assert_eq!(
        verify_with(
            &candidate,
            &runner,
            Path::new(SIGNTOOL),
            &action(OTHER_SIGNTOOL),
        )
        .expect_err("wrong selected SignTool must fail"),
        SigningError::WrongSelectedSignTool
    );
    assert!(runner.witness().expect("read witness").is_empty());

    let mut drifted = action(SIGNTOOL);
    drifted.argv[1] = "/kp".to_owned();
    assert_eq!(
        verify_with(&candidate, &runner, Path::new(SIGNTOOL), &drifted)
            .expect_err("argv drift must fail"),
        SigningError::SignToolActionInvalid
    );
}

#[test]
fn missing_duplicate_unsigned_and_wrong_leaf_are_rejected() {
    let cases = [
        (
            "missing-output",
            0,
            String::new(),
            SigningError::MissingOutput,
        ),
        (
            "missing-signature",
            0,
            accepted_grammar().replace("Signature Index: 0 (Primary Signature)\n", ""),
            SigningError::MissingSignature,
        ),
        (
            "duplicate",
            0,
            accepted_grammar().replace(
                "Signature Index: 0 (Primary Signature)\n",
                concat!(
                    "Signature Index: 0 (Primary Signature)\n",
                    "Signature Index: 1\n"
                ),
            ),
            SigningError::DuplicateSignature,
        ),
        (
            "unsigned",
            1,
            "SignTool Error: No signature found.\n".to_owned(),
            SigningError::Unsigned,
        ),
        (
            "wrong-leaf",
            0,
            accepted_grammar().replace(
                PUBLIC_LEAF_UPPER,
                "4444444444444444444444444444444444444444",
            ),
            SigningError::WrongLeaf,
        ),
    ];
    for (label, status, grammar, expected) in cases {
        let candidate = Candidate::new(label);
        let runner =
            FakeCommandRunner::new(vec![command(&candidate, status, grammar.as_bytes(), b"")]);
        assert_eq!(
            verify_with(&candidate, &runner, Path::new(SIGNTOOL), &action(SIGNTOOL),)
                .expect_err("invalid signature grammar must fail"),
            expected
        );
    }
}

#[test]
fn chain_timestamp_exit_and_grammar_failures_are_distinct() {
    let cases = [
        (
            "untrusted",
            1,
            format!(
                "{}SignTool Error: WinVerifyTrust returned error: certificate chain untrusted\n",
                accepted_grammar()
            ),
            SigningError::UntrustedChain,
        ),
        ("nonzero", 7, accepted_grammar(), SigningError::NonzeroExit),
        (
            "timestamp-absent",
            0,
            accepted_grammar().replace(
                "The signature is timestamped: Wed Jul 22 01:33:32 2026\n",
                "",
            ),
            SigningError::MissingTimestamp,
        ),
        (
            "timestamp-chain-absent",
            0,
            accepted_grammar().replace("Timestamp Verified by:\n", ""),
            SigningError::MissingTimestamp,
        ),
        (
            "legacy-protocol-line",
            0,
            accepted_grammar().replace(
                "The signature is timestamped: Wed Jul 22 01:33:32 2026\n",
                concat!(
                    "The signature is timestamped: Wed Jul 22 01:33:32 2026\n",
                    "Timestamp protocol: RFC3161\n"
                ),
            ),
            SigningError::GrammarDrift {
                stage: SigningGrammarStage::TimestampChainHeader,
            },
        ),
        (
            "timestamp-chain-mangled",
            0,
            accepted_grammar().replace(
                "                SHA1 hash: DD6230AC860A2D306BDA38B16879523007FB417E\n",
                "",
            ),
            SigningError::GrammarDrift {
                stage: SigningGrammarStage::TimestampCertificateThumbprint,
            },
        ),
        (
            "grammar-drift",
            0,
            format!("{}Unexpected trailing success prose\n", accepted_grammar()),
            SigningError::GrammarDrift {
                stage: SigningGrammarStage::TrailingOutput,
            },
        ),
    ];
    for (label, status, grammar, expected) in cases {
        let candidate = Candidate::new(label);
        let runner =
            FakeCommandRunner::new(vec![command(&candidate, status, grammar.as_bytes(), b"")]);
        assert_eq!(
            verify_with(&candidate, &runner, Path::new(SIGNTOOL), &action(SIGNTOOL),)
                .expect_err("signature policy mutation must fail"),
            expected
        );
    }
}

struct MutatingRunner {
    setup: PathBuf,
    output: CommandOutput,
}

impl CommandRunner for MutatingRunner {
    fn run(
        &self,
        _program: &Path,
        _args: &[String],
        _stdin: Option<&[u8]>,
        _env: Option<&BTreeMap<String, String>>,
    ) -> Result<CommandOutput, CommandRunnerError> {
        fs::write(&self.setup, b"mutated signed setup bytes").expect("mutate setup in fake");
        Ok(self.output.clone())
    }
}

#[test]
fn setup_mutation_during_verify_is_rejected_before_grammar_success() {
    let candidate = Candidate::new("mutated");
    let runner = MutatingRunner {
        setup: candidate.setup_path(),
        output: CommandOutput {
            status: 0,
            stdout: accepted_grammar().into_bytes(),
            stderr: Vec::new(),
        },
    };
    assert_eq!(
        verify_with(&candidate, &runner, Path::new(SIGNTOOL), &action(SIGNTOOL),)
            .expect_err("setup mutation must fail"),
        SigningError::SetupMutated
    );
}

#[test]
fn errors_and_results_do_not_leak_private_paths_credentials_or_certificates() {
    let candidate = Candidate::new("private-canary");
    let private_leaf = "5555555555555555555555555555555555555555";
    let private_subject = "private account credential";
    let grammar = accepted_grammar()
        .replace(PUBLIC_LEAF_UPPER, private_leaf)
        .replace("sol pbc", private_subject);
    let runner = FakeCommandRunner::new(vec![command(&candidate, 0, grammar.as_bytes(), b"")]);
    let wrong_leaf = verify_with(&candidate, &runner, Path::new(SIGNTOOL), &action(SIGNTOOL))
        .expect_err("wrong private leaf must fail");

    let grammar_canary = "SYNTHETIC-PRIVATE-CERTIFICATE-CANARY";
    let drifted = format!("{}\n{grammar_canary}\n", accepted_grammar().trim_end());
    let drift_candidate = Candidate::new("grammar-private-canary");
    let runner =
        FakeCommandRunner::new(vec![command(&drift_candidate, 0, drifted.as_bytes(), b"")]);
    let grammar_drift = verify_with(
        &drift_candidate,
        &runner,
        Path::new(SIGNTOOL),
        &action(SIGNTOOL),
    )
    .expect_err("unconsumed private output must fail");
    assert_eq!(
        grammar_drift,
        SigningError::GrammarDrift {
            stage: SigningGrammarStage::TrailingOutput,
        }
    );

    let all_errors = [
        SigningError::PolicyUnavailable,
        SigningError::PolicyMalformed,
        SigningError::PolicyMismatch,
        SigningError::WrongSelectedSignTool,
        SigningError::SignToolActionInvalid,
        SigningError::SetupContainment,
        SigningError::SetupReadFailed,
        SigningError::SetupMutated,
        SigningError::SignToolInvocationFailed,
        SigningError::MissingOutput,
        SigningError::Unsigned,
        SigningError::DuplicateSignature,
        SigningError::MissingSignature,
        wrong_leaf,
        SigningError::UntrustedChain,
        SigningError::NonzeroExit,
        SigningError::MissingTimestamp,
        grammar_drift,
    ];
    let forbidden = [
        candidate.root.to_str().expect("utf8 private root"),
        drift_candidate
            .root
            .to_str()
            .expect("utf8 drift private root"),
        "credential",
        private_leaf,
        grammar_canary,
        "Issued to:",
        "Issued by:",
        "SHA1 hash:",
    ];
    for error in all_errors {
        let direct = error.to_string();
        let promoted = FinalizeError::SigningVerification(error).to_string();
        for secret in &forbidden {
            assert!(!direct.contains(secret), "direct error leaked {secret:?}");
            assert!(
                !promoted.contains(secret),
                "promoted error leaked {secret:?}"
            );
        }
    }

    let candidate = Candidate::new("safe-result");
    let grammar = accepted_grammar();
    let runner = FakeCommandRunner::new(vec![command(&candidate, 0, grammar.as_bytes(), b"")]);
    let verified = verify_with(&candidate, &runner, Path::new(SIGNTOOL), &action(SIGNTOOL))
        .expect("verify safe result");
    let rendered = format!("{verified:?}");
    assert!(!rendered.contains(candidate.root.to_str().expect("utf8 root")));
    assert!(!rendered.contains("credential"));
    assert!(!rendered.contains("SHA1"));
}
