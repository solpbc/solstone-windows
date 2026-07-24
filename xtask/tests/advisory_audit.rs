// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use base64::Engine as _;
use sha2::{Digest, Sha256};
use twox_hash::XxHash64;
use url::Url;
use xtask::advisory_audit::{
    run_advisory_audit, AdvisoryAuditPrograms, AdvisoryAuditRequest, AdvisoryAuditTrust,
    AdvisoryAuditWitness, AuditError, CARGO_DENY_VERSION,
};
use xtask::artifact_fs::child_process_path_text;
use xtask::release_advisory::{
    canonical_freshness_body, format_advisory_mirror_trusted_comment, validate_mirror_locator,
    AdvisoryError,
};
use xtask::release_clock::FixedClock;
use xtask::release_exec::{
    CommandOutput, CommandRunner, CommandRunnerError, RemovedEnvironmentProcessCommandRunner,
};

const LOCATOR: &str = "https://synthetic-user@mirror.example.invalid/advisory-db";
const NOW: &str = "2026-07-23T12:00:00Z";
const KEY_ID: &str = "A1A2A3A4A5A6A7A8";
const CANARY: &str = "synthetic-child-output-canary";
const GIT: &str = "/synthetic-tools/git";
const MINISIGN: &str = "/synthetic-tools/minisign";
const CARGO_DENY: &str = "/synthetic-tools/cargo-deny";

#[derive(Clone, Debug)]
struct Invocation {
    program: PathBuf,
    args: Vec<String>,
    env: Option<BTreeMap<String, String>>,
}

struct RecordingRunner {
    commit: String,
    invocations: Mutex<Vec<Invocation>>,
}

impl RecordingRunner {
    fn new(commit: &str) -> Self {
        Self {
            commit: commit.to_owned(),
            invocations: Mutex::new(Vec::new()),
        }
    }

    fn invocations(&self) -> Vec<Invocation> {
        self.invocations.lock().expect("invocation lock").clone()
    }
}

impl CommandRunner for RecordingRunner {
    fn run(
        &self,
        program: &Path,
        args: &[String],
        _stdin: Option<&[u8]>,
        env: Option<&BTreeMap<String, String>>,
    ) -> Result<CommandOutput, CommandRunnerError> {
        self.invocations
            .lock()
            .map_err(|_| CommandRunnerError::FakeStatePoisoned)?
            .push(Invocation {
                program: program.to_path_buf(),
                args: args.to_vec(),
                env: env.cloned(),
            });
        let stdout = if program == Path::new(MINISIGN) && args == ["-v"] {
            b"minisign 0.12\n".to_vec()
        } else if program == Path::new(CARGO_DENY) && args == ["--version"] {
            format!("cargo-deny {CARGO_DENY_VERSION}\n").into_bytes()
        } else if args.iter().any(|arg| arg == "list-heads") {
            format!("{} HEAD\n{} refs/heads/main\n", self.commit, self.commit).into_bytes()
        } else if args.iter().any(|arg| arg == "HEAD^{commit}") {
            format!("{}\n", self.commit).into_bytes()
        } else if args.iter().any(|arg| arg == "--is-shallow-repository") {
            b"false\n".to_vec()
        } else if args.iter().any(|arg| arg == "status") {
            Vec::new()
        } else {
            CANARY.as_bytes().to_vec()
        };
        Ok(CommandOutput {
            status: 0,
            stdout,
            stderr: CANARY.as_bytes().to_vec(),
        })
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    checkout: PathBuf,
    packet_root: PathBuf,
    receipt: PathBuf,
    public_key: PathBuf,
    bundle: PathBuf,
    public_key_sha256: String,
    commit: String,
    ambient_cargo_home: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = tempfile::Builder::new()
            .prefix(&format!("advisory-audit-{label}-"))
            .tempdir()
            .expect("create synthetic audit fixture");
        let checkout = root.path().join("checkout");
        fs::create_dir_all(checkout.join("target")).expect("create target");
        fs::write(
            checkout.join("Cargo.toml"),
            b"[package]\nname = \"synthetic-advisory-consumer\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        )
        .expect("write synthetic manifest");
        fs::create_dir(checkout.join("src")).expect("create synthetic source directory");
        fs::write(
            checkout.join("src/lib.rs"),
            b"// Synthetic advisory-audit fixture.\n",
        )
        .expect("write synthetic source");
        fs::write(
            checkout.join("Cargo.lock"),
            b"# synthetic lockfile\nversion = 4\n\n[[package]]\nname = \"synthetic-advisory-consumer\"\nversion = \"0.0.0\"\n",
        )
        .expect("write synthetic lockfile");
        fs::write(
            checkout.join("deny.toml"),
            b"[advisories]\nyanked = \"warn\"\nunmaintained = \"workspace\"\nignore = [\n  { id = \"RUSTSEC-2026-0194\", reason = \"Synthetic policy fixture.\" },\n  { id = \"RUSTSEC-2026-0195\", reason = \"Synthetic policy fixture.\" },\n]\n",
        )
        .expect("write synthetic policy");
        for relative in [
            "Releases/sentinel",
            "target/release-candidate/sentinel",
            "target/release-evidence/sentinel",
        ] {
            let path = checkout.join(relative);
            fs::create_dir_all(path.parent().expect("sentinel parent"))
                .expect("create sentinel parent");
            fs::write(path, b"synthetic preserved bytes").expect("write sentinel");
        }

        let operator = root.path().join("operator");
        fs::create_dir(&operator).expect("create operator root");
        let commit = "1234567890abcdef1234567890abcdef12345678".to_owned();
        let receipt = operator.join("freshness.json");
        fs::write(&receipt, canonical_freshness_body(&commit, NOW, 86_400)).expect("write receipt");
        fs::write(
            operator.join("freshness.json.minisig"),
            format!(
                "untrusted comment: synthetic signature\nsynthetic-signature\ntrusted comment: {}\nsynthetic-global-signature\n",
                format_advisory_mirror_trusted_comment(&commit, NOW, 86_400)
            ),
        )
        .expect("write signature");
        let public_key_bytes = synthetic_public_key_file(&synthetic_key_payload());
        let public_key = operator.join("mirror.pub");
        fs::write(&public_key, &public_key_bytes).expect("write public key");
        let bundle = operator.join("mirror.bundle");
        fs::write(&bundle, b"synthetic bundle bytes").expect("write bundle");
        let ambient_cargo_home = root.path().join("ambient-cargo-home");
        fs::create_dir(&ambient_cargo_home).expect("create ambient cargo home");
        fs::write(
            ambient_cargo_home.join("sentinel"),
            b"synthetic ambient cargo-home bytes",
        )
        .expect("write ambient cargo-home sentinel");
        Self {
            _root: root,
            checkout,
            packet_root: operator,
            receipt,
            public_key,
            bundle,
            public_key_sha256: format!("{:x}", Sha256::digest(&public_key_bytes)),
            commit,
            ambient_cargo_home,
        }
    }

    fn request(&self) -> AdvisoryAuditRequest<'_> {
        AdvisoryAuditRequest {
            checkout_root: &self.checkout,
            locator: LOCATOR,
            receipt_path: &self.receipt,
            public_key_path: &self.public_key,
            bundle_path: &self.bundle,
            programs: AdvisoryAuditPrograms {
                git: Path::new(GIT),
                minisign: Path::new(MINISIGN),
                cargo_deny: Path::new(CARGO_DENY),
            },
            trust: AdvisoryAuditTrust {
                public_key_sha256: &self.public_key_sha256,
                public_key_id: KEY_ID,
            },
        }
    }
}

