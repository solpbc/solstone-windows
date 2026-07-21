// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Local-only source identity for release construction.

use std::fmt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::artifact_fs::{ContainedRoot, UnixModePolicy};
use crate::release_exec::{CommandOutput, CommandRunner};

const CARGO_LOCK: &str = "Cargo.lock";
const UI_PACKAGE_LOCK: &str = "ui/package-lock.json";
const MAIN_REF: &str = "refs/heads/main";
const SYNC_REF: &str = "refs/heads/__swsync";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceBinding {
    pub commit: String,
    pub checkout_ref: String,
    pub cargo_lock_sha256: String,
    pub ui_package_lock_sha256: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LockFile {
    Cargo,
    UiPackage,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceBindingError {
    InvalidExpectedCommit,
    CheckoutContainment,
    GitInvocationFailed { step: &'static str },
    LocalCommitMissing,
    WrongLineage,
    HeadMalformed,
    HeadMismatch,
    DetachedHead,
    CheckoutRefRejected,
    StatusUnavailable,
    DirtyCheckout,
    LockNotTracked { lock: LockFile },
    LockNotRegular { lock: LockFile },
    ReverifyHeadDrift,
    ReverifyRefDrift,
    ReverifyStatusDrift,
    ReverifyLockDrift { lock: LockFile },
}

impl fmt::Display for SourceBindingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidExpectedCommit => write!(
                formatter,
                "EXPECTED_RELEASE_COMMIT is not exactly 40 lowercase hexadecimal characters; pass the full local commit object id"
            ),
            Self::CheckoutContainment => write!(
                formatter,
                "release checkout containment could not be established; use one real checkout directory without links or reparse points"
            ),
            Self::GitInvocationFailed { step } => write!(
                formatter,
                "local git {step} could not run through the selected executable; restore the selected Git executable and retry"
            ),
            Self::LocalCommitMissing => write!(
                formatter,
                "EXPECTED_RELEASE_COMMIT is not a commit in the local object database; provide a locally available full commit and retry"
            ),
            Self::WrongLineage => write!(
                formatter,
                "EXPECTED_RELEASE_COMMIT is not an ancestor of local HEAD; check out the intended local release lineage and retry"
            ),
            Self::HeadMalformed => write!(
                formatter,
                "local HEAD did not resolve to one full lowercase commit id; repair the local checkout and retry"
            ),
            Self::HeadMismatch => write!(
                formatter,
                "local HEAD does not equal EXPECTED_RELEASE_COMMIT; check out that exact local commit and retry"
            ),
            Self::DetachedHead => write!(
                formatter,
                "release checkout has detached HEAD; check out refs/heads/main or refs/heads/__swsync and retry"
            ),
            Self::CheckoutRefRejected => write!(
                formatter,
                "release checkout is not on refs/heads/main or refs/heads/__swsync; check out an allowed local branch and retry"
            ),
            Self::StatusUnavailable => write!(
                formatter,
                "local source status could not be read completely; repair the checkout and retry"
            ),
            Self::DirtyCheckout => write!(
                formatter,
                "release checkout has tracked, unmerged, submodule, or untracked source changes; restore a clean checkout and retry"
            ),
            Self::LockNotTracked { lock } => write!(
                formatter,
                "{} is not tracked by the release checkout; restore and commit the canonical lockfile before retrying",
                lock.label()
            ),
            Self::LockNotRegular { lock } => write!(
                formatter,
                "{} is not one regular contained file; replace it with the tracked regular lockfile and retry",
                lock.label()
            ),
            Self::ReverifyHeadDrift => write!(
                formatter,
                "release HEAD changed after source binding; restore the bound commit and restart finalization"
            ),
            Self::ReverifyRefDrift => write!(
                formatter,
                "release checkout ref changed after source binding; restore the bound branch and restart finalization"
            ),
            Self::ReverifyStatusDrift => write!(
                formatter,
                "release source status changed after source binding; restore a clean checkout and restart finalization"
            ),
            Self::ReverifyLockDrift { lock } => write!(
                formatter,
                "{} changed after source binding; restore the bound lockfile bytes and restart finalization",
                lock.label()
            ),
        }
    }
}

