// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::fs::{self, FileTimes};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};
use xtask::artifact_fs::child_process_path_text;
use xtask::release_advisory::{
    canonical_freshness_body, format_advisory_mirror_trusted_comment,
    materialize_advisory_config_at, render_advisory_config, run_advisory_check,
    validate_mirror_locator, AdvisoryError, AdvisorySnapshot, MirrorPacketInputs,
    ADVISORY_DB_RELATIVE, MIRROR_COHORT_ID,
};
use xtask::release_clock::{FixedClock, UtcTimestamp};
use xtask::release_exec::test_support::{FakeCommand, FakeCommandRunner};
use xtask::release_exec::CommandOutput;
use xtask::release_selection::SelectedAction;

const VERSION: &str = "0.2.11";
const NOW: &str = "2026-07-21T12:00:00Z";
const FRESH: &str = "2026-07-21T00:00:00Z";
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const LOCATOR: &str = "https://private-token@mirror.example.invalid/advisory-db";
const REPOSITORY: &str = "advisory-db-a5a5a5a5a5a5a5a5";
const FAKE_PUBLIC_KEY: &[u8] = b"untrusted comment: fake mirror test key\nRWQFAKEMIRRORKEY\n";
#[cfg(not(windows))]
const GIT: &str = "/selected/git";
#[cfg(windows)]
const GIT: &str = r"C:\selected\git.exe";
#[cfg(not(windows))]
const MINISIGN: &str = "/selected/minisign";
#[cfg(windows)]
const MINISIGN: &str = r"C:\selected\minisign.exe";
#[cfg(not(windows))]
const CARGO: &str = "/selected/cargo.exe";
#[cfg(windows)]
const CARGO: &str = r"C:\selected\cargo.exe";
#[cfg(not(windows))]
const ISOLATED_ADVISORY_DB: &str = "/isolated/advisory-db";
#[cfg(windows)]
const ISOLATED_ADVISORY_DB: &str = r"C:\isolated\advisory-db";
const ARCHIVE: &[u8] = b"deterministic git archive bytes";

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TestCheckout {
    root: PathBuf,
    freshness_receipt: PathBuf,
    mirror_public_key: PathBuf,
}

impl TestCheckout {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "solstone-release-advisory-{label}-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("create test checkout");
        fs::write(
            root.join("deny.toml"),
            fs::read(workspace_root().join("deny.toml")).expect("read committed deny.toml"),
        )
        .expect("write test deny.toml");
        fs::create_dir_all(root.join(format!("target/release-finalizer/{VERSION}")))
            .expect("create transaction root");
        let packet = root.join("mirror-packet");
        fs::create_dir(&packet).expect("create mirror packet directory");
        let freshness_receipt = packet.join("freshness.json");
        let mirror_public_key = packet.join("mirror.pub");
        fs::write(
            &freshness_receipt,
            canonical_freshness_body(COMMIT, NOW, 86_400),
        )
        .expect("write freshness body");
        let trusted = format_advisory_mirror_trusted_comment(COMMIT, NOW, 86_400);
        fs::write(
            freshness_receipt.with_file_name("freshness.json.minisig"),
            format!("untrusted comment: fake signature\nAAAA\ntrusted comment: {trusted}\nBBBB\n"),
        )
        .expect("write freshness signature");
        fs::write(&mirror_public_key, FAKE_PUBLIC_KEY).expect("write fake public key");
        let checkout = Self {
            root,
            freshness_receipt,
            mirror_public_key,
        };
        checkout.create_repository(REPOSITORY, FRESH);
        checkout
    }

    fn create_repository(&self, name: &str, acquired_at: &str) {
        let repo = self.root.join(ADVISORY_DB_RELATIVE).join(name);
        fs::create_dir_all(repo.join(".git")).expect("create fake advisory git dir");
        fs::write(repo.join(".git/HEAD"), b"ref: refs/heads/main\n").expect("write HEAD");
        fs::write(repo.join(".git/FETCH_HEAD"), b"fetch evidence\n").expect("write FETCH_HEAD");
        fs::write(repo.join("README.md"), b"RustSec snapshot\n").expect("write repo file");
        self.set_fetch_time(name, acquired_at);
    }

    fn set_fetch_time(&self, name: &str, acquired_at: &str) {
        let modified = UtcTimestamp::parse(acquired_at)
            .expect("parse test acquisition")
            .system_time();
        fs::OpenOptions::new()
            .write(true)
            .open(
                self.root
                    .join(ADVISORY_DB_RELATIVE)
                    .join(name)
                    .join(".git/FETCH_HEAD"),
            )
            .expect("open FETCH_HEAD")
            .set_times(FileTimes::new().set_modified(modified))
            .expect("set FETCH_HEAD mtime");
    }

    fn repository_path(&self) -> PathBuf {
        let canonical = fs::canonicalize(self.root.join(ADVISORY_DB_RELATIVE).join(REPOSITORY))
            .expect("canonicalize fake repository");
        PathBuf::from(
            child_process_path_text(&canonical).expect("child-process fake repository path"),
        )
    }

    fn config_path(&self) -> PathBuf {
        let config = fs::canonicalize(&self.root)
            .expect("canonicalize fake checkout")
            .join(format!("target/release-finalizer/{VERSION}"))
            .join("advisory")
            .join("deny.toml");
        PathBuf::from(child_process_path_text(&config).expect("child-process fake config path"))
    }

    fn mirror_inputs(&self) -> MirrorPacketInputs<'_> {
        MirrorPacketInputs {
            locator: LOCATOR,
            receipt_path: &self.freshness_receipt,
            public_key_path: &self.mirror_public_key,
            minisign_program: Path::new(MINISIGN),
            expected_public_key_sha256: fake_public_key_sha256(),
        }
    }

    fn signature_path(&self) -> PathBuf {
        self.freshness_receipt
            .with_file_name("freshness.json.minisig")
    }
}