#[test]
fn signed_packet_bundle_and_offline_check_emit_one_canonical_witness() {
    let fixture = Fixture::new("success");
    let before = preserved_inventory(&fixture);
    let runner = RecordingRunner::new(&fixture.commit);
    let bytes = run_advisory_audit(
        &fixture.request(),
        &runner,
        &FixedClock::new(NOW).expect("clock"),
    )
    .expect("synthetic audit passes");
    let witness = single_line_witness(&bytes).expect("parse single-line witness");
    assert_eq!(witness.schema, "solstone.advisory-audit.v1");
    assert_eq!(witness.product, "solstone-windows");
    assert_eq!(witness.synced_commit, fixture.commit);
    assert_eq!(witness.receipt_utc, NOW);
    assert_eq!(witness.max_age, 86_400);
    assert_eq!(witness.checked_at, NOW);
    assert_eq!(witness.cargo_deny_version, CARGO_DENY_VERSION);
    assert_eq!(witness.verdict, "pass");
    assert!(!String::from_utf8_lossy(&bytes).contains(CANARY));
    assert!(
        preserved_inventory(&fixture) == before,
        "preserved recursive inventory changed"
    );
    assert_no_audit_temporary_state(&fixture.checkout);

    let invocations = runner.invocations();
    let operations: Vec<&str> = invocations.iter().map(operation_label).collect();
    assert_eq!(
        operations,
        vec![
            "minisign-version",
            "cargo-deny-version",
            "minisign-verify",
            "git-init",
            "bundle-verify",
            "bundle-list-heads",
            "bundle-clone",
            "checkout-commit",
            "head-commit",
            "shallow-check",
            "status-check",
            "cargo-deny-advisories",
        ]
    );
    assert!(invocations.iter().all(|invocation| {
        !invocation.args.iter().any(|arg| arg == LOCATOR)
            || invocation.program == Path::new(CARGO_DENY)
    }));
    assert_exact_git_contract(&invocations, &fixture.bundle, &fixture.commit);
    for invocation in invocations
        .iter()
        .filter(|invocation| invocation.program == Path::new(GIT))
    {
        let env = invocation.env.as_ref().expect("Git environment overlay");
        assert_eq!(
            env.get("GIT_CONFIG_NOSYSTEM").map(String::as_str),
            Some("1")
        );
        assert_eq!(env.get("GIT_CONFIG_COUNT").map(String::as_str), Some("0"));
        assert_eq!(
            env.get("GIT_ALLOW_PROTOCOL").map(String::as_str),
            Some("file")
        );
        assert_eq!(
            env.get("GIT_PROTOCOL_FROM_USER").map(String::as_str),
            Some("0")
        );
        assert_eq!(
            env.get("GIT_TERMINAL_PROMPT").map(String::as_str),
            Some("0")
        );
    }
    let cargo_check = invocations
        .iter()
        .find(|invocation| {
            invocation.program == Path::new(CARGO_DENY)
                && invocation.args.iter().any(|arg| arg == "advisories")
        })
        .expect("cargo-deny advisory invocation");
    assert!(cargo_check.args.iter().any(|arg| arg == "--manifest-path"));
    assert!(cargo_check.args.iter().any(|arg| arg == "--locked"));
    assert!(cargo_check.args.iter().any(|arg| arg == "--offline"));
    assert_eq!(
        cargo_check
            .env
            .as_ref()
            .and_then(|env| env.get("CARGO_NET_OFFLINE"))
            .map(String::as_str),
        Some("true")
    );
}

#[test]
fn single_line_witness_rejects_noncanonical_line_shapes() {
    let fixture = Fixture::new("single-line-shapes");
    let bytes = run_advisory_audit(
        &fixture.request(),
        &RecordingRunner::new(&fixture.commit),
        &FixedClock::new(NOW).expect("clock"),
    )
    .expect("synthetic audit passes");
    let witness = single_line_witness(&bytes).expect("real compact witness is accepted");

    let mut pretty = serde_json::to_vec_pretty(&witness).expect("pretty witness serializes");
    pretty.push(b'\n');
    assert!(
        single_line_witness(&pretty).is_none(),
        "pretty witness must be rejected"
    );

    let value: serde_json::Value = serde_json::from_slice(
        bytes
            .strip_suffix(b"\n")
            .expect("real compact witness has one trailing newline"),
    )
    .expect("real compact witness parses as JSON");
    let mut alphabetical_key_order =
        serde_json::to_vec(&value).expect("JSON value witness serializes");
    alphabetical_key_order.push(b'\n');
    assert!(
        single_line_witness(&alphabetical_key_order).is_none(),
        "witness with alphabetical key order must be rejected"
    );

    let mut carriage_return = bytes.clone();
    carriage_return.insert(carriage_return.len() - 1, b'\r');
    assert!(
        single_line_witness(&carriage_return).is_none(),
        "witness containing a carriage return must be rejected"
    );

    let missing_newline = &bytes[..bytes.len() - 1];
    assert!(
        single_line_witness(missing_newline).is_none(),
        "witness missing its trailing newline must be rejected"
    );

    let mut second_newline = bytes;
    second_newline.push(b'\n');
    assert!(
        single_line_witness(&second_newline).is_none(),
        "witness with a second trailing newline must be rejected"
    );
}

#[test]
fn child_environment_removal_covers_operator_inputs_and_git_trace_controls() {
    let removed: std::collections::BTreeSet<&str> =
        xtask::advisory_audit::ADVISORY_AUDIT_REMOVED_ENV
            .into_iter()
            .collect();
    assert_eq!(
        removed.len(),
        xtask::advisory_audit::ADVISORY_AUDIT_REMOVED_ENV.len(),
        "removed child environment list must not contain duplicates"
    );
    for required in [
        "SOLSTONE_ADVISORY_MIRROR_LOCATOR",
        "SOLSTONE_ADVISORY_RECEIPT",
        "SOLSTONE_ADVISORY_MIRROR_PUB",
        "SOLSTONE_ADVISORY_BUNDLE",
        "GIT_TRACE",
        "GIT_TRACE2",
        "GIT_TRACE2_EVENT",
        "GIT_TRACE2_PERF",
        "GIT_TRACE_PACKET",
        "GIT_TRACE_PACKFILE",
        "GIT_TRACE_PACK_ACCESS",
        "GIT_TRACE_PERFORMANCE",
        "GIT_TRACE_SETUP",
        "GIT_TRACE_CURL",
        "GIT_TRACE_CURL_NO_DATA",
        "GIT_TRACE_REFS",
        "GIT_TRACE_SHALLOW",
        "GIT_TRACE_FSMONITOR",
    ] {
        assert!(
            removed.contains(required),
            "required child environment removal is missing"
        );
    }
}

