// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use xtask::release_exec::test_support::{FakeCommand, FakeCommandRunner};
use xtask::release_exec::{CommandOutput, ProcessCommandRunner};
use xtask::release_source_binding::{LockFile, SourceBindingError, SourceBindingVerifier};

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const OTHER_COMMIT: &str = "89abcdef0123456789abcdef0123456789abcdef";
const GIT: &str = "/selected/git";

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TestCheckout {
    root: PathBuf,
}

impl TestCheckout {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "solstone-release-source-binding-{label}-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("create test checkout");
        let checkout = Self { root };
        checkout.write("Cargo.lock", b"cargo lock bytes");
        checkout.write("ui/package-lock.json", b"ui lock bytes");
        checkout
    }

    fn write(&self, relative: &str, bytes: &[u8]) {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create lock parent");
        }
        fs::write(path, bytes).expect("write checkout file");
    }

    fn canonical(&self) -> String {
        fs::canonicalize(&self.root)
            .expect("canonicalize checkout")
            .to_str()
            .expect("utf8 test checkout")
            .to_owned()
    }
}

impl Drop for TestCheckout {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove test checkout");
    }
}

fn output(status: i32, stdout: &[u8]) -> CommandOutput {
    CommandOutput {
        status,
        stdout: stdout.to_vec(),
        stderr: Vec::new(),
    }
}

fn git_command(root: &TestCheckout, tail: &[&str], status: i32, stdout: &[u8]) -> FakeCommand {
    let mut args = vec!["-C".to_owned(), root.canonical()];
    args.extend(tail.iter().map(|arg| (*arg).to_owned()));
    FakeCommand::output(PathBuf::from(GIT), args, output(status, stdout))
}

fn initial_commands(root: &TestCheckout, checkout_ref: &str) -> Vec<FakeCommand> {
    vec![
        git_command(
            root,
            &["cat-file", "-e", &format!("{COMMIT}^{{commit}}")],
            0,
            b"",
        ),
        git_command(
            root,
            &["merge-base", "--is-ancestor", COMMIT, "HEAD"],
            0,
            b"",
        ),
        git_command(
            root,
            &["rev-parse", "HEAD"],
            0,
            format!("{COMMIT}\n").as_bytes(),
        ),
        git_command(
            root,
            &["symbolic-ref", "HEAD"],
            0,
            format!("{checkout_ref}\n").as_bytes(),
        ),
        git_command(
            root,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignore-submodules=none",
            ],
            0,
            b"",
        ),
        git_command(
            root,
            &["ls-files", "--error-unmatch", "--", "Cargo.lock"],
            0,
            b"Cargo.lock\n",
        ),
        git_command(
            root,
            &["ls-files", "--error-unmatch", "--", "ui/package-lock.json"],
            0,
            b"ui/package-lock.json\n",
        ),
    ]
}

fn reverify_commands(root: &TestCheckout, checkout_ref: &str) -> Vec<FakeCommand> {
    vec![
        git_command(
            root,
            &["rev-parse", "HEAD"],
            0,
            format!("{COMMIT}\n").as_bytes(),
        ),
        git_command(
            root,
            &["symbolic-ref", "HEAD"],
            0,
            format!("{checkout_ref}\n").as_bytes(),
        ),
        git_command(
            root,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignore-submodules=none",
            ],
            0,
            b"",
        ),
        git_command(
            root,
            &["ls-files", "--error-unmatch", "--", "Cargo.lock"],
            0,
            b"Cargo.lock\n",
        ),
        git_command(
            root,
            &["ls-files", "--error-unmatch", "--", "ui/package-lock.json"],
            0,
            b"ui/package-lock.json\n",
        ),
    ]
}

#[test]
fn source_binding_accepts_main_and_sync_and_uses_only_local_git_operations() {
    for checkout_ref in ["refs/heads/main", "refs/heads/__swsync"] {
        let root = TestCheckout::new("happy");
        let runner = FakeCommandRunner::new(initial_commands(&root, checkout_ref));
        let verifier = SourceBindingVerifier::new(&root.root, Path::new(GIT), &runner)
            .expect("create source verifier");
        let binding = verifier
            .verify(COMMIT)
            .expect("verify local source binding");

        assert_eq!(binding.commit, COMMIT);
        assert_eq!(binding.checkout_ref, checkout_ref);
        assert_eq!(binding.cargo_lock_sha256.len(), 64);
        assert_eq!(binding.ui_package_lock_sha256.len(), 64);
        assert_ne!(binding.cargo_lock_sha256, binding.ui_package_lock_sha256);
        assert_eq!(runner.remaining().expect("read fake queue"), 0);
        let witness = runner.witness().expect("read git witness");
        assert!(witness.iter().all(|invocation| {
            !invocation.args.iter().any(|arg| {
                matches!(
                    arg.as_str(),
                    "fetch" | "ls-remote" | "pull" | "remote" | "gh"
                )
            })
        }));
    }
}