impl std::error::Error for SourceBindingError {}

impl LockFile {
    fn label(self) -> &'static str {
        match self {
            Self::Cargo => CARGO_LOCK,
            Self::UiPackage => UI_PACKAGE_LOCK,
        }
    }

    fn relative(self) -> &'static str {
        self.label()
    }
}

pub struct SourceBindingVerifier<'a, R: CommandRunner + ?Sized> {
    checkout: ContainedRoot,
    checkout_arg: String,
    git_program: PathBuf,
    runner: &'a R,
}

impl<'a, R: CommandRunner + ?Sized> SourceBindingVerifier<'a, R> {
    pub fn new(
        checkout_root: &Path,
        git_program: &Path,
        runner: &'a R,
    ) -> Result<Self, SourceBindingError> {
        let checkout = ContainedRoot::new(
            checkout_root,
            "release checkout",
            UnixModePolicy::AllowExecute,
        )
        .map_err(|_| SourceBindingError::CheckoutContainment)?;
        let checkout_arg = checkout
            .canonical_path()
            .to_str()
            .ok_or(SourceBindingError::CheckoutContainment)?
            .to_owned();
        Ok(Self {
            checkout,
            checkout_arg,
            git_program: git_program.to_path_buf(),
            runner,
        })
    }

    pub fn verify(&self, expected_commit: &str) -> Result<SourceBinding, SourceBindingError> {
        if !is_full_lower_hex_commit(expected_commit) {
            return Err(SourceBindingError::InvalidExpectedCommit);
        }

        let commit_expression = format!("{expected_commit}^{{commit}}");
        if self
            .git(&["cat-file", "-e", &commit_expression], "cat-file")?
            .status
            != 0
        {
            return Err(SourceBindingError::LocalCommitMissing);
        }
        if self
            .git(
                &["merge-base", "--is-ancestor", expected_commit, "HEAD"],
                "merge-base",
            )?
            .status
            != 0
        {
            return Err(SourceBindingError::WrongLineage);
        }

        let head = self.read_head()?;
        if head != expected_commit {
            return Err(SourceBindingError::HeadMismatch);
        }
        let checkout_ref = self.read_checkout_ref()?;
        self.require_clean_status()?;
        let cargo_lock_sha256 = self.read_lock_digest(LockFile::Cargo)?;
        let ui_package_lock_sha256 = self.read_lock_digest(LockFile::UiPackage)?;

        Ok(SourceBinding {
            commit: expected_commit.to_owned(),
            checkout_ref,
            cargo_lock_sha256,
            ui_package_lock_sha256,
        })
    }

    pub fn reverify(&self, binding: &SourceBinding) -> Result<(), SourceBindingError> {
        let head = self
            .git(&["rev-parse", "HEAD"], "rev-parse recheck")
            .and_then(|output| {
                if output.status == 0 {
                    parse_git_line(&output.stdout).ok_or(SourceBindingError::ReverifyHeadDrift)
                } else {
                    Err(SourceBindingError::ReverifyHeadDrift)
                }
            })?;
        if head != binding.commit {
            return Err(SourceBindingError::ReverifyHeadDrift);
        }

        let checkout_ref = self
            .git(&["symbolic-ref", "HEAD"], "symbolic-ref recheck")
            .and_then(|output| {
                if output.status == 0 {
                    parse_git_line(&output.stdout).ok_or(SourceBindingError::ReverifyRefDrift)
                } else {
                    Err(SourceBindingError::ReverifyRefDrift)
                }
            })?;
        if checkout_ref != binding.checkout_ref
            || (checkout_ref != MAIN_REF && checkout_ref != SYNC_REF)
        {
            return Err(SourceBindingError::ReverifyRefDrift);
        }

        let status = self.git(
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
            "status recheck",
        )?;
        if status.status != 0 || !status.stdout.is_empty() {
            return Err(SourceBindingError::ReverifyStatusDrift);
        }

        let cargo = self.read_lock_digest(LockFile::Cargo).map_err(|_| {
            SourceBindingError::ReverifyLockDrift {
                lock: LockFile::Cargo,
            }
        })?;
        if cargo != binding.cargo_lock_sha256 {
            return Err(SourceBindingError::ReverifyLockDrift {
                lock: LockFile::Cargo,
            });
        }
        let ui = self.read_lock_digest(LockFile::UiPackage).map_err(|_| {
            SourceBindingError::ReverifyLockDrift {
                lock: LockFile::UiPackage,
            }
        })?;
        if ui != binding.ui_package_lock_sha256 {
            return Err(SourceBindingError::ReverifyLockDrift {
                lock: LockFile::UiPackage,
            });
        }
        Ok(())
    }