fn assert_exact_git_contract(invocations: &[Invocation], bundle: &Path, commit: &str) {
    let git: Vec<&Invocation> = invocations
        .iter()
        .filter(|invocation| invocation.program == Path::new(GIT))
        .collect();
    assert!(git.len() == 8, "unexpected Git invocation count");

    let verify_root = git[0].args.get(1).map(PathBuf::from);
    assert!(
        git[0].args.len() == 4
            && git[0].args.first().map(String::as_str) == Some("-C")
            && verify_root
                .as_deref()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                == Some("bundle-verify")
            && git[0].args.get(2).map(String::as_str) == Some("init")
            && git[0].args.get(3).map(String::as_str) == Some("--initial-branch=main"),
        "Git init argv contract changed"
    );
    // Child tools receive the simplified spelling, never the verbatim prefix that
    // Windows canonicalization adds; on Unix this is the canonical path unchanged.
    let canonical_bundle = PathBuf::from(
        child_process_path_text(&fs::canonicalize(bundle).expect("canonical synthetic bundle"))
            .expect("child-process text for the synthetic bundle"),
    );
    for (invocation, operation) in [(git[1], "verify"), (git[2], "list-heads")] {
        assert!(
            invocation.args.len() == 5
                && invocation.args.first().map(String::as_str) == Some("-C")
                && invocation.args.get(1).map(PathBuf::from) == verify_root
                && invocation.args.get(2).map(String::as_str) == Some("bundle")
                && invocation.args.get(3).map(String::as_str) == Some(operation)
                && invocation.args.get(4).map(PathBuf::from) == Some(canonical_bundle.clone()),
            "Git bundle argv contract changed"
        );
    }
    assert!(
        git[3].args.len() == 5
            && git[3].args.first().map(String::as_str) == Some("clone")
            && git[3].args.get(1).map(String::as_str) == Some("--no-checkout")
            && git[3].args.get(2).map(String::as_str) == Some("--no-tags")
            && git[3].args.get(3).map(PathBuf::from) == Some(canonical_bundle)
            && git[3]
                .args
                .get(4)
                .map(PathBuf::from)
                .as_deref()
                .and_then(Path::parent)
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                == Some("database"),
        "Git clone argv contract changed"
    );
    let database_checkout = git[3].args.get(4).map(PathBuf::from);
    for invocation in &git[4..] {
        assert!(
            invocation.args.first().map(String::as_str) == Some("-C")
                && invocation.args.get(1).map(PathBuf::from) == database_checkout,
            "Git checkout inspection root changed"
        );
    }
    assert!(
        git[4].args.len() == 6
            && git[4].args.get(2).map(String::as_str) == Some("checkout")
            && git[4].args.get(3).map(String::as_str) == Some("--detach")
            && git[4].args.get(4).map(String::as_str) == Some("--force")
            && git[4].args.get(5).map(String::as_str) == Some(commit),
        "Git checkout argv contract changed"
    );
    assert!(
        git[5].args.len() == 5
            && git[5].args.get(2).map(String::as_str) == Some("rev-parse")
            && git[5].args.get(3).map(String::as_str) == Some("--verify")
            && git[5].args.get(4).map(String::as_str) == Some("HEAD^{commit}"),
        "Git HEAD inspection argv contract changed"
    );
    assert!(
        git[6].args.len() == 4
            && git[6].args.get(2).map(String::as_str) == Some("rev-parse")
            && git[6].args.get(3).map(String::as_str) == Some("--is-shallow-repository"),
        "Git shallow inspection argv contract changed"
    );
    assert!(
        git[7].args.len() == 5
            && git[7].args.get(2).map(String::as_str) == Some("status")
            && git[7].args.get(3).map(String::as_str) == Some("--porcelain=v1")
            && git[7].args.get(4).map(String::as_str) == Some("--untracked-files=all"),
        "Git status argv contract changed"
    );
}

fn operation_label(invocation: &Invocation) -> &'static str {
    if invocation.program == Path::new(MINISIGN) {
        return if invocation.args.first().map(String::as_str) == Some("-v") {
            "minisign-version"
        } else {
            "minisign-verify"
        };
    }
    if invocation.program == Path::new(CARGO_DENY) {
        return if invocation.args.first().map(String::as_str) == Some("--version") {
            "cargo-deny-version"
        } else {
            "cargo-deny-advisories"
        };
    }
    if invocation.args.iter().any(|arg| arg == "list-heads") {
        "bundle-list-heads"
    } else if invocation.args.iter().any(|arg| arg == "verify") {
        "bundle-verify"
    } else if invocation.args.first().map(String::as_str) == Some("clone") {
        "bundle-clone"
    } else if invocation.args.iter().any(|arg| arg == "checkout") {
        "checkout-commit"
    } else if invocation.args.iter().any(|arg| arg == "HEAD^{commit}") {
        "head-commit"
    } else if invocation
        .args
        .iter()
        .any(|arg| arg == "--is-shallow-repository")
    {
        "shallow-check"
    } else if invocation.args.iter().any(|arg| arg == "status") {
        "status-check"
    } else {
        "git-init"
    }
}

#[test]
fn shared_locator_rejections_are_identical_and_scp_style_is_recurring_only() {
    let fixture = Fixture::new("locators");
    let malformed = " https://mirror.example.invalid/advisory-db";
    let mut request = fixture.request();
    request.locator = malformed;
    let shared = validate_mirror_locator(malformed).expect_err("shared locator rejects");
    assert_eq!(
        run_advisory_audit(
            &request,
            &RecordingRunner::new(&fixture.commit),
            &FixedClock::new(NOW).expect("clock"),
        )
        .expect_err("audit shares locator rejection"),
        AuditError::Authority(shared)
    );

    request.locator = "synthetic@mirror.example.invalid:advisory-db";
    assert_eq!(validate_mirror_locator(request.locator), Ok(()));
    assert_eq!(
        run_advisory_audit(
            &request,
            &RecordingRunner::new(&fixture.commit),
            &FixedClock::new(NOW).expect("clock"),
        )
        .expect_err("audit requires URL locator"),
        AuditError::LocatorNotUrl
    );
}

#[test]
fn recurring_locator_accepts_https_and_ssh_with_userinfo_and_ports() {
    let fixture = Fixture::new("url-shapes");
    let locators = [
        "https://synthetic-user@mirror.example.invalid:8443/advisory-db".to_owned(),
        "ssh://synthetic-user@mirror.example.invalid:2222/rustsec-advisory-db.git".to_owned(),
        format!("https://{}.{}.{}.{}:8443/advisory-db", 192, 0, 2, 1),
        format!(
            "ssh://[{}:{}::{}]:2222/rustsec-advisory-db.git",
            "2001", "db8", 1
        ),
    ];
    for locator in &locators {
        let mut request = fixture.request();
        request.locator = locator;
        let witness = run_advisory_audit(
            &request,
            &RecordingRunner::new(&fixture.commit),
            &FixedClock::new(NOW).expect("clock"),
        )
        .expect("hierarchical synthetic URL accepted");
        assert_eq!(
            serde_json::from_slice::<AdvisoryAuditWitness>(&witness)
                .expect("parse locator-shape witness")
                .verdict,
            "pass"
        );
    }
}