#[test]
fn expected_commit_rejects_every_non_full_lowercase_form_without_git() {
    let root = TestCheckout::new("expected-format");
    for invalid in [
        "main",
        "v0.2.11",
        "01234567",
        "0123456789ABCDEF0123456789ABCDEF01234567",
        "0123456789abcdef0123456789abcdef0123456g",
        "0123456789abcdef0123456789abcdef01234567^",
    ] {
        let runner = FakeCommandRunner::new(Vec::new());
        let verifier = SourceBindingVerifier::new(&root.root, Path::new(GIT), &runner)
            .expect("create source verifier");
        assert_eq!(
            verifier
                .verify(invalid)
                .expect_err("invalid expected commit must fail"),
            SourceBindingError::InvalidExpectedCommit
        );
        assert!(runner.witness().expect("read empty witness").is_empty());
    }
}

#[test]
fn absent_local_object_and_wrong_lineage_are_distinct() {
    let root = TestCheckout::new("object-lineage");
    let runner = FakeCommandRunner::new(vec![git_command(
        &root,
        &["cat-file", "-e", &format!("{COMMIT}^{{commit}}")],
        1,
        b"",
    )]);
    let verifier = SourceBindingVerifier::new(&root.root, Path::new(GIT), &runner)
        .expect("create source verifier");
    assert_eq!(
        verifier
            .verify(COMMIT)
            .expect_err("missing local object must fail"),
        SourceBindingError::LocalCommitMissing
    );

    let runner = FakeCommandRunner::new(vec![
        git_command(
            &root,
            &["cat-file", "-e", &format!("{COMMIT}^{{commit}}")],
            0,
            b"",
        ),
        git_command(
            &root,
            &["merge-base", "--is-ancestor", COMMIT, "HEAD"],
            1,
            b"",
        ),
    ]);
    let verifier = SourceBindingVerifier::new(&root.root, Path::new(GIT), &runner)
        .expect("create source verifier");
    assert_eq!(
        verifier
            .verify(COMMIT)
            .expect_err("wrong lineage must fail"),
        SourceBindingError::WrongLineage
    );
}

#[test]
fn head_detachment_and_wrong_ref_are_rejected() {
    let root = TestCheckout::new("head-ref");
    let mut commands = initial_commands(&root, "refs/heads/main");
    commands.truncate(3);
    commands[2] = git_command(
        &root,
        &["rev-parse", "HEAD"],
        0,
        format!("{OTHER_COMMIT}\n").as_bytes(),
    );
    let runner = FakeCommandRunner::new(commands);
    let verifier = SourceBindingVerifier::new(&root.root, Path::new(GIT), &runner)
        .expect("create source verifier");
    assert_eq!(
        verifier
            .verify(COMMIT)
            .expect_err("HEAD mismatch must fail"),
        SourceBindingError::HeadMismatch
    );

    let mut commands = initial_commands(&root, "refs/heads/main");
    commands.truncate(4);
    commands[3] = git_command(&root, &["symbolic-ref", "HEAD"], 1, b"");
    let runner = FakeCommandRunner::new(commands);
    let verifier = SourceBindingVerifier::new(&root.root, Path::new(GIT), &runner)
        .expect("create source verifier");
    assert_eq!(
        verifier
            .verify(COMMIT)
            .expect_err("detached HEAD must fail"),
        SourceBindingError::DetachedHead
    );

    let mut commands = initial_commands(&root, "refs/heads/feature");
    commands.truncate(4);
    let runner = FakeCommandRunner::new(commands);
    let verifier = SourceBindingVerifier::new(&root.root, Path::new(GIT), &runner)
        .expect("create source verifier");
    assert_eq!(
        verifier
            .verify(COMMIT)
            .expect_err("other checkout ref must fail"),
        SourceBindingError::CheckoutRefRejected
    );
}

