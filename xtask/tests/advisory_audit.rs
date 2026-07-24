// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use base64::Engine as _;
use sha2::{Digest, Sha256};
use xtask::advisory_audit::{
    run_advisory_audit, AdvisoryAuditPrograms, AdvisoryAuditRequest, AdvisoryAuditTrust,
    AdvisoryAuditWitness, AuditError, CARGO_DENY_VERSION,
};
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
    let witness: AdvisoryAuditWitness = serde_json::from_slice(&bytes).expect("parse witness");
    assert!(bytes.ends_with(b"\n"), "witness must end in one newline");
    assert_eq!(witness.schema, "solstone.advisory-audit.v1");
    assert_eq!(witness.product, "solstone-windows");
    assert_eq!(witness.synced_commit, fixture.commit);
    assert_eq!(witness.receipt_utc, NOW);
    assert_eq!(witness.max_age, 86_400);
    assert_eq!(witness.checked_at, NOW);
    assert_eq!(witness.cargo_deny_version, CARGO_DENY_VERSION);
    assert_eq!(witness.verdict, "pass");
    assert!(!String::from_utf8_lossy(&bytes).contains(CANARY));
    assert_eq!(preserved_inventory(&fixture), before);
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
    assert!(!invocations.iter().any(|invocation| {
        invocation
            .args
            .iter()
            .any(|arg| matches!(arg.as_str(), "fetch" | "pull" | "ls-remote"))
    }));
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
    for locator in [
        "https://synthetic-user@mirror.example.invalid:8443/advisory-db",
        "ssh://synthetic-user@mirror.example.invalid:2222/rustsec-advisory-db.git",
    ] {
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

#[cfg(unix)]
#[test]
fn cli_never_forwards_child_output_to_either_stream() {
    use std::os::unix::fs::PermissionsExt as _;

    let fixture = Fixture::new("cli-redaction");
    let tools = fixture._root.path().join("synthetic-tools");
    fs::create_dir(&tools).expect("create synthetic tool directory");
    for (name, body) in [
        (
            "minisign",
            format!(
                "#!/bin/sh\nif [ \"$1\" = -v ]; then printf 'minisign 0.12\\n'; printf '%s\\n' {CANARY} >&2; exit 0; fi\nprintf '%s\\n' {CANARY} >&2\nexit 1\n"
            ),
        ),
        (
            "cargo-deny",
            format!(
                "#!/bin/sh\nprintf '%s\\n' {CANARY} >&2\nif [ \"$1\" = --version ]; then printf 'cargo-deny 0.20.2\\n'; exit 0; fi\nexit 1\n"
            ),
        ),
        (
            "git",
            format!("#!/bin/sh\nprintf '%s\\n' {CANARY} >&2\nexit 1\n"),
        ),
    ] {
        let path = tools.join(name);
        fs::write(&path, body).expect("write synthetic child tool");
        let mut permissions = fs::metadata(&path).expect("tool metadata").permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(path, permissions).expect("make synthetic tool executable");
    }
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("advisory-audit")
        .env("PATH", &tools)
        .env("SOLSTONE_ADVISORY_MIRROR_LOCATOR", LOCATOR)
        .env("SOLSTONE_ADVISORY_RECEIPT", &fixture.receipt)
        .env("SOLSTONE_ADVISORY_MIRROR_PUB", &fixture.public_key)
        .env("SOLSTONE_ADVISORY_BUNDLE", &fixture.bundle)
        .output()
        .expect("run advisory-audit CLI");
    assert!(!output.status.success(), "wrong production key must fail");
    assert!(
        output.stdout.is_empty(),
        "failed audit stdout must be empty"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains(CANARY));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(CANARY));
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

fn preserved_inventory(fixture: &Fixture) -> Vec<Vec<u8>> {
    [
        fixture.receipt.clone(),
        fixture.receipt.with_extension("json.minisig"),
        fixture.public_key.clone(),
        fixture.bundle.clone(),
        fixture.checkout.join("Cargo.toml"),
        fixture.checkout.join("Cargo.lock"),
        fixture.checkout.join("deny.toml"),
        fixture.checkout.join("src/lib.rs"),
        fixture.ambient_cargo_home.join("sentinel"),
        fixture.checkout.join("Releases/sentinel"),
        fixture.checkout.join("target/release-candidate/sentinel"),
        fixture.checkout.join("target/release-evidence/sentinel"),
    ]
    .iter()
    .map(|path| fs::read(path).expect("read preserved input"))
    .collect()
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

#[test]
#[ignore = "requires real git, minisign, and cargo-deny 0.20.2; run through advisory-audit-real-tool.test.sh"]
fn real_tool_derived_name_matches_cargo_deny() {
    let git = required_test_tool("SOLSTONE_TEST_GIT");
    let minisign = required_test_tool("SOLSTONE_TEST_MINISIGN");
    let cargo_deny = required_test_tool("SOLSTONE_TEST_CARGO_DENY");
    let fixture = RealFixture::new(&git, &minisign);
    let runner = RemovedEnvironmentProcessCommandRunner::new(
        &xtask::advisory_audit::ADVISORY_AUDIT_REMOVED_ENV,
    );
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
    let witness = run_advisory_audit(&request, &runner, &FixedClock::new(NOW).expect("clock"))
        .expect("real cargo-deny accepts the product-derived database location");
    assert_eq!(
        serde_json::from_slice::<AdvisoryAuditWitness>(&witness)
            .expect("parse real-tool witness")
            .verdict,
        "pass"
    );
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
        let public_key_id = String::from_utf8_lossy(&public_key_bytes)
            .lines()
            .find_map(|line| {
                line.strip_prefix("untrusted comment: minisign public key ")
                    .filter(|value| {
                        value.len() == 16
                            && value
                                .bytes()
                                .all(|byte| byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte))
                    })
            })
            .expect("generated minisign key ID")
            .to_owned();
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
        }
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
        let output = Command::new(cargo_deny)
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