#[test]
fn key_id_gate_uses_the_signed_key_bytes_and_child_output_is_redacted() {
    let fixture = Fixture::new("key-id");
    let mut request = fixture.request();
    request.trust.public_key_id = "B1B2B3B4B5B6B7B8";
    let runner = RecordingRunner::new(&fixture.commit);
    let error = run_advisory_audit(&request, &runner, &FixedClock::new(NOW).expect("clock"))
        .expect_err("wrong key ID rejected");
    assert_eq!(
        error,
        AuditError::Authority(AdvisoryError::MirrorPublicKeyIdMismatch)
    );
    assert!(!error.to_string().contains(CANARY));
    assert_eq!(runner.invocations().len(), 2);
    assert_no_audit_temporary_state(&fixture.checkout);
}

#[test]
fn key_id_payload_parser_rejects_malformed_algorithm_length_and_shape() {
    let mut wrong_algorithm = b"XX".to_vec();
    wrong_algorithm.extend_from_slice(&0xA1A2_A3A4_A5A6_A7A8_u64.to_le_bytes());
    wrong_algorithm.extend_from_slice(&[0x5a; 32]);
    let mut wrong_length = b"Ed".to_vec();
    wrong_length.extend_from_slice(&0xA1A2_A3A4_A5A6_A7A8_u64.to_le_bytes());
    wrong_length.extend_from_slice(&[0x5a; 31]);
    for (label, bytes) in [
        (
            "base64",
            b"untrusted comment: synthetic mirror key\nnot-base64!\n".to_vec(),
        ),
        ("algorithm", synthetic_public_key_file(&wrong_algorithm)),
        ("length", synthetic_public_key_file(&wrong_length)),
        (
            "shape",
            [
                synthetic_public_key_file(&synthetic_key_payload()),
                b"unexpected third line\n".to_vec(),
            ]
            .concat(),
        ),
    ] {
        let mut fixture = Fixture::new(label);
        fs::write(&fixture.public_key, &bytes).expect("replace malformed public key");
        fixture.public_key_sha256 = format!("{:x}", Sha256::digest(&bytes));
        let runner = RecordingRunner::new(&fixture.commit);
        assert_eq!(
            run_advisory_audit(
                &fixture.request(),
                &runner,
                &FixedClock::new(NOW).expect("clock"),
            )
            .expect_err("malformed key payload rejected"),
            AuditError::Authority(AdvisoryError::MirrorPublicKeyIdMismatch)
        );
        assert_eq!(runner.invocations().len(), 2);
    }
}

fn synthetic_key_payload() -> Vec<u8> {
    let mut payload = b"Ed".to_vec();
    payload.extend_from_slice(&0xA1A2_A3A4_A5A6_A7A8_u64.to_le_bytes());
    payload.extend_from_slice(&[0x5a; 32]);
    payload
}

fn synthetic_public_key_file(payload: &[u8]) -> Vec<u8> {
    format!(
        "untrusted comment: synthetic mirror key\n{}\n",
        base64::engine::general_purpose::STANDARD.encode(payload)
    )
    .into_bytes()
}

#[test]
fn bundle_heads_must_be_exact_and_cleanup_still_runs() {
    let fixture = Fixture::new("heads");
    let runner = WrongHeadsRunner(RecordingRunner::new(&fixture.commit));
    let error = run_advisory_audit(
        &fixture.request(),
        &runner,
        &FixedClock::new(NOW).expect("clock"),
    )
    .expect_err("extra bundle head rejected");
    assert_eq!(error, AuditError::BundleHeadsMismatch);
    assert!(!error.to_string().contains(CANARY));
    assert_no_audit_temporary_state(&fixture.checkout);
}

#[test]
fn each_bundle_head_must_name_the_signed_commit() {
    for wrong_head in [WrongHead::Head, WrongHead::Main] {
        let fixture = Fixture::new("head-commit");
        let runner = WrongHeadCommitRunner {
            inner: RecordingRunner::new(&fixture.commit),
            wrong_head,
        };
        assert_eq!(
            run_advisory_audit(
                &fixture.request(),
                &runner,
                &FixedClock::new(NOW).expect("clock"),
            )
            .expect_err("bundle head at the wrong commit must fail"),
            AuditError::BundleHeadsMismatch
        );
        assert_no_audit_temporary_state(&fixture.checkout);
    }
}

#[test]
fn recursive_inventory_detects_new_files_in_every_preserved_scope() {
    for scope in [
        "packet",
        "source",
        "ambient",
        "releases",
        "candidate",
        "evidence",
    ] {
        let fixture = Fixture::new("inventory-addition");
        let before = preserved_inventory(&fixture);
        let root = match scope {
            "packet" => fixture.packet_root.clone(),
            "source" => fixture.checkout.clone(),
            "ambient" => fixture.ambient_cargo_home.clone(),
            "releases" => fixture.checkout.join("Releases"),
            "candidate" => fixture.checkout.join("target/release-candidate"),
            "evidence" => fixture.checkout.join("target/release-evidence"),
            _ => unreachable!("closed synthetic inventory scope"),
        };
        fs::write(root.join("synthetic-added-file"), b"synthetic added bytes")
            .expect("write recursive inventory mutation");
        assert!(
            preserved_inventory(&fixture) != before,
            "recursive inventory missed an added file"
        );
    }
}