impl Drop for TestCheckout {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove test checkout");
    }
}

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has workspace parent")
}

fn tree_sha256() -> String {
    Sha256::digest(ARCHIVE)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn fake_public_key_sha256() -> &'static str {
    static DIGEST: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DIGEST.get_or_init(|| {
        Sha256::digest(FAKE_PUBLIC_KEY)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    })
}

fn output(status: i32, stdout: &[u8]) -> CommandOutput {
    CommandOutput {
        status,
        stdout: stdout.to_vec(),
        stderr: Vec::new(),
    }
}

fn git_command(checkout: &TestCheckout, tail: &[&str], status: i32, stdout: &[u8]) -> FakeCommand {
    let mut args = vec![
        "-C".to_owned(),
        checkout
            .repository_path()
            .to_str()
            .expect("utf8 fake repo")
            .to_owned(),
    ];
    args.extend(tail.iter().map(|arg| (*arg).to_owned()));
    FakeCommand::output(PathBuf::from(GIT), args, output(status, stdout))
}

fn snapshot_commands(checkout: &TestCheckout) -> Vec<FakeCommand> {
    vec![
        git_command(
            checkout,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignored",
            ],
            0,
            b"",
        ),
        git_command(
            checkout,
            &["remote", "get-url", "origin"],
            0,
            format!("{LOCATOR}\n").as_bytes(),
        ),
        git_command(
            checkout,
            &["rev-parse", "HEAD^{commit}"],
            0,
            format!("{COMMIT}\n").as_bytes(),
        ),
        git_command(
            checkout,
            &["rev-parse", "--is-shallow-repository"],
            0,
            b"false\n",
        ),
        git_command(checkout, &["archive", "--format=tar", "HEAD"], 0, ARCHIVE),
    ]
}

fn minisign_command(checkout: &TestCheckout, status: i32) -> FakeCommand {
    let scratch = fs::canonicalize(&checkout.root)
        .expect("canonical checkout")
        .join(format!(
            "target/release-finalizer/{VERSION}/.advisory-mirror-verify"
        ));
    FakeCommand::output(
        PathBuf::from(MINISIGN),
        vec![
            "-V".to_owned(),
            "-p".to_owned(),
            child_process_path_text(&scratch.join("mirror.pub")).expect("fake public key path"),
            "-m".to_owned(),
            child_process_path_text(&scratch.join("freshness.json")).expect("fake body path"),
            "-x".to_owned(),
            child_process_path_text(&scratch.join("freshness.json.minisig"))
                .expect("fake signature path"),
        ],
        output(status, b""),
    )
}