#[test]
fn tracked_untracked_and_unmerged_statuses_are_all_dirty() {
    let root = TestCheckout::new("dirty");
    for status in [
        b" M Cargo.lock\0".as_slice(),
        b"?? local-source.rs\0".as_slice(),
        b"UU src-tauri/src/main.rs\0".as_slice(),
    ] {
        let mut commands = initial_commands(&root, "refs/heads/main");
        commands.truncate(5);
        commands[4] = git_command(
            &root,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignore-submodules=none",
            ],
            0,
            status,
        );
        let runner = FakeCommandRunner::new(commands);
        let verifier = SourceBindingVerifier::new(&root.root, Path::new(GIT), &runner)
            .expect("create source verifier");
        assert_eq!(
            verifier.verify(COMMIT).expect_err("dirty status must fail"),
            SourceBindingError::DirtyCheckout
        );
    }
}

#[test]
fn configured_ignored_dirty_submodule_is_still_rejected_by_real_git() {
    let git = absolute_git();
    let parent = TestCheckout::new("real-submodule-parent");
    let child = TestCheckout::new("real-submodule-child");
    init_repository(&git, &child.root);
    child.write("member.txt", b"clean submodule bytes\n");
    git_ok(&git, &child.root, &["add", "."]);
    git_ok(&git, &child.root, &["commit", "-m", "submodule fixture"]);

    init_repository(&git, &parent.root);
    git_ok(&git, &parent.root, &["add", "."]);
    git_ok(&git, &parent.root, &["commit", "-m", "parent fixture"]);
    git_ok(
        &git,
        &parent.root,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            child.root.to_str().expect("UTF-8 submodule path"),
            "vendor/sub",
        ],
    );
    git_ok(&git, &parent.root, &["commit", "-am", "add submodule"]);
    git_ok(
        &git,
        &parent.root,
        &["config", "submodule.vendor/sub.ignore", "all"],
    );
    fs::write(
        parent.root.join("vendor/sub/member.txt"),
        b"dirty submodule\n",
    )
    .expect("dirty submodule worktree");

    let default_status = git_output(
        &git,
        &parent.root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    );
    assert!(
        default_status.is_empty(),
        "fixture must mask default submodule dirt"
    );
    let expected = String::from_utf8(git_output(&git, &parent.root, &["rev-parse", "HEAD"]))
        .expect("HEAD is UTF-8")
        .trim()
        .to_owned();
    let runner = ProcessCommandRunner;
    let verifier = SourceBindingVerifier::new(&parent.root, &git, &runner)
        .expect("create real-Git source verifier");
    assert_eq!(
        verifier
            .verify(&expected)
            .expect_err("explicit submodule status must reject dirt"),
        SourceBindingError::DirtyCheckout
    );
}

fn absolute_git() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").expect("PATH must be available"))
        .flat_map(|directory| [directory.join("git"), directory.join("git.exe")].into_iter())
        .find(|candidate| candidate.is_file())
        .and_then(|candidate| fs::canonicalize(candidate).ok())
        .expect("real Git executable must be available for source-binding test")
}

fn init_repository(git: &Path, root: &Path) {
    git_ok(git, root, &["init", "-b", "main"]);
    git_ok(git, root, &["config", "user.email", "tests@solstone.app"]);
    git_ok(git, root, &["config", "user.name", "solstone tests"]);
}