#[cfg(unix)]
#[test]
fn cleanup_failure_suppresses_an_otherwise_successful_witness() {
    use std::os::unix::fs::PermissionsExt as _;

    let fixture = Fixture::new("cleanup-failure");
    let runner = CleanupFailureRunner {
        inner: RecordingRunner::new(&fixture.commit),
        run_root: Mutex::new(None),
    };
    assert_eq!(
        run_advisory_audit(
            &fixture.request(),
            &runner,
            &FixedClock::new(NOW).expect("clock"),
        )
        .expect_err("cleanup failure suppresses success"),
        AuditError::CleanupFailed
    );
    let run_root = runner
        .run_root
        .lock()
        .expect("cleanup root lock")
        .take()
        .expect("captured audit run root");
    let mut permissions = fs::metadata(&run_root)
        .expect("failed run metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&run_root, permissions).expect("restore failed-run permissions");
    fs::remove_dir_all(&run_root).expect("remove deliberately failed audit run");
    assert_no_audit_temporary_state(&fixture.checkout);
}

#[test]
fn cargo_lock_change_after_the_check_suppresses_the_witness() {
    let fixture = Fixture::new("lock-change");
    let runner = LockMutationRunner {
        inner: RecordingRunner::new(&fixture.commit),
        cargo_lock: fixture.checkout.join("Cargo.lock"),
    };
    assert_eq!(
        run_advisory_audit(
            &fixture.request(),
            &runner,
            &FixedClock::new(NOW).expect("clock"),
        )
        .expect_err("changed lockfile must suppress success"),
        AuditError::CargoLockChanged
    );
    assert_no_audit_temporary_state(&fixture.checkout);
}

#[test]
fn git_launch_failure_is_not_reported_as_a_bundle_failure() {
    let fixture = Fixture::new("git-launch");
    let runner = GitLaunchFailureRunner(RecordingRunner::new(&fixture.commit));
    assert_eq!(
        run_advisory_audit(
            &fixture.request(),
            &runner,
            &FixedClock::new(NOW).expect("clock"),
        )
        .expect_err("unlaunchable Git must fail its preflight"),
        AuditError::GitUnavailable
    );
    assert_no_audit_temporary_state(&fixture.checkout);
}

#[cfg(unix)]
struct CleanupFailureRunner {
    inner: RecordingRunner,
    run_root: Mutex<Option<PathBuf>>,
}

#[cfg(unix)]
impl CommandRunner for CleanupFailureRunner {
    fn run(
        &self,
        program: &Path,
        args: &[String],
        stdin: Option<&[u8]>,
        env: Option<&BTreeMap<String, String>>,
    ) -> Result<CommandOutput, CommandRunnerError> {
        use std::os::unix::fs::PermissionsExt as _;

        let output = self.inner.run(program, args, stdin, env)?;
        if program == Path::new(CARGO_DENY) && args.iter().any(|arg| arg == "advisories") {
            let config = args
                .iter()
                .position(|arg| arg == "--config")
                .and_then(|index| args.get(index + 1))
                .map(PathBuf::from)
                .ok_or(CommandRunnerError::UnexpectedInvocation)?;
            let run_root = config
                .parent()
                .and_then(Path::parent)
                .ok_or(CommandRunnerError::UnexpectedInvocation)?
                .to_path_buf();
            let mut permissions = fs::metadata(&run_root)
                .map_err(|_| CommandRunnerError::UnexpectedInvocation)?
                .permissions();
            permissions.set_mode(0o000);
            fs::set_permissions(&run_root, permissions)
                .map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
            *self
                .run_root
                .lock()
                .map_err(|_| CommandRunnerError::FakeStatePoisoned)? = Some(run_root);
        }
        Ok(output)
    }
}

struct LockMutationRunner {
    inner: RecordingRunner,
    cargo_lock: PathBuf,
}

impl CommandRunner for LockMutationRunner {
    fn run(
        &self,
        program: &Path,
        args: &[String],
        stdin: Option<&[u8]>,
        env: Option<&BTreeMap<String, String>>,
    ) -> Result<CommandOutput, CommandRunnerError> {
        let output = self.inner.run(program, args, stdin, env)?;
        if program == Path::new(CARGO_DENY) && args.iter().any(|arg| arg == "advisories") {
            fs::write(
                &self.cargo_lock,
                b"# synthetic concurrent lockfile change\n",
            )
            .map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
        }
        Ok(output)
    }
}

struct GitLaunchFailureRunner(RecordingRunner);

impl CommandRunner for GitLaunchFailureRunner {
    fn run(
        &self,
        program: &Path,
        args: &[String],
        stdin: Option<&[u8]>,
        env: Option<&BTreeMap<String, String>>,
    ) -> Result<CommandOutput, CommandRunnerError> {
        if program == Path::new(GIT) {
            return Err(CommandRunnerError::LaunchFailed);
        }
        self.0.run(program, args, stdin, env)
    }
}

#[cfg(unix)]
#[test]
fn cli_never_forwards_child_output_to_either_stream() {
    let fixture = Fixture::new("cli-redaction");
    let tools = fixture._root.path().join("synthetic-tools");
    fs::create_dir(&tools).expect("create synthetic tool directory");
    let child_env_witness = fixture._root.path().join("child-env-witness");
    for (name, body) in [
        (
            "minisign",
            format!(
                "#!/bin/sh\n{}if [ \"$1\" = -v ]; then printf 'minisign 0.12\\n'; printf '%s\\n' {CANARY} >&2; exit 0; fi\nprintf '%s\\n' {CANARY} >&2\nexit 1\n",
                child_environment_guard()
            ),
        ),
        (
            "cargo-deny",
            format!(
                "#!/bin/sh\n{}printf '%s\\n' {CANARY} >&2\nif [ \"$1\" = --version ]; then printf 'cargo-deny 0.20.2\\n'; exit 0; fi\nexit 1\n",
                child_environment_guard()
            ),
        ),
        (
            "git",
            format!(
                "#!/bin/sh\n{}printf '%s\\n' {CANARY} >&2\nexit 1\n",
                child_environment_guard()
            ),
        ),
    ] {
        write_executable_tool(&tools.join(name), &body);
    }
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("advisory-audit")
        .env("PATH", &tools)
        .env("SOLSTONE_ADVISORY_MIRROR_LOCATOR", LOCATOR)
        .env("SOLSTONE_ADVISORY_RECEIPT", &fixture.receipt)
        .env("SOLSTONE_ADVISORY_MIRROR_PUB", &fixture.public_key)
        .env("SOLSTONE_ADVISORY_BUNDLE", &fixture.bundle)
        .env("SOLSTONE_TEST_CHILD_ENV_WITNESS", &child_env_witness)
        .output()
        .expect("run advisory-audit CLI");
    assert!(!output.status.success(), "wrong production key must fail");
    assert!(
        output.stdout.is_empty(),
        "failed audit stdout must be empty"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains(CANARY));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(CANARY));
    assert!(
        !child_env_witness.exists(),
        "operator inputs must be removed from every child environment"
    );
}

#[cfg(unix)]
#[test]
fn cli_never_forwards_child_stdout_canaries() {
    let fixture = Fixture::new("cli-stdout-redaction");
    let tools = fixture._root.path().join("synthetic-stdout-tools");
    fs::create_dir(&tools).expect("create synthetic stdout tool directory");
    write_executable_tool(
        &tools.join("minisign"),
        &format!(
            "#!/bin/sh\nif [ \"$1\" = -v ]; then printf 'minisign 0.12\\n%s\\n' {CANARY}; exit 0; fi\nexit 1\n"
        ),
    );
    for name in ["cargo-deny", "git"] {
        write_executable_tool(&tools.join(name), "#!/bin/sh\nexit 1\n");
    }
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("advisory-audit")
        .env("PATH", &tools)
        .env("SOLSTONE_ADVISORY_MIRROR_LOCATOR", LOCATOR)
        .env("SOLSTONE_ADVISORY_RECEIPT", &fixture.receipt)
        .env("SOLSTONE_ADVISORY_MIRROR_PUB", &fixture.public_key)
        .env("SOLSTONE_ADVISORY_BUNDLE", &fixture.bundle)
        .output()
        .expect("run advisory-audit stdout-redaction CLI");
    assert!(
        !output.status.success(),
        "canary version output must fail safely"
    );
    assert!(
        output.stdout.is_empty(),
        "failed audit stdout must be empty"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains(CANARY));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(CANARY));
}

#[cfg(unix)]
fn write_executable_tool(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt as _;

    fs::write(path, body).expect("write synthetic child tool");
    let mut permissions = fs::metadata(path).expect("tool metadata").permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions).expect("make synthetic tool executable");
}