fn advisory_action() -> SelectedAction {
    SelectedAction {
        program: PathBuf::from(CARGO),
        argv: [
            "deny",
            "--locked",
            "--offline",
            "--config",
            "{advisory_config}",
            "check",
            "advisories",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
    }
}

fn cargo_command(checkout: &TestCheckout, status: i32) -> FakeCommand {
    FakeCommand::output(
        PathBuf::from(CARGO),
        vec![
            "deny".to_owned(),
            "--locked".to_owned(),
            "--offline".to_owned(),
            "--config".to_owned(),
            checkout
                .config_path()
                .to_str()
                .expect("utf8 config path")
                .to_owned(),
            "check".to_owned(),
            "advisories".to_owned(),
        ],
        output(status, b""),
    )
}

fn write_packet(checkout: &TestCheckout, commit: &str, utc: &str, max_age: u64) {
    fs::write(
        &checkout.freshness_receipt,
        canonical_freshness_body(commit, utc, max_age),
    )
    .expect("write test freshness body");
    write_trusted_comment(
        checkout,
        &format_advisory_mirror_trusted_comment(commit, utc, max_age),
    );
}

fn write_trusted_comment(checkout: &TestCheckout, trusted_comment: &str) {
    fs::write(
        checkout.signature_path(),
        format!(
            "untrusted comment: private signature canary\nAAAA\ntrusted comment: {trusted_comment}\nBBBB\n"
        ),
    )
    .expect("write test trusted comment");
}

fn run_check(
    checkout: &TestCheckout,
    commands: Vec<FakeCommand>,
    now: &str,
) -> Result<xtask::release_advisory::AdvisoryProvenance, AdvisoryError> {
    run_advisory_check(
        &checkout.root,
        VERSION,
        Path::new(GIT),
        &tree_sha256(),
        &advisory_action(),
        &checkout.mirror_inputs(),
        &FakeCommandRunner::new(commands),
        &FixedClock::new(now).expect("create check clock"),
    )
}

#[test]
fn deterministic_advisory_config_is_byte_exact() {
    let deny = fs::read(workspace_root().join("deny.toml")).expect("read deny.toml");
    let database = Path::new(ISOLATED_ADVISORY_DB);
    let expected = concat!(
        "[advisories]\n",
        "db-path = \"__ISOLATED_ADVISORY_DB__\"\n",
        "db-urls = [\"https://private-token@mirror.example.invalid/advisory-db\"]\n",
        "yanked = \"warn\"\n",
        "unmaintained = \"workspace\"\n",
        "ignore = [\n",
        "  { id = \"RUSTSEC-2026-0194\", reason = \"quick-xml 0.39.4 O(N^2) attribute dup-check DoS; transitive via plist<-Tauri; no upstream release with the >=0.41 fix yet (plist 1.9.0 pins ^0.39.2). Remove once plist bumps quick-xml. Owner: VPE.\" },\n",
        "  { id = \"RUSTSEC-2026-0195\", reason = \"quick-xml 0.39.4 unbounded namespace-decl growth DoS; transitive via plist<-Tauri; no upstream release with the >=0.41 fix yet (plist 1.9.0 pins ^0.39.2). Remove once plist bumps quick-xml. Owner: VPE.\" },\n",
        "]\n"
    )
    .replace(
        "__ISOLATED_ADVISORY_DB__",
        &ISOLATED_ADVISORY_DB.replace('\\', "\\\\"),
    )
    .into_bytes();

    let first = render_advisory_config(&deny, database, LOCATOR).expect("render config");
    let second = render_advisory_config(&deny, database, LOCATOR).expect("render config again");
    assert_eq!(first, expected);
    assert_eq!(second, expected);
}

#[test]
fn mirror_locator_validation_rejects_public_or_malformed_sources() {
    validate_mirror_locator(LOCATOR).expect("accept credential-bearing private locator");
    for locator in [
        "",
        " ",
        " https://mirror.example.invalid/advisory-db",
        "https://mirror.example.invalid/advisory-db ",
        "https://mirror.example.invalid/advisory\n-db",
        "https://mirror.example.invalid/not-advisory-db",
        "https://mirror.example.invalid/advisory-db/",
    ] {
        assert_eq!(
            validate_mirror_locator(locator).expect_err("malformed locator must fail"),
            AdvisoryError::MirrorLocatorInvalid,
            "locator case {locator:?}"
        );
    }
    for locator in [
        "https://github.com/RustSec/advisory-db",
        "HTTP://GITHUB.COM/rustsec/advisory-db.git/",
        "git://github.com/RustSec/advisory-db",
        "ssh://git@github.com/RustSec/advisory-db.git",
        "git@github.com:RustSec/advisory-db",
        "github.com:rustsec/advisory-db.git/",
    ] {
        assert_eq!(
            validate_mirror_locator(locator).expect_err("public source must fail"),
            AdvisoryError::PublicRustsecSourceForbidden,
            "locator case {locator:?}"
        );
    }
}

#[test]
fn deployed_freshness_protocol_rendering_is_byte_exact() {
    assert_eq!(
        canonical_freshness_body(COMMIT, NOW, 86_400),
        format!("{{\"max_age\":86400,\"synced_commit\":\"{COMMIT}\",\"utc\":\"{NOW}\"}}\n")
            .into_bytes()
    );
    assert_eq!(
        format_advisory_mirror_trusted_comment(COMMIT, NOW, 86_400),
        format!("solpbc-advisory-mirror-v1 synced_commit={COMMIT} utc={NOW} max_age=86400")
    );
}

#[test]
fn ci_materializer_uses_the_canonical_bytes_and_refuses_overwrite() {
    let checkout = TestCheckout::new("ci-materializer");
    let output_dir = checkout.root.join("release-advisory-config-check");
    fs::create_dir(&output_dir).expect("create config-check directory");
    let output = fs::canonicalize(output_dir)
        .expect("canonicalize config-check directory")
        .join("deny.toml");
    let database = fs::canonicalize(checkout.root.join(ADVISORY_DB_RELATIVE))
        .expect("canonicalize isolated database root");
    let expected = render_advisory_config(
        &fs::read(checkout.root.join("deny.toml")).expect("read deny.toml"),
        &database,
        LOCATOR,
    )
    .expect("render expected policy");

    let materialized = materialize_advisory_config_at(&checkout.root, &database, &output, LOCATOR)
        .expect("materialize CI advisory config");
    assert_eq!(materialized.bytes, expected);
    assert_eq!(
        fs::read(&output).expect("read materialized policy"),
        expected
    );
    assert_eq!(materialized.database_root, database);
    assert_eq!(materialized.path, output);
    assert_eq!(
        materialize_advisory_config_at(&checkout.root, &database, &materialized.path, LOCATOR)
            .expect_err("existing output must refuse overwrite"),
        AdvisoryError::ConfigMaterializationFailed
    );
}

#[test]
fn clean_full_fresh_snapshot_passes_with_public_provenance() {
    let checkout = TestCheckout::new("snapshot-pass");
    let runner = FakeCommandRunner::new(snapshot_commands(&checkout));
    let clock = FixedClock::new(NOW).expect("create fixed clock");
    let snapshot = AdvisorySnapshot::inspect(
        &checkout.root,
        Path::new(GIT),
        &tree_sha256(),
        LOCATOR,
        &runner,
        &clock,
    )
    .expect("inspect fresh snapshot");

    assert_eq!(snapshot.source_id, MIRROR_COHORT_ID);
    assert_eq!(snapshot.commit, COMMIT);
    assert_eq!(snapshot.tree_sha256, tree_sha256());
    assert_eq!(snapshot.acquired_at, FRESH);
    assert_eq!(clock.calls(), 1);
    assert_eq!(runner.remaining().expect("read fake queue"), 0);
}

#[test]
fn regular_database_lock_is_tolerated_but_a_foreign_child_is_not() {
    let checkout = TestCheckout::new("snapshot-db-lock");
    fs::write(
        checkout.root.join(ADVISORY_DB_RELATIVE).join("db.lock"),
        b"cargo-deny lock",
    )
    .expect("write regular db.lock");
    let runner = FakeCommandRunner::new(snapshot_commands(&checkout));
    AdvisorySnapshot::inspect(
        &checkout.root,
        Path::new(GIT),
        &tree_sha256(),
        LOCATOR,
        &runner,
        &FixedClock::new(NOW).expect("clock"),
    )
    .expect("regular db.lock beside the repository is tolerated");
    assert_eq!(runner.remaining().expect("read fake queue"), 0);

    let checkout = TestCheckout::new("snapshot-foreign-child");
    fs::write(
        checkout
            .root
            .join(ADVISORY_DB_RELATIVE)
            .join("foreign-child"),
        b"foreign",
    )
    .expect("write foreign child");
    assert_eq!(
        AdvisorySnapshot::inspect(
            &checkout.root,
            Path::new(GIT),
            &tree_sha256(),
            LOCATOR,
            &FakeCommandRunner::new(Vec::new()),
            &FixedClock::new(NOW).expect("clock"),
        )
        .expect_err("foreign second child must fail"),
        AdvisoryError::RepositoryCount
    );
}

#[test]
fn source_repo_count_name_and_archive_identity_fail_closed() {
    let checkout = TestCheckout::new("source-mismatch");
    let mut commands = snapshot_commands(&checkout);
    commands.truncate(2);
    commands[1] = git_command(
        &checkout,
        &["remote", "get-url", "origin"],
        0,
        b"https://example.invalid/advisory-db\n",
    );
    let runner = FakeCommandRunner::new(commands);
    let clock = FixedClock::new(NOW).expect("clock");
    assert_eq!(
        AdvisorySnapshot::inspect(
            &checkout.root,
            Path::new(GIT),
            &tree_sha256(),
            LOCATOR,
            &runner,
            &clock,
        )
        .expect_err("wrong source must fail"),
        AdvisoryError::SourceMismatch
    );
    assert_eq!(clock.calls(), 0);

    let checkout = TestCheckout::new("multiple-repos");
    checkout.create_repository("advisory-db-aaaaaaaaaaaaaaaa", FRESH);
    let runner = FakeCommandRunner::new(Vec::new());
    let clock = FixedClock::new(NOW).expect("clock");
    assert_eq!(
        AdvisorySnapshot::inspect(
            &checkout.root,
            Path::new(GIT),
            &tree_sha256(),
            LOCATOR,
            &runner,
            &clock,
        )
        .expect_err("multiple repositories must fail"),
        AdvisoryError::RepositoryCount
    );

    let checkout = TestCheckout::new("wrong-repo-name");
    fs::rename(
        checkout.root.join(ADVISORY_DB_RELATIVE).join(REPOSITORY),
        checkout
            .root
            .join(ADVISORY_DB_RELATIVE)
            .join("rustsec-cache"),
    )
    .expect("rename repository");
    let runner = FakeCommandRunner::new(Vec::new());
    let clock = FixedClock::new(NOW).expect("clock");
    assert_eq!(
        AdvisorySnapshot::inspect(
            &checkout.root,
            Path::new(GIT),
            &tree_sha256(),
            LOCATOR,
            &runner,
            &clock,
        )
        .expect_err("wrong repository name must fail"),
        AdvisoryError::RepositoryName
    );

    let checkout = TestCheckout::new("archive-mismatch");
    let runner = FakeCommandRunner::new(snapshot_commands(&checkout));
    let clock = FixedClock::new(NOW).expect("clock");
    assert_eq!(
        AdvisorySnapshot::inspect(
            &checkout.root,
            Path::new(GIT),
            &"0".repeat(64),
            LOCATOR,
            &runner,
            &clock,
        )
        .expect_err("archive digest mismatch must fail"),
        AdvisoryError::ArchiveDigestMismatch
    );
    assert_eq!(clock.calls(), 0);
}

#[test]
fn fetch_head_missing_stale_and_future_are_distinct() {
    let checkout = TestCheckout::new("missing-fetch");
    fs::remove_file(
        checkout
            .root
            .join(ADVISORY_DB_RELATIVE)
            .join(REPOSITORY)
            .join(".git/FETCH_HEAD"),
    )
    .expect("remove FETCH_HEAD");
    let runner = FakeCommandRunner::new(Vec::new());
    let clock = FixedClock::new(NOW).expect("clock");
    assert_eq!(
        AdvisorySnapshot::inspect(
            &checkout.root,
            Path::new(GIT),
            &tree_sha256(),
            LOCATOR,
            &runner,
            &clock,
        )
        .expect_err("missing FETCH_HEAD must fail"),
        AdvisoryError::FetchHeadMissing
    );
    assert_eq!(clock.calls(), 0);

    for (label, acquired, expected) in [
        (
            "stale-fetch",
            "2026-07-20T11:59:59Z",
            AdvisoryError::SnapshotStale,
        ),
        (
            "future-fetch",
            "2026-07-21T12:00:01Z",
            AdvisoryError::AcquisitionFuture,
        ),
    ] {
        let checkout = TestCheckout::new(label);
        checkout.set_fetch_time(REPOSITORY, acquired);
        let runner = FakeCommandRunner::new(snapshot_commands(&checkout));
        let clock = FixedClock::new(NOW).expect("clock");
        assert_eq!(
            AdvisorySnapshot::inspect(
                &checkout.root,
                Path::new(GIT),
                &tree_sha256(),
                LOCATOR,
                &runner,
                &clock,
            )
            .expect_err("invalid acquisition age must fail"),
            expected
        );
        assert_eq!(clock.calls(), 1);
    }
}

#[test]
fn tracked_untracked_and_ignored_snapshot_statuses_are_dirty() {
    for (label, dirty) in [
        ("tracked", b" M README.md\0".as_slice()),
        ("untracked", b"?? local.txt\0".as_slice()),
        ("ignored", b"!! ignored.tmp\0".as_slice()),
    ] {
        let checkout = TestCheckout::new(label);
        let runner = FakeCommandRunner::new(vec![git_command(
            &checkout,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignored",
            ],
            0,
            dirty,
        )]);
        let clock = FixedClock::new(NOW).expect("clock");
        assert_eq!(
            AdvisorySnapshot::inspect(
                &checkout.root,
                Path::new(GIT),
                &tree_sha256(),
                LOCATOR,
                &runner,
                &clock,
            )
            .expect_err("dirty snapshot must fail"),
            AdvisoryError::SnapshotDirty
        );
        assert_eq!(clock.calls(), 0);
    }
}

