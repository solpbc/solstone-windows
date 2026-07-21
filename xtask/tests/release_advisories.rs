// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::fs::{self, FileTimes};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};
use xtask::release_advisory::{
    materialize_advisory_config_at, render_advisory_config, run_advisory_check, AdvisoryError,
    AdvisorySnapshot, ADVISORY_DB_RELATIVE, RUSTSEC_SOURCE_ID,
};
use xtask::release_clock::{FixedClock, UtcTimestamp};
use xtask::release_exec::test_support::{FakeCommand, FakeCommandRunner};
use xtask::release_exec::CommandOutput;
use xtask::release_selection::SelectedAction;

const VERSION: &str = "0.2.11";
const NOW: &str = "2026-07-21T12:00:00Z";
const FRESH: &str = "2026-07-21T00:00:00Z";
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const REPOSITORY: &str = "advisory-db-3157b0e258782691";
const GIT: &str = "/selected/git";
const CARGO: &str = "/selected/cargo.exe";
const ARCHIVE: &[u8] = b"deterministic git archive bytes";

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TestCheckout {
    root: PathBuf,
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
        let checkout = Self { root };
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
        fs::canonicalize(self.root.join(ADVISORY_DB_RELATIVE).join(REPOSITORY))
            .expect("canonicalize fake repository")
    }

    fn config_path(&self) -> PathBuf {
        self.root.join(format!(
            "target/release-finalizer/{VERSION}/advisory/deny.toml"
        ))
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
            format!("{RUSTSEC_SOURCE_ID}\n").as_bytes(),
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

#[test]
fn deterministic_advisory_config_is_byte_exact() {
    let deny = fs::read(workspace_root().join("deny.toml")).expect("read deny.toml");
    let database = Path::new("/isolated/advisory-db");
    let expected = concat!(
        "[advisories]\n",
        "db-path = \"/isolated/advisory-db\"\n",
        "db-urls = [\"https://github.com/RustSec/advisory-db\"]\n",
        "yanked = \"warn\"\n",
        "unmaintained = \"workspace\"\n",
        "ignore = [\n",
        "  { id = \"RUSTSEC-2026-0194\", reason = \"quick-xml 0.39.4 O(N^2) attribute dup-check DoS; transitive via plist<-Tauri; no upstream release with the >=0.41 fix yet (plist 1.9.0 pins ^0.39.2). Remove once plist bumps quick-xml. Owner: VPE.\" },\n",
        "  { id = \"RUSTSEC-2026-0195\", reason = \"quick-xml 0.39.4 unbounded namespace-decl growth DoS; transitive via plist<-Tauri; no upstream release with the >=0.41 fix yet (plist 1.9.0 pins ^0.39.2). Remove once plist bumps quick-xml. Owner: VPE.\" },\n",
        "]\n"
    )
    .as_bytes();

    let first = render_advisory_config(&deny, database).expect("render config");
    let second = render_advisory_config(&deny, database).expect("render config again");
    assert_eq!(first, expected);
    assert_eq!(second, expected);
}

#[test]
fn ci_materializer_uses_the_canonical_bytes_and_refuses_overwrite() {
    let checkout = TestCheckout::new("ci-materializer");
    let output_dir = checkout.root.join("target/release-advisory-config-check");
    fs::create_dir(&output_dir).expect("create config-check directory");
    let output = output_dir.join("deny.toml");
    let database = fs::canonicalize(checkout.root.join(ADVISORY_DB_RELATIVE))
        .expect("canonicalize isolated database root");
    let expected = render_advisory_config(
        &fs::read(checkout.root.join("deny.toml")).expect("read deny.toml"),
        &database,
    )
    .expect("render expected policy");

    let materialized = materialize_advisory_config_at(&checkout.root, &database, &output)
        .expect("materialize CI advisory config");
    assert_eq!(materialized.bytes, expected);
    assert_eq!(
        fs::read(&output).expect("read materialized policy"),
        expected
    );
    assert_eq!(materialized.database_root, database);
    assert_eq!(materialized.path, output);
    assert_eq!(
        materialize_advisory_config_at(&checkout.root, &database, &materialized.path)
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
        &runner,
        &clock,
    )
    .expect("inspect fresh snapshot");

    assert_eq!(snapshot.source_id, RUSTSEC_SOURCE_ID);
    assert_eq!(snapshot.commit, COMMIT);
    assert_eq!(snapshot.tree_sha256, tree_sha256());
    assert_eq!(snapshot.acquired_at, FRESH);
    assert_eq!(clock.calls(), 1);
    assert_eq!(runner.remaining().expect("read fake queue"), 0);
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
fn checked_at_is_earned_only_after_cargo_deny_success() {
    let checkout = TestCheckout::new("check-pass");
    let mut commands = snapshot_commands(&checkout);
    commands.push(cargo_command(&checkout, 0));
    let runner = FakeCommandRunner::new(commands);
    let clock = FixedClock::new(NOW).expect("clock");
    let provenance = run_advisory_check(
        &checkout.root,
        VERSION,
        Path::new(GIT),
        &tree_sha256(),
        &advisory_action(),
        &runner,
        &clock,
    )
    .expect("complete advisory check");
    assert_eq!(provenance.checked_at, NOW);
    assert_eq!(clock.calls(), 2);
    assert_eq!(runner.remaining().expect("read fake queue"), 0);

    let checkout = TestCheckout::new("check-fail");
    let mut commands = snapshot_commands(&checkout);
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
            &runner,
            &clock,
        )
        .expect_err("cargo-deny failure must fail"),
        AdvisoryError::CargoDenyFailed
    );
    assert_eq!(clock.calls(), 1, "checked_at was never requested");
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
    let message = AdvisorySnapshot::inspect(
        &checkout.root,
        Path::new(GIT),
        &tree_sha256(),
        &runner,
        &clock,
    )
    .expect_err("private source must fail")
    .to_string();
    assert!(!message.contains(checkout.root.to_str().expect("utf8 root")));
    assert!(!message.contains("private-token"));

    let checkout = TestCheckout::new("public-provenance");
    let runner = FakeCommandRunner::new(snapshot_commands(&checkout));
    let clock = FixedClock::new(NOW).expect("clock");
    let snapshot = AdvisorySnapshot::inspect(
        &checkout.root,
        Path::new(GIT),
        &tree_sha256(),
        &runner,
        &clock,
    )
    .expect("inspect public snapshot");
    let rendered = serde_json::to_string(&snapshot).expect("render snapshot");
    assert!(!rendered.contains(checkout.root.to_str().expect("utf8 root")));
    assert!(!rendered.contains("credential"));
    assert!(rendered.contains(RUSTSEC_SOURCE_ID));
}