#[cfg(unix)]
fn child_environment_guard() -> &'static str {
    "if [ \"${SOLSTONE_ADVISORY_MIRROR_LOCATOR+x}\" = x ] || [ \"${SOLSTONE_ADVISORY_RECEIPT+x}\" = x ] || [ \"${SOLSTONE_ADVISORY_MIRROR_PUB+x}\" = x ] || [ \"${SOLSTONE_ADVISORY_BUNDLE+x}\" = x ]; then printf 'inherited\\n' >>\"$SOLSTONE_TEST_CHILD_ENV_WITNESS\"; fi\n"
}

struct WrongHeadsRunner(RecordingRunner);

impl CommandRunner for WrongHeadsRunner {
    fn run(
        &self,
        program: &Path,
        args: &[String],
        stdin: Option<&[u8]>,
        env: Option<&BTreeMap<String, String>>,
    ) -> Result<CommandOutput, CommandRunnerError> {
        let mut output = self.0.run(program, args, stdin, env)?;
        if args.iter().any(|arg| arg == "list-heads") {
            output.stdout.extend_from_slice(
                format!("{} refs/heads/synthetic-extra\n", self.0.commit).as_bytes(),
            );
        }
        Ok(output)
    }
}

#[derive(Clone, Copy)]
enum WrongHead {
    Head,
    Main,
}

struct WrongHeadCommitRunner {
    inner: RecordingRunner,
    wrong_head: WrongHead,
}

impl CommandRunner for WrongHeadCommitRunner {
    fn run(
        &self,
        program: &Path,
        args: &[String],
        stdin: Option<&[u8]>,
        env: Option<&BTreeMap<String, String>>,
    ) -> Result<CommandOutput, CommandRunnerError> {
        let mut output = self.inner.run(program, args, stdin, env)?;
        if args.iter().any(|arg| arg == "list-heads") {
            let wrong_commit = "fedcba9876543210fedcba9876543210fedcba98";
            output.stdout = match self.wrong_head {
                WrongHead::Head => format!(
                    "{wrong_commit} HEAD\n{} refs/heads/main\n",
                    self.inner.commit
                ),
                WrongHead::Main => format!(
                    "{} HEAD\n{wrong_commit} refs/heads/main\n",
                    self.inner.commit
                ),
            }
            .into_bytes();
        }
        Ok(output)
    }
}

#[derive(Eq, PartialEq)]
struct InventoryEntry {
    scope: &'static str,
    relative: PathBuf,
    kind: &'static str,
    bytes: Vec<u8>,
}

fn preserved_inventory(fixture: &Fixture) -> Vec<InventoryEntry> {
    let mut entries = Vec::new();
    let releases = fixture.checkout.join("Releases");
    let release_candidate = fixture.checkout.join("target/release-candidate");
    let release_evidence = fixture.checkout.join("target/release-evidence");
    for (scope, root) in [
        ("packet-inputs", fixture.packet_root.as_path()),
        ("source-tree", fixture.checkout.as_path()),
        ("ambient-cargo-home", fixture.ambient_cargo_home.as_path()),
        ("releases", releases.as_path()),
        ("release-candidate", release_candidate.as_path()),
        ("release-evidence", release_evidence.as_path()),
    ] {
        collect_inventory(scope, root, Path::new(""), &mut entries);
    }
    entries
        .sort_by(|left, right| (left.scope, &left.relative).cmp(&(right.scope, &right.relative)));
    entries
}

fn collect_inventory(
    scope: &'static str,
    root: &Path,
    relative: &Path,
    entries: &mut Vec<InventoryEntry>,
) {
    let path = root.join(relative);
    let metadata = fs::symlink_metadata(&path).expect("read recursive inventory metadata");
    let file_type = metadata.file_type();
    let (kind, bytes) = if file_type.is_dir() {
        ("directory", Vec::new())
    } else if file_type.is_file() {
        (
            "file",
            fs::read(&path).expect("read recursive inventory file"),
        )
    } else if file_type.is_symlink() {
        (
            "symlink",
            fs::read_link(&path)
                .expect("read recursive inventory link")
                .to_string_lossy()
                .into_owned()
                .into_bytes(),
        )
    } else {
        ("other", Vec::new())
    };
    entries.push(InventoryEntry {
        scope,
        relative: relative.to_path_buf(),
        kind,
        bytes,
    });
    if file_type.is_dir() {
        let mut children: Vec<PathBuf> = fs::read_dir(&path)
            .expect("read recursive inventory directory")
            .map(|entry| relative.join(entry.expect("read recursive inventory entry").file_name()))
            .collect();
        children.sort();
        for child in children {
            collect_inventory(scope, root, &child, entries);
        }
    }
}

fn assert_no_audit_temporary_state(checkout: &Path) {
    let entries = fs::read_dir(checkout.join("target")).expect("read target");
    assert!(entries.filter_map(Result::ok).all(|entry| {
        !entry
            .file_name()
            .to_string_lossy()
            .starts_with(".advisory-audit-")
    }));
}

fn single_line_witness(bytes: &[u8]) -> Option<AdvisoryAuditWitness> {
    let body = bytes.strip_suffix(b"\n")?;
    if body.contains(&b'\n') || body.contains(&b'\r') {
        return None;
    }
    let witness: AdvisoryAuditWitness = serde_json::from_slice(body).ok()?;
    let mut rendered = serde_json::to_vec(&witness).ok()?;
    rendered.push(b'\n');
    (rendered == bytes).then_some(witness)
}

#[test]
#[ignore = "requires real git, minisign, and cargo-deny 0.20.2; run through advisory-audit-real-tool.test.sh"]
fn real_tool_derived_name_matches_cargo_deny() {
    let git = required_test_tool("SOLSTONE_TEST_GIT");
    let minisign = required_test_tool("SOLSTONE_TEST_MINISIGN");
    let cargo_deny = required_test_tool("SOLSTONE_TEST_CARGO_DENY");
    let fixture = RealFixture::new(&git, &minisign);
    let trace_sink = required_test_path("SOLSTONE_TEST_GIT_TRACE_SINK");
    let witness_sink = required_test_path("SOLSTONE_TEST_WITNESS_SINK");
    fs::write(&trace_sink, b"").expect("reset synthetic Git trace sink");
    let runner = CargoHomeRunner {
        inner: RemovedEnvironmentProcessCommandRunner::new(
            &xtask::advisory_audit::ADVISORY_AUDIT_REMOVED_ENV,
        ),
        cargo_home: fixture.ambient_cargo_home.clone(),
    };
    let request = AdvisoryAuditRequest {
        checkout_root: &fixture.checkout,
        locator: LOCATOR,
        receipt_path: &fixture.receipt,
        public_key_path: &fixture.public_key,
        bundle_path: &fixture.bundle,
        programs: AdvisoryAuditPrograms {
            git: &git,
            minisign: &minisign,
            cargo_deny: &cargo_deny,
        },
        trust: AdvisoryAuditTrust {
            public_key_sha256: &fixture.public_key_sha256,
            public_key_id: &fixture.public_key_id,
        },
    };
    let witness_bytes =
        run_advisory_audit(&request, &runner, &FixedClock::new(NOW).expect("clock"))
            .expect("real cargo-deny accepts the product-derived database location");
    let witness = single_line_witness(&witness_bytes).expect("parse single-line real-tool witness");
    fs::write(&witness_sink, &witness_bytes).expect("write real-tool witness sink");
    assert_eq!(witness.verdict, "pass");
    assert!(
        fs::read(&trace_sink)
            .expect("read synthetic Git trace sink")
            .is_empty(),
        "audit child wrote to the removed Git trace sink"
    );
    fixture.assert_decoy_database_rejects(&cargo_deny);
    let mut thin_request = request.clone();
    thin_request.bundle_path = &fixture.thin_bundle;
    assert_eq!(
        run_advisory_audit(
            &thin_request,
            &runner,
            &FixedClock::new(NOW).expect("clock"),
        )
        .expect_err("empty verification repository rejects thin bundle"),
        AuditError::BundleVerificationFailed
    );
    fixture.assert_wrong_database_name_fails(&git, &cargo_deny);
    assert!(
        fs::read(trace_sink)
            .expect("read final synthetic Git trace sink")
            .is_empty(),
        "audit child wrote to the removed Git trace sink"
    );
}