    fn read_head(&self) -> Result<String, SourceBindingError> {
        let output = self.git(&["rev-parse", "HEAD"], "rev-parse")?;
        if output.status != 0 {
            return Err(SourceBindingError::HeadMalformed);
        }
        let head = parse_git_line(&output.stdout).ok_or(SourceBindingError::HeadMalformed)?;
        if !is_full_lower_hex_commit(&head) {
            return Err(SourceBindingError::HeadMalformed);
        }
        Ok(head)
    }

    fn read_checkout_ref(&self) -> Result<String, SourceBindingError> {
        let output = self.git(&["symbolic-ref", "HEAD"], "symbolic-ref")?;
        if output.status != 0 {
            return Err(SourceBindingError::DetachedHead);
        }
        let checkout_ref =
            parse_git_line(&output.stdout).ok_or(SourceBindingError::CheckoutRefRejected)?;
        if checkout_ref != MAIN_REF && checkout_ref != SYNC_REF {
            return Err(SourceBindingError::CheckoutRefRejected);
        }
        Ok(checkout_ref)
    }

    fn require_clean_status(&self) -> Result<(), SourceBindingError> {
        let output = self.git(
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
            "status",
        )?;
        if output.status != 0 {
            return Err(SourceBindingError::StatusUnavailable);
        }
        if !output.stdout.is_empty() {
            return Err(SourceBindingError::DirtyCheckout);
        }
        Ok(())
    }

    fn read_lock_digest(&self, lock: LockFile) -> Result<String, SourceBindingError> {
        let output = self.git(
            &["ls-files", "--error-unmatch", "--", lock.relative()],
            "ls-files",
        )?;
        if output.status != 0 {
            return Err(SourceBindingError::LockNotTracked { lock });
        }
        let bytes = self
            .checkout
            .read(lock.relative(), lock.label())
            .map_err(|_| SourceBindingError::LockNotRegular { lock })?;
        Ok(hex_sha256(&bytes))
    }

    fn git(&self, args: &[&str], step: &'static str) -> Result<CommandOutput, SourceBindingError> {
        let mut full_args = Vec::with_capacity(args.len() + 2);
        full_args.push("-C".to_owned());
        full_args.push(self.checkout_arg.clone());
        full_args.extend(args.iter().map(|arg| (*arg).to_owned()));
        self.runner
            .run(&self.git_program, &full_args, None, None)
            .map_err(|_| SourceBindingError::GitInvocationFailed { step })
    }
}

fn is_full_lower_hex_commit(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn parse_git_line(bytes: &[u8]) -> Option<String> {
    let mut line = bytes;
    if let Some(stripped) = line.strip_suffix(b"\n") {
        line = stripped;
    }
    if let Some(stripped) = line.strip_suffix(b"\r") {
        line = stripped;
    }
    if line.is_empty() || line.iter().any(|byte| byte.is_ascii_control()) {
        return None;
    }
    String::from_utf8(line.to_vec()).ok()
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