fn git_ok(git: &Path, root: &Path, args: &[&str]) {
    let output = Command::new(git)
        .args(args)
        .current_dir(root)
        .output()
        .expect("run fixture Git");
    assert!(
        output.status.success(),
        "fixture Git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_output(git: &Path, root: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new(git)
        .args(args)
        .current_dir(root)
        .output()
        .expect("run fixture Git");
    assert!(output.status.success(), "fixture Git command must succeed");
    output.stdout
}

#[test]
fn missing_untracked_and_non_regular_lockfiles_are_rejected() {
    let root = TestCheckout::new("locks");
    let mut commands = initial_commands(&root, "refs/heads/main");
    commands.truncate(6);
    commands[5] = git_command(
        &root,
        &["ls-files", "--error-unmatch", "--", "Cargo.lock"],
        1,
        b"",
    );
    let runner = FakeCommandRunner::new(commands);
    let verifier = SourceBindingVerifier::new(&root.root, Path::new(GIT), &runner)
        .expect("create source verifier");
    assert_eq!(
        verifier
            .verify(COMMIT)
            .expect_err("untracked lock must fail"),
        SourceBindingError::LockNotTracked {
            lock: LockFile::Cargo
        }
    );

    fs::remove_file(root.root.join("Cargo.lock")).expect("remove Cargo.lock");
    fs::create_dir(root.root.join("Cargo.lock")).expect("replace lock with directory");
    let mut commands = initial_commands(&root, "refs/heads/main");
    commands.truncate(6);
    let runner = FakeCommandRunner::new(commands);
    let verifier = SourceBindingVerifier::new(&root.root, Path::new(GIT), &runner)
        .expect("create source verifier");
    assert_eq!(
        verifier
            .verify(COMMIT)
            .expect_err("non-regular lock must fail"),
        SourceBindingError::LockNotRegular {
            lock: LockFile::Cargo
        }
    );

    fs::remove_dir(root.root.join("Cargo.lock")).expect("remove lock directory");
    let mut commands = initial_commands(&root, "refs/heads/main");
    commands.truncate(6);
    let runner = FakeCommandRunner::new(commands);
    let verifier = SourceBindingVerifier::new(&root.root, Path::new(GIT), &runner)
        .expect("create source verifier");
    assert_eq!(
        verifier
            .verify(COMMIT)
            .expect_err("missing tracked lock bytes must fail"),
        SourceBindingError::LockNotRegular {
            lock: LockFile::Cargo
        }
    );
}

#[test]
fn reverify_rejects_head_ref_and_status_drift() {
    let root = TestCheckout::new("reverify-git");
    for (replacement, expected_error) in [
        (
            git_command(
                &root,
                &["rev-parse", "HEAD"],
                0,
                format!("{OTHER_COMMIT}\n").as_bytes(),
            ),
            SourceBindingError::ReverifyHeadDrift,
        ),
        (
            git_command(
                &root,
                &["symbolic-ref", "HEAD"],
                0,
                b"refs/heads/__swsync\n",
            ),
            SourceBindingError::ReverifyRefDrift,
        ),
        (
            git_command(
                &root,
                &[
                    "status",
                    "--porcelain=v1",
                    "-z",
                    "--untracked-files=all",
                    "--ignore-submodules=none",
                ],
                0,
                b"?? drift\0",
            ),
            SourceBindingError::ReverifyStatusDrift,
        ),
    ] {
        let mut commands = initial_commands(&root, "refs/heads/main");
        let mut reverify = reverify_commands(&root, "refs/heads/main");
        let index = match expected_error {
            SourceBindingError::ReverifyHeadDrift => 0,
            SourceBindingError::ReverifyRefDrift => 1,
            SourceBindingError::ReverifyStatusDrift => 2,
            _ => unreachable!(),
        };
        reverify[index] = replacement;
        reverify.truncate(index + 1);
        commands.extend(reverify);
        let runner = FakeCommandRunner::new(commands);
        let verifier = SourceBindingVerifier::new(&root.root, Path::new(GIT), &runner)
            .expect("create source verifier");
        let binding = verifier.verify(COMMIT).expect("establish source binding");
        assert_eq!(
            verifier
                .reverify(&binding)
                .expect_err("git drift must fail reverify"),
            expected_error
        );
    }
}

#[test]
fn reverify_rejects_each_lock_digest_drift() {
    for (relative, lock) in [
        ("Cargo.lock", LockFile::Cargo),
        ("ui/package-lock.json", LockFile::UiPackage),
    ] {
        let root = TestCheckout::new("reverify-lock");
        let mut commands = initial_commands(&root, "refs/heads/main");
        commands.extend(reverify_commands(&root, "refs/heads/main"));
        let runner = FakeCommandRunner::new(commands);
        let verifier = SourceBindingVerifier::new(&root.root, Path::new(GIT), &runner)
            .expect("create source verifier");
        let binding = verifier.verify(COMMIT).expect("establish source binding");
        root.write(relative, b"drifted lock bytes");
        assert_eq!(
            verifier
                .reverify(&binding)
                .expect_err("lock drift must fail reverify"),
            SourceBindingError::ReverifyLockDrift { lock }
        );
    }
}

#[test]
fn diagnostics_do_not_echo_the_absolute_checkout_path() {
    let root = TestCheckout::new("private-canary");
    let runner = FakeCommandRunner::new(vec![git_command(
        &root,
        &["cat-file", "-e", &format!("{COMMIT}^{{commit}}")],
        1,
        b"",
    )]);
    let verifier = SourceBindingVerifier::new(&root.root, Path::new(GIT), &runner)
        .expect("create source verifier");
    let message = verifier
        .verify(COMMIT)
        .expect_err("missing local commit must fail")
        .to_string();
    assert!(!message.contains(root.root.to_str().expect("utf8 root")));
    assert!(message.contains("local object database"));
}