struct CargoHomeRunner<R> {
    inner: R,
    cargo_home: PathBuf,
}

impl<R: CommandRunner> CommandRunner for CargoHomeRunner<R> {
    fn run(
        &self,
        program: &Path,
        args: &[String],
        stdin: Option<&[u8]>,
        env: Option<&BTreeMap<String, String>>,
    ) -> Result<CommandOutput, CommandRunnerError> {
        let mut child_env = env.cloned().unwrap_or_default();
        child_env.insert(
            "CARGO_HOME".to_owned(),
            self.cargo_home.to_string_lossy().into_owned(),
        );
        self.inner.run(program, args, stdin, Some(&child_env))
    }
}

struct RealFixture {
    _root: tempfile::TempDir,
    checkout: PathBuf,
    receipt: PathBuf,
    public_key: PathBuf,
    bundle: PathBuf,
    thin_bundle: PathBuf,
    public_key_sha256: String,
    public_key_id: String,
    repository: PathBuf,
    ambient_cargo_home: PathBuf,
}

impl RealFixture {
    fn new(git: &Path, minisign: &Path) -> Self {
        let root = tempfile::Builder::new()
            .prefix("synthetic-advisory-real-tool-")
            .tempdir()
            .expect("create real-tool fixture");
        let repository = root.path().join("repository");
        fs::create_dir(&repository).expect("create repository");
        command_ok(git, &repository, &["init", "-b", "main"]);
        command_ok(
            git,
            &repository,
            &["config", "user.email", "synthetic@example.invalid"],
        );
        command_ok(
            git,
            &repository,
            &["config", "user.name", "synthetic advisory test"],
        );
        fs::write(
            repository.join("synthetic-base.txt"),
            b"Synthetic bundle prerequisite.\n",
        )
        .expect("write bundle base");
        command_ok(git, &repository, &["add", "--", "synthetic-base.txt"]);
        command_ok(
            git,
            &repository,
            &["commit", "--no-gpg-sign", "-m", "synthetic bundle base"],
        );
        fs::create_dir_all(repository.join("crates/synthetic")).expect("create advisory crates");
        fs::write(
            repository.join("crates/synthetic/RUSTSEC-2099-0001.md"),
            b"# Synthetic advisory\n\n```toml\n[advisory]\nid = \"RUSTSEC-2099-0001\"\npackage = \"synthetic-unrelated-crate\"\ndate = \"2026-07-23\"\nurl = \"https://advisory.example.invalid/synthetic\"\n\n[versions]\npatched = [\">= 1.0.0\"]\n```\n",
        )
        .expect("write advisory marker");
        command_ok(
            git,
            &repository,
            &["add", "--", "crates/synthetic/RUSTSEC-2099-0001.md"],
        );
        command_ok(
            git,
            &repository,
            &[
                "commit",
                "--no-gpg-sign",
                "-m",
                "synthetic advisory database",
            ],
        );

        let ambient_cargo_home = root.path().join("ambient-cargo-home");
        let ambient_database_root = ambient_cargo_home.join("advisory-dbs");
        let decoy_repository = ambient_database_root.join(cargo_deny_database_name(LOCATOR));
        fs::create_dir_all(&decoy_repository).expect("create synthetic decoy database");
        command_ok(git, &decoy_repository, &["init", "-b", "main"]);
        command_ok(
            git,
            &decoy_repository,
            &["config", "user.email", "synthetic@example.invalid"],
        );
        command_ok(
            git,
            &decoy_repository,
            &["config", "user.name", "synthetic advisory decoy"],
        );
        fs::create_dir_all(decoy_repository.join("crates/synthetic"))
            .expect("create synthetic decoy advisory crates");
        fs::write(
            decoy_repository.join("crates/synthetic/RUSTSEC-2099-0002.md"),
            b"# Synthetic malformed decoy advisory\n\n```toml\n[advisory]\nid = \"RUSTSEC-2099-0002\"\npackage = \"synthetic-advisory-consumer\"\ndate = \"not-a-date\"\nurl = \"https://decoy-advisory.example.invalid/synthetic\"\n\n[versions]\npatched = [\">= 1.0.0\"]\n```\n",
        )
        .expect("write synthetic decoy advisory");
        command_ok(git, &decoy_repository, &["add", "--", "."]);
        command_ok(
            git,
            &decoy_repository,
            &["commit", "--no-gpg-sign", "-m", "synthetic decoy database"],
        );
        let commit = command_stdout(git, &repository, &["rev-parse", "HEAD"]);
        let bundle = root.path().join("synthetic-advisory.bundle");
        command_ok(
            git,
            &repository,
            &[
                "bundle",
                "create",
                bundle.to_str().expect("bundle path"),
                "HEAD",
                "refs/heads/main",
            ],
        );
        let thin_bundle = root.path().join("synthetic-thin-advisory.bundle");
        command_ok(
            git,
            &repository,
            &[
                "bundle",
                "create",
                thin_bundle.to_str().expect("thin bundle path"),
                "HEAD",
                "refs/heads/main",
                "^HEAD~1",
            ],
        );

        let checkout = root.path().join("consumer");
        fs::create_dir(&checkout).expect("create consumer");
        write_consumer(&checkout);
        let receipt = root.path().join("freshness.json");
        fs::write(&receipt, canonical_freshness_body(&commit, NOW, 86_400))
            .expect("write real receipt");
        let public_key = root.path().join("synthetic.pub");
        let secret_key = root.path().join("synthetic.key");
        let keygen = Command::new(minisign)
            .args(["-G", "-W", "-p"])
            .arg(&public_key)
            .arg("-s")
            .arg(&secret_key)
            .output()
            .expect("run minisign keygen");
        assert!(keygen.status.success(), "synthetic minisign keygen failed");
        let public_key_bytes = fs::read(&public_key).expect("read generated public key");
        let public_key_text = String::from_utf8_lossy(&public_key_bytes);
        let public_key_payload = public_key_text
            .lines()
            .nth(1)
            .expect("generated minisign public-key payload");
        let decoded_public_key = base64::engine::general_purpose::STANDARD
            .decode(public_key_payload)
            .expect("decode generated minisign public key");
        let public_key_id = format!(
            "{:016X}",
            u64::from_le_bytes(
                decoded_public_key[2..10]
                    .try_into()
                    .expect("generated minisign key-ID bytes"),
            )
        );
        let signature = receipt.with_extension("json.minisig");
        let sign = Command::new(minisign)
            .arg("-S")
            .arg("-s")
            .arg(&secret_key)
            .arg("-m")
            .arg(&receipt)
            .arg("-x")
            .arg(&signature)
            .arg("-t")
            .arg(format_advisory_mirror_trusted_comment(&commit, NOW, 86_400))
            .output()
            .expect("run minisign signer");
        assert!(sign.status.success(), "synthetic minisign signing failed");
        Self {
            _root: root,
            checkout,
            receipt,
            public_key,
            bundle,
            thin_bundle,
            public_key_sha256: format!("{:x}", Sha256::digest(&public_key_bytes)),
            public_key_id,
            repository,
            ambient_cargo_home,
        }
    }

