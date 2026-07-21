// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use xtask::release_exec::test_support::{FakeCommand, FakeCommandRunner};
use xtask::release_exec::{CommandOutput, CommandRunner, CommandRunnerError};
use xtask::release_selection::SelectedAction;
use xtask::release_signing::{
    verify_release_signing, SigningError, SigningPolicy, SigningVerificationRequest,
    SIGNED_VERIFIED_MODE, UNSIGNED_MODE,
};

const SETUP: &str = "solstone-setup-0.2.11.exe";
const SIGNTOOL: &str = "/selected/signtool.exe";
const PUBLIC_LEAF: &str = "ac5472d41d5f63e339468e41f7b4438126e84860";
const PUBLIC_LEAF_UPPER: &str = "AC5472D41D5F63E339468E41F7B4438126E84860";

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
        fs::canonicalize(self.root.join(SETUP)).expect("canonicalize setup fixture")
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
    format!(
        concat!(
            "Verifying: solstone-setup-0.2.11.exe\n",
            "Signature Index: 0 (Primary Signature)\n",
            "Hash of file (sha256): AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n",
            "Signing Certificate Chain:\n",
            "  Issued to: Public Root CA\n",
            "  Issued by: Public Root CA\n",
            "  Expires: Sat Jun 23 23:04:01 2035\n",
            "  SHA1 hash: 1111111111111111111111111111111111111111\n",
            "    Issued to: sol pbc\n",
            "    Issued by: Public Code Signing CA\n",
            "    Expires: Wed May 06 19:24:54 2027\n",
            "    SHA1 hash: {}\n",
            "The signature is timestamped: Tue Jul 21 12:00:00 2026\n",
            "Timestamp protocol: RFC3161\n",
            "Timestamp Verified by:\n",
            "  Issued to: Public Timestamp Root\n",
            "  Issued by: Public Timestamp Root\n",
            "  Expires: Mon Sep 30 19:32:25 2030\n",
            "  SHA1 hash: 2222222222222222222222222222222222222222\n",
            "    Issued to: Public Timestamp Service\n",
            "    Issued by: Public Timestamp Root\n",
            "    Expires: Wed Apr 22 20:42:47 2027\n",
            "    SHA1 hash: 3333333333333333333333333333333333333333\n",
            "Successfully verified: solstone-setup-0.2.11.exe\n",
            "Number of signatures successfully Verified: 1\n",
            "Number of warnings: 0\n",
            "Number of errors: 0\n"
        ),
        PUBLIC_LEAF_UPPER
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
    let runner = FakeCommandRunner::new(vec![command(&candidate, 0, grammar.as_bytes(), b"")]);
    let verified = verify_with(&candidate, &runner, Path::new(SIGNTOOL), &action(SIGNTOOL))
        .expect("accept complete selected SignTool grammar");

    assert_eq!(verified.signing_mode, SIGNED_VERIFIED_MODE);
    assert_eq!(verified.setup_sha256.as_deref().map(str::len), Some(64));
    assert_eq!(runner.remaining().expect("read fake queue"), 0);
    // The real certificate chain and RFC 3161 timestamp exercise is box-only
    // post-ship. CI witnesses this fail-closed parser and selected-path call.
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
            &action("/other/signtool.exe"),
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
                "The signature is timestamped: Tue Jul 21 12:00:00 2026\n",
                "",
            ),
            SigningError::MissingTimestamp,
        ),
        (
            "timestamp-invalid",
            0,
            accepted_grammar().replace(
                "Timestamp protocol: RFC3161",
                "Timestamp protocol: Authenticode",
            ),
            SigningError::InvalidRfc3161Timestamp,
        ),
        (
            "grammar-drift",
            0,
            format!("{}Unexpected trailing success prose\n", accepted_grammar()),
            SigningError::GrammarDrift,
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
    let grammar = accepted_grammar()
        .replace(PUBLIC_LEAF_UPPER, private_leaf)
        .replace("sol pbc", "private account credential");
    let runner = FakeCommandRunner::new(vec![command(&candidate, 0, grammar.as_bytes(), b"")]);
    let message = verify_with(&candidate, &runner, Path::new(SIGNTOOL), &action(SIGNTOOL))
        .expect_err("wrong private leaf must fail")
        .to_string();
    assert!(!message.contains(candidate.root.to_str().expect("utf8 root")));
    assert!(!message.contains("credential"));
    assert!(!message.contains(private_leaf));

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