#[test]
fn shallow_and_linked_snapshot_ancestry_are_rejected() {
    let checkout = TestCheckout::new("malformed-commit");
    let mut commands = snapshot_commands(&checkout);
    commands.truncate(3);
    commands[2] = git_command(
        &checkout,
        &["rev-parse", "HEAD^{commit}"],
        0,
        b"0123456789ABCDEF0123456789ABCDEF01234567\n",
    );
    let runner = FakeCommandRunner::new(commands);
    let clock = FixedClock::new(NOW).expect("clock");
    assert_eq!(
        AdvisorySnapshot::inspect(
            &checkout.root,
            Path::new(GIT),
            &tree_sha256(),
            LOCATOR,
            &runner,
            &clock,
        )
        .expect_err("malformed commit must fail"),
        AdvisoryError::CommitMalformed
    );

    let checkout = TestCheckout::new("shallow");
    let mut commands = snapshot_commands(&checkout);
    commands.truncate(4);
    commands[3] = git_command(
        &checkout,
        &["rev-parse", "--is-shallow-repository"],
        0,
        b"true\n",
    );
    let runner = FakeCommandRunner::new(commands);
    let clock = FixedClock::new(NOW).expect("clock");
    assert_eq!(
        AdvisorySnapshot::inspect(
            &checkout.root,
            Path::new(GIT),
            &tree_sha256(),
            LOCATOR,
            &runner,
            &clock,
        )
        .expect_err("shallow repository must fail"),
        AdvisoryError::ShallowRepository
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let checkout = TestCheckout::new("linked-git");
        let repository = checkout.root.join(ADVISORY_DB_RELATIVE).join(REPOSITORY);
        let outside = std::env::temp_dir().join(format!(
            "solstone-release-advisory-outside-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&outside).expect("create outside directory");
        fs::write(outside.join("sentinel"), b"outside-private-sentinel")
            .expect("write outside sentinel");
        fs::remove_dir_all(repository.join(".git")).expect("remove contained .git");
        symlink(&outside, repository.join(".git")).expect("plant .git symlink");
        let runner = FakeCommandRunner::new(Vec::new());
        let clock = FixedClock::new(NOW).expect("clock");
        assert_eq!(
            AdvisorySnapshot::inspect(
                &checkout.root,
                Path::new(GIT),
                &tree_sha256(),
                LOCATOR,
                &runner,
                &clock,
            )
            .expect_err("linked ancestry must fail"),
            AdvisoryError::SnapshotContainment
        );
        assert_eq!(
            fs::read(outside.join("sentinel")).expect("read outside sentinel"),
            b"outside-private-sentinel"
        );
        fs::remove_dir_all(outside).expect("remove outside directory");
        // Live Windows junction/reparse coverage is post-ship; this path uses the
        // same artifact_fs reparse/link rejection seam exercised by its unit tests.
    }
}

#[test]
fn mirror_packet_files_must_be_separate_safe_regular_files() {
    let checkout = TestCheckout::new("missing-body");
    fs::remove_file(&checkout.freshness_receipt).expect("remove freshness body");
    assert_eq!(
        run_check(&checkout, Vec::new(), NOW).expect_err("missing body must fail"),
        AdvisoryError::FreshnessReceiptMissing
    );

    let checkout = TestCheckout::new("missing-signature");
    fs::remove_file(checkout.signature_path()).expect("remove freshness signature");
    assert_eq!(
        run_check(&checkout, Vec::new(), NOW).expect_err("missing signature must fail"),
        AdvisoryError::FreshnessSignatureMissing
    );

    let checkout = TestCheckout::new("missing-public-key");
    fs::remove_file(&checkout.mirror_public_key).expect("remove public key");
    assert_eq!(
        run_check(&checkout, Vec::new(), NOW).expect_err("missing public key must fail"),
        AdvisoryError::MirrorPublicKeyMissing
    );

    for (label, target, expected) in [
        (
            "directory-body",
            "body",
            AdvisoryError::FreshnessReceiptMissing,
        ),
        (
            "directory-signature",
            "signature",
            AdvisoryError::FreshnessSignatureMissing,
        ),
        (
            "directory-public-key",
            "public-key",
            AdvisoryError::MirrorPublicKeyMissing,
        ),
    ] {
        let checkout = TestCheckout::new(label);
        let path = match target {
            "body" => checkout.freshness_receipt.clone(),
            "signature" => checkout.signature_path(),
            "public-key" => checkout.mirror_public_key.clone(),
            _ => unreachable!(),
        };
        fs::remove_file(&path).expect("remove packet file");
        fs::create_dir(&path).expect("replace packet file with directory");
        assert_eq!(
            run_check(&checkout, Vec::new(), NOW).expect_err("directory must fail"),
            expected
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        for (label, target, expected) in [
            (
                "linked-body",
                "body",
                AdvisoryError::FreshnessReceiptMissing,
            ),
            (
                "linked-signature",
                "signature",
                AdvisoryError::FreshnessSignatureMissing,
            ),
            (
                "linked-public-key",
                "public-key",
                AdvisoryError::MirrorPublicKeyMissing,
            ),
        ] {
            let checkout = TestCheckout::new(label);
            let path = match target {
                "body" => checkout.freshness_receipt.clone(),
                "signature" => checkout.signature_path(),
                "public-key" => checkout.mirror_public_key.clone(),
                _ => unreachable!(),
            };
            let outside = std::env::temp_dir().join(format!(
                "solstone-advisory-packet-outside-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            fs::write(&outside, b"private linked packet bytes").expect("write outside packet");
            fs::remove_file(&path).expect("remove packet leaf");
            symlink(&outside, &path).expect("plant packet symlink");
            assert_eq!(
                run_check(&checkout, Vec::new(), NOW).expect_err("linked packet must fail"),
                expected
            );
            fs::remove_file(outside).expect("remove outside packet");
        }
    }
}

#[test]
fn mirror_key_pin_and_minisign_fail_before_repository_inspection() {
    let checkout = TestCheckout::new("wrong-key-pin");
    let runner = FakeCommandRunner::new(Vec::new());
    let wrong_pin = "0".repeat(64);
    let inputs = MirrorPacketInputs {
        expected_public_key_sha256: &wrong_pin,
        ..checkout.mirror_inputs()
    };
    assert_eq!(
        run_advisory_check(
            &checkout.root,
            VERSION,
            Path::new(GIT),
            &tree_sha256(),
            &advisory_action(),
            &inputs,
            &runner,
            &FixedClock::new(NOW).expect("clock"),
        )
        .expect_err("wrong key pin must fail"),
        AdvisoryError::MirrorPublicKeyPinMismatch
    );
    assert_eq!(runner.remaining().expect("wrong-key runner queue"), 0);

    let checkout = TestCheckout::new("minisign-nonzero");
    assert_eq!(
        run_check(&checkout, vec![minisign_command(&checkout, 1)], NOW)
            .expect_err("nonzero minisign must fail"),
        AdvisoryError::FreshnessSignatureInvalid
    );

    let checkout = TestCheckout::new("minisign-invocation");
    assert_eq!(
        run_check(&checkout, Vec::new(), NOW).expect_err("invocation failure must fail"),
        AdvisoryError::MinisignInvocationFailed
    );
}

#[test]
fn trusted_comment_and_body_binding_fail_closed() {
    let malformed_cases = [
        (
            "wrong-prefix",
            format!("wrong-prefix synced_commit={COMMIT} utc={NOW} max_age=86400"),
            AdvisoryError::FreshnessTrustedCommentPrefix,
        ),
        (
            "wrong-count",
            format!("solpbc-advisory-mirror-v1 synced_commit={COMMIT} utc={NOW}"),
            AdvisoryError::FreshnessTrustedCommentFields,
        ),
        (
            "wrong-key-order",
            format!(
                "solpbc-advisory-mirror-v1 utc={NOW} synced_commit={COMMIT} max_age=86400"
            ),
            AdvisoryError::FreshnessTrustedCommentFields,
        ),
        (
            "empty-field",
            format!("solpbc-advisory-mirror-v1 synced_commit= utc={NOW} max_age=86400"),
            AdvisoryError::FreshnessTrustedCommentFields,
        ),
        (
            "commit-shape",
            format!(
                "solpbc-advisory-mirror-v1 synced_commit={} utc={NOW} max_age=86400",
                COMMIT.to_ascii_uppercase()
            ),
            AdvisoryError::FreshnessSyncedCommitMalformed,
        ),
        (
            "utc-shape",
            format!(
                "solpbc-advisory-mirror-v1 synced_commit={COMMIT} utc=2026-07-21T12:00:00+00:00 max_age=86400"
            ),
            AdvisoryError::FreshnessUtcMalformed,
        ),
        (
            "decimal-max-age",
            format!(
                "solpbc-advisory-mirror-v1 synced_commit={COMMIT} utc={NOW} max_age=86400.0"
            ),
            AdvisoryError::FreshnessMaxAgeInvalid,
        ),
        (
            "noncanonical-max-age",
            format!(
                "solpbc-advisory-mirror-v1 synced_commit={COMMIT} utc={NOW} max_age=086400"
            ),
            AdvisoryError::FreshnessMaxAgeInvalid,
        ),
        (
            "wrong-max-age",
            format!(
                "solpbc-advisory-mirror-v1 synced_commit={COMMIT} utc={NOW} max_age=86401"
            ),
            AdvisoryError::FreshnessMaxAgeInvalid,
        ),
    ];
    for (label, comment, expected) in malformed_cases {
        let checkout = TestCheckout::new(label);
        write_trusted_comment(&checkout, &comment);
        assert_eq!(
            run_check(&checkout, vec![minisign_command(&checkout, 0)], NOW)
                .expect_err("malformed trusted comment must fail"),
            expected,
            "case {label}"
        );
    }

    let checkout = TestCheckout::new("non-utf8-signature");
    fs::write(checkout.signature_path(), [0xff, 0xfe]).expect("write non-UTF-8 signature");
    assert_eq!(
        run_check(&checkout, vec![minisign_command(&checkout, 0)], NOW)
            .expect_err("non-UTF-8 signature must fail"),
        AdvisoryError::FreshnessTrustedCommentMissing
    );

    let checkout = TestCheckout::new("body-mismatch");
    fs::write(&checkout.freshness_receipt, b"{}\n").expect("replace freshness body");
    assert_eq!(
        run_check(&checkout, vec![minisign_command(&checkout, 0)], NOW)
            .expect_err("body mismatch must fail"),
        AdvisoryError::FreshnessBodyMismatch
    );
}

#[test]
fn freshness_and_commit_boundaries_are_exact() {
    for (label, utc) in [
        ("future-boundary", "2026-07-21T12:05:00Z"),
        ("age-boundary", "2026-07-20T12:00:00Z"),
    ] {
        let checkout = TestCheckout::new(label);
        write_packet(&checkout, COMMIT, utc, 86_400);
        let mut commands = vec![minisign_command(&checkout, 0)];
        commands.extend(snapshot_commands(&checkout));
        commands.push(cargo_command(&checkout, 0));
        run_check(&checkout, commands, NOW).expect("inclusive freshness boundary must pass");
    }

    for (label, utc, expected) in [
        (
            "future-over-boundary",
            "2026-07-21T12:05:01Z",
            AdvisoryError::FreshnessUtcFuture,
        ),
        (
            "stale-over-boundary",
            "2026-07-20T11:59:59Z",
            AdvisoryError::FreshnessStale,
        ),
    ] {
        let checkout = TestCheckout::new(label);
        write_packet(&checkout, COMMIT, utc, 86_400);
        assert_eq!(
            run_check(&checkout, vec![minisign_command(&checkout, 0)], NOW)
                .expect_err("exclusive freshness boundary must fail"),
            expected
        );
    }

    let checkout = TestCheckout::new("commit-mismatch");
    write_packet(
        &checkout,
        "fedcba9876543210fedcba9876543210fedcba98",
        NOW,
        86_400,
    );
    let mut commands = vec![minisign_command(&checkout, 0)];
    commands.extend(snapshot_commands(&checkout));
    assert_eq!(
        run_check(&checkout, commands, NOW).expect_err("commit mismatch must fail"),
        AdvisoryError::FreshnessCommitMismatch
    );
}

#[test]
fn checked_at_is_earned_only_after_cargo_deny_success() {
    let checkout = TestCheckout::new("check-pass");
    let mut commands = vec![minisign_command(&checkout, 0)];
    commands.extend(snapshot_commands(&checkout));
    commands.push(cargo_command(&checkout, 0));
    let runner = FakeCommandRunner::new(commands);
    let clock = FixedClock::new(NOW).expect("clock");
    let provenance = run_advisory_check(
        &checkout.root,
        VERSION,
        Path::new(GIT),
        &tree_sha256(),
        &advisory_action(),
        &checkout.mirror_inputs(),
        &runner,
        &clock,
    )
    .expect("complete advisory check");
    assert_eq!(provenance.checked_at, NOW);
    assert_eq!(clock.calls(), 3);
    assert_eq!(runner.remaining().expect("read fake queue"), 0);

    let checkout = TestCheckout::new("check-fail");
    let mut commands = vec![minisign_command(&checkout, 0)];
    commands.extend(snapshot_commands(&checkout));
    commands.push(cargo_command(&checkout, 9));
    let runner = FakeCommandRunner::new(commands);
    let clock = FixedClock::new(NOW).expect("clock");
    assert_eq!(
        run_advisory_check(
            &checkout.root,
            VERSION,
            Path::new(GIT),
            &tree_sha256(),
            &advisory_action(),
            &checkout.mirror_inputs(),
            &runner,
            &clock,
        )
        .expect_err("cargo-deny failure must fail"),
        AdvisoryError::CargoDenyFailed
    );
    assert_eq!(clock.calls(), 2, "checked_at was never requested");
}

#[test]
fn advisory_errors_and_provenance_do_not_leak_private_data() {
    let checkout = TestCheckout::new("private-canary");
    let mut commands = snapshot_commands(&checkout);
    commands.truncate(2);
    commands[1] = git_command(
        &checkout,
        &["remote", "get-url", "origin"],
        0,
        b"https://credential.example/private-token\n",
    );
    let runner = FakeCommandRunner::new(commands);
    let clock = FixedClock::new(NOW).expect("clock");
    let origin_message = AdvisorySnapshot::inspect(
        &checkout.root,
        Path::new(GIT),
        &tree_sha256(),
        LOCATOR,
        &runner,
        &clock,
    )
    .expect_err("private source must fail")
    .to_string();

    let signature_checkout = TestCheckout::new("private-signature-canary");
    let signature_message = run_check(
        &signature_checkout,
        vec![minisign_command(&signature_checkout, 1)],
        NOW,
    )
    .expect_err("private signature failure must fail")
    .to_string();

    let comment_checkout = TestCheckout::new("private-comment-canary");
    write_trusted_comment(
        &comment_checkout,
        "private-token malformed trusted comment path=/operator/private/packet",
    );
    let comment_message = run_check(
        &comment_checkout,
        vec![minisign_command(&comment_checkout, 0)],
        NOW,
    )
    .expect_err("private comment failure must fail")
    .to_string();

    let cargo_checkout = TestCheckout::new("private-cargo-canary");
    let mut cargo_commands = vec![minisign_command(&cargo_checkout, 0)];
    cargo_commands.extend(snapshot_commands(&cargo_checkout));
    cargo_commands.push(cargo_command(&cargo_checkout, 1));
    let cargo_message = run_check(&cargo_checkout, cargo_commands, NOW)
        .expect_err("private cargo failure must fail")
        .to_string();

    for message in [
        origin_message,
        signature_message,
        comment_message,
        cargo_message,
    ] {
        for private in [
            checkout.root.to_str().expect("utf8 root"),
            signature_checkout
                .freshness_receipt
                .to_str()
                .expect("utf8 signature receipt path"),
            comment_checkout
                .mirror_public_key
                .to_str()
                .expect("utf8 comment key path"),
            cargo_checkout.root.to_str().expect("utf8 cargo root"),
            LOCATOR,
            "mirror.example.invalid",
            "private-token",
            "/operator/private/packet",
            "private signature canary",
        ] {
            assert!(
                !message.contains(private),
                "private canary leaked: {private}"
            );
        }
    }

    let checkout = TestCheckout::new("public-provenance");
    let mut commands = vec![minisign_command(&checkout, 0)];
    commands.extend(snapshot_commands(&checkout));
    commands.push(cargo_command(&checkout, 0));
    let provenance = run_check(&checkout, commands, NOW).expect("produce public provenance");
    let rendered = serde_json::to_string(&provenance).expect("render provenance");
    assert!(!rendered.contains(checkout.root.to_str().expect("utf8 root")));
    assert!(!rendered.contains("credential"));
    assert!(rendered.contains(MIRROR_COHORT_ID));
    assert!(!rendered.contains(LOCATOR));
    assert!(!rendered.contains("private-token"));
}