    fn assert_decoy_database_rejects(&self, cargo_deny: &Path) {
        let config = self._root.path().join("decoy-deny.toml");
        let bytes = xtask::release_advisory::render_advisory_config(
            &fs::read(self.checkout.join("deny.toml")).expect("read policy"),
            &self.ambient_cargo_home.join("advisory-dbs"),
            LOCATOR,
        )
        .expect("render decoy config");
        fs::write(&config, bytes).expect("write decoy config");
        let mut command = Command::new(cargo_deny);
        scrub_setup_git_environment(&mut command);
        let output = command
            .arg("--manifest-path")
            .arg(self.checkout.join("Cargo.toml"))
            .arg("--locked")
            .arg("--offline")
            .arg("--config")
            .arg(config)
            .args(["check", "advisories"])
            .env("CARGO_HOME", &self.ambient_cargo_home)
            .env("CARGO_NET_OFFLINE", "true")
            .output()
            .expect("run decoy cargo-deny check");
        assert!(
            !output.status.success(),
            "ambient decoy database must produce a rejecting verdict"
        );
    }

    fn assert_wrong_database_name_fails(&self, git: &Path, cargo_deny: &Path) {
        let wrong_root = self.checkout.join("wrong-name-check");
        let database_root = wrong_root.join("database");
        let wrong_checkout = database_root.join("synthetic-wrong-cache-name");
        fs::create_dir_all(&database_root).expect("create wrong-name database root");
        command_ok(
            git,
            &self.repository,
            &[
                "clone",
                "--no-tags",
                self.bundle.to_str().expect("bundle path"),
                wrong_checkout.to_str().expect("wrong checkout path"),
            ],
        );
        let config = wrong_root.join("deny.toml");
        let bytes = xtask::release_advisory::render_advisory_config(
            &fs::read(self.checkout.join("deny.toml")).expect("read policy"),
            &database_root,
            LOCATOR,
        )
        .expect("render wrong-name config");
        fs::write(&config, bytes).expect("write wrong-name config");
        let mut command = Command::new(cargo_deny);
        scrub_setup_git_environment(&mut command);
        let output = command
            .arg("--manifest-path")
            .arg(self.checkout.join("Cargo.toml"))
            .arg("--locked")
            .arg("--offline")
            .arg("--config")
            .arg(&config)
            .args(["check", "advisories"])
            .env("CARGO_NET_OFFLINE", "true")
            .output()
            .expect("run wrong-name cargo-deny");
        assert!(
            !output.status.success(),
            "real cargo-deny must reject an incorrectly named offline database"
        );
        fs::remove_dir_all(wrong_root).expect("remove wrong-name fixture");
    }
}

fn write_consumer(root: &Path) {
    fs::create_dir(root.join("target")).expect("create consumer target");
    fs::create_dir(root.join("src")).expect("create consumer source directory");
    fs::write(
        root.join("src/lib.rs"),
        b"// Synthetic advisory-audit acceptance fixture.\n",
    )
    .expect("write consumer source");
    fs::write(
        root.join("Cargo.toml"),
        b"[package]\nname = \"synthetic-advisory-consumer\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .expect("write consumer manifest");
    fs::write(
        root.join("Cargo.lock"),
        b"# This file is automatically @generated by Cargo.\n# It is not intended for manual editing.\nversion = 4\n\n[[package]]\nname = \"synthetic-advisory-consumer\"\nversion = \"0.0.0\"\n",
    )
    .expect("write consumer lockfile");
    fs::write(
        root.join("deny.toml"),
        b"[advisories]\nyanked = \"warn\"\nunmaintained = \"workspace\"\nignore = [\n  { id = \"RUSTSEC-2026-0194\", reason = \"Synthetic policy fixture.\" },\n  { id = \"RUSTSEC-2026-0195\", reason = \"Synthetic policy fixture.\" },\n]\n",
    )
    .expect("write consumer policy");
}

fn required_test_tool(name: &str) -> PathBuf {
    let value = std::env::var_os(name).expect("real-tool script exports tool path");
    let path = PathBuf::from(value);
    assert!(
        path.is_absolute() && path.is_file(),
        "test tool must be absolute"
    );
    path
}

fn required_test_path(name: &str) -> PathBuf {
    let value = std::env::var_os(name).expect("real-tool script exports test path");
    let path = PathBuf::from(value);
    assert!(path.is_absolute(), "test path must be absolute");
    path
}

fn cargo_deny_database_name(locator: &str) -> String {
    let first = Url::parse(locator).expect("synthetic locator parses");
    let second = Url::parse(&first.as_str().to_lowercase()).expect("lowercase locator parses");
    let last = second
        .path_segments()
        .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
        .expect("synthetic locator has a final segment");
    let hash = XxHash64::oneshot(0xca80de71, second.as_str().as_bytes());
    format!("{last}-{hash:016x}")
}

fn command_ok(program: &Path, root: &Path, args: &[&str]) {
    let mut command = Command::new(program);
    scrub_setup_git_environment(&mut command);
    let output = command
        .args(args)
        .current_dir(root)
        .output()
        .expect("run synthetic setup command");
    assert!(output.status.success(), "synthetic setup command failed");
}

fn command_stdout(program: &Path, root: &Path, args: &[&str]) -> String {
    let mut command = Command::new(program);
    scrub_setup_git_environment(&mut command);
    let output = command
        .args(args)
        .current_dir(root)
        .output()
        .expect("run synthetic setup command");
    assert!(output.status.success(), "synthetic setup command failed");
    String::from_utf8(output.stdout)
        .expect("synthetic command output UTF-8")
        .trim()
        .to_owned()
}

fn scrub_setup_git_environment(command: &mut Command) {
    for name in xtask::advisory_audit::ADVISORY_AUDIT_REMOVED_ENV {
        command.env_remove(name);
    }
}
