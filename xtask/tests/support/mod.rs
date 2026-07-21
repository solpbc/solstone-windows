// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::collections::BTreeMap;
use std::fs::{self, FileTimes};
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde_json::{json, Value};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use xtask::release_advisory::{ADVISORY_DB_RELATIVE, RUSTSEC_SOURCE_ID};
use xtask::release_clock::UtcTimestamp;
use xtask::release_exec::{CommandOutput, CommandRunner, CommandRunnerError};
use xtask::release_finalizer::{FinalizeRequest, FinalizeRuntime};
use xtask::release_selection::SelectionMode;
use xtask::release_source_binding::SourceBinding;
use xtask::rust_release_manifest::{
    companion_basename, default_velopack_setup_basename, gather_checkout_facts_from_binding,
    render_release_evidence, validate_manifest_bytes, BundleNames, CheckoutFacts, ReleaseEvidence,
};
use xtask::version_gate::setup_exe_name;
use zip::write::{SimpleFileOptions, ZipWriter};
use zip::CompressionMethod;

pub const VERSION: &str = "0.2.11";
pub const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
pub const ADVISORY_COMMIT: &str = "89abcdef0123456789abcdef0123456789abcdef";
pub const ACQUIRED_AT: &str = "2026-07-21T10:00:00Z";
pub const CHECKED_AT: &str = "2026-07-21T12:00:00Z";
pub const GIT: &str = "/fake-tools/git";
pub const CARGO: &str = "/fake-tools/cargo";
pub const NPM: &str = "/fake-tools/npm";
pub const VPK: &str = "/fake-tools/vpk";
pub const POWERSHELL: &str = "/fake-tools/powershell";
pub const DOTNET: &str = "/fake-tools/dotnet";
pub const SMCTL: &str = "/fake-tools/smctl";
pub const SIGNTOOL: &str = "/fake-tools/signtool";
pub const UNSIGNED_APP_BYTES: &[u8] = b"inert unsigned release executable";
pub const SIGNED_APP_BYTES: &[u8] = b"inert signed release executable";

const ADVISORY_REPOSITORY: &str = "advisory-db-3157b0e258782691";
const ADVISORY_ARCHIVE: &[u8] = b"deterministic RustSec git archive bytes";
const PUBLIC_LEAF_UPPER: &str = "AC5472D41D5F63E339468E41F7B4438126E84860";

static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WitnessEvent {
    Phase(String),
    Invocation { program: PathBuf, args: Vec<String> },
}

/// One engine-boundary mutation per runner. Pure validation details remain in
/// the focused module tests; these hooks prove the transaction propagates them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RunnerMutation {
    #[default]
    None,
    SourceDirty,
    SourceUntracked,
    SourceUnmerged,
    SourceSubmodule,
    SourceObjectAbsent,
    SourceWrongLineage,
    SourceWrongHead,
    SourceWrongRef,
    SourceDetached,
    DetachedBoxMismatch,
    CargoLockUntracked,
    UiLockUntracked,
    LateHead,
    LateRef,
    LateStatus,
    LateCargoLock,
    LateUiLock,
    VersionAuthorityFailure,
    AdvisoryDirty,
    AdvisorySourceMismatch,
    AdvisoryShallow,
    AdvisoryArchiveMismatch,
    AdvisoryCommandFailure,
    SigningAuthFailure,
    NpmCiFailure,
    CargoBuildNoOutput,
    VpkMissingOutput,
    VpkExtraOutput,
    VpkDefaultSetupConflict,
    AssetsMissingInstaller,
    AssetsMalformed,
    AssetsDuplicateInstaller,
    AssetsChangedInstaller,
    DeltaFeedMissing,
    DeltaAssetsMissing,
    DeltaPackageMissing,
    ReleasesFullMissing,
    StagedExecutableDiverges,
    NupkgExecutableDiverges,
    PortableExecutableDiverges,
    NupkgMemberMissing,
    NupkgMemberDuplicate,
    SignToolFailure,
    HistoricalCandidateLeak,
    PostHashArtifactMutation,
    PostHashManifestMutation,
    ManifestDeltaEntryMissing,
    Phase8ArtifactMutation,
    PromotionTargetRace,
    ReceiptTargetRace,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NativeProofMutation {
    #[default]
    None,
    NonemptyProofRoot,
    PreexistingInstalledApp,
    PreexistingMatchingInstalledApp,
    InstallerNoOp,
    InstallerFailure,
    InstallerTimeout,
    InstallerSkipped,
    InstallerMissingApp,
    InstalledAppDiverges,
    DumpStateWrongVersion,
    DumpStateMalformed,
    SmokeMissingAppArgument,
    SmokeMissingVersionArgument,
    SmokeMissingHashArgument,
    SmokeFallbackEnabled,
    SmokeWrongHashTemplate,
    SmokeWrongVersionTemplate,
    SmokeFailure,
    SmokeMissingOk,
    SmokeMutatesArtifact,
    SmokeMutatesManifest,
    ReceiptPromotionRace,
}

pub struct FakeReleaseCheckout {
    root: PathBuf,
}

impl FakeReleaseCheckout {
    pub fn new(label: &str, delta_base: bool) -> Self {
        let root = std::env::temp_dir().join(format!(
            "solstone-finalizer-{label}-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("create fake release checkout");
        for relative in [
            "ui",
            "packaging",
            "target/release-advisory-db",
            "target/release",
        ] {
            fs::create_dir_all(root.join(relative)).expect("create fixture directory");
        }
        fs::write(root.join("Cargo.toml"), b"[workspace]\n").expect("write Cargo.toml");
        copy_workspace_file(&root, "Cargo.lock");
        copy_workspace_file(&root, "ui/package-lock.json");
        copy_workspace_file(&root, "deny.toml");
        copy_workspace_file(&root, "packaging/release-toolchain.json");
        copy_workspace_file(&root, "packaging/signing-policy.json");
        fs::write(
            root.join("CHANGELOG.md"),
            b"# Changelog\n\n## [0.2.11] - 2026-07-21\n\n- Deterministic inert release fixture.\n\n## [0.2.10] - 2026-07-01\n\n- Older.\n",
        )
        .expect("write changelog");

        let repository = root.join(ADVISORY_DB_RELATIVE).join(ADVISORY_REPOSITORY);
        fs::create_dir_all(repository.join(".git")).expect("create advisory git directory");
        fs::write(repository.join(".git/HEAD"), b"ref: refs/heads/main\n")
            .expect("write advisory HEAD");
        fs::write(repository.join(".git/FETCH_HEAD"), b"fetch evidence\n")
            .expect("write advisory FETCH_HEAD");
        fs::write(repository.join("README.md"), b"inert RustSec snapshot\n")
            .expect("write advisory snapshot file");
        let acquired = UtcTimestamp::parse(ACQUIRED_AT)
            .expect("parse acquisition time")
            .system_time();
        fs::OpenOptions::new()
            .write(true)
            .open(repository.join(".git/FETCH_HEAD"))
            .expect("open advisory FETCH_HEAD")
            .set_times(FileTimes::new().set_modified(acquired))
            .expect("set advisory acquisition time");

        if delta_base {
            fs::create_dir(root.join("Releases")).expect("create Releases");
            fs::write(
                root.join("Releases/Solstone-0.2.10-full.nupkg"),
                b"inert historical full package",
            )
            .expect("write historical full package");
        }
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn advisory_repository(&self) -> PathBuf {
        fs::canonicalize(
            self.root
                .join(ADVISORY_DB_RELATIVE)
                .join(ADVISORY_REPOSITORY),
        )
        .expect("canonical advisory repository")
    }

    pub fn runtime<'a>(&'a self, signing_alias: Option<&'a str>) -> FinalizeRuntime<'a> {
        FinalizeRuntime {
            checkout_root: &self.root,
            git_program: Path::new(GIT),
            advisory_tree_sha256: advisory_tree_sha256_static(),
            signing_keypair_alias: signing_alias,
        }
    }
}

impl Drop for FakeReleaseCheckout {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove fake release checkout");
    }
}

pub struct FakeReleaseRunner {
    checkout: PathBuf,
    advisory_repository: PathBuf,
    reverse_output_order: bool,
    mutation: RunnerMutation,
    native_proof_mutation: NativeProofMutation,
    source_head_reads: AtomicU64,
    source_ref_reads: AtomicU64,
    source_status_reads: AtomicU64,
    events: Mutex<Vec<WitnessEvent>>,
}

impl FakeReleaseRunner {
    pub fn new(checkout: &FakeReleaseCheckout, reverse_output_order: bool) -> Self {
        Self::with_mutations(
            checkout,
            reverse_output_order,
            RunnerMutation::None,
            NativeProofMutation::None,
        )
    }

    pub fn with_mutation(
        checkout: &FakeReleaseCheckout,
        reverse_output_order: bool,
        mutation: RunnerMutation,
    ) -> Self {
        Self::with_mutations(
            checkout,
            reverse_output_order,
            mutation,
            NativeProofMutation::None,
        )
    }

    #[allow(dead_code)]
    pub fn with_native_proof_mutation(
        checkout: &FakeReleaseCheckout,
        mutation: NativeProofMutation,
    ) -> Self {
        Self::with_mutations(checkout, false, RunnerMutation::None, mutation)
    }

    fn with_mutations(
        checkout: &FakeReleaseCheckout,
        reverse_output_order: bool,
        mutation: RunnerMutation,
        native_proof_mutation: NativeProofMutation,
    ) -> Self {
        Self {
            checkout: fs::canonicalize(checkout.root()).expect("canonical fake checkout"),
            advisory_repository: fs::canonicalize(checkout.root())
                .expect("canonical fake checkout")
                .join(ADVISORY_DB_RELATIVE)
                .join(ADVISORY_REPOSITORY),
            reverse_output_order,
            mutation,
            native_proof_mutation,
            source_head_reads: AtomicU64::new(0),
            source_ref_reads: AtomicU64::new(0),
            source_status_reads: AtomicU64::new(0),
            events: Mutex::new(Vec::new()),
        }
    }

    pub fn events(&self) -> Vec<WitnessEvent> {
        self.events.lock().expect("read fake witness").clone()
    }

    fn output(stdout: impl Into<Vec<u8>>) -> CommandOutput {
        CommandOutput {
            status: 0,
            stdout: stdout.into(),
            stderr: Vec::new(),
        }
    }

    fn failure(stderr: &'static [u8]) -> CommandOutput {
        CommandOutput {
            status: 1,
            stdout: Vec::new(),
            stderr: stderr.to_vec(),
        }
    }

    fn candidate_temp(&self) -> Result<PathBuf, CommandRunnerError> {
        let parent = self.checkout.join("target/release-candidate");
        let mut matches = fs::read_dir(parent)
            .map_err(|_| CommandRunnerError::UnexpectedInvocation)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with(&format!(".{VERSION}.finalize-")) && name.ends_with(".tmp")
                    })
            });
        let candidate = matches
            .next()
            .ok_or(CommandRunnerError::UnexpectedInvocation)?;
        if matches.next().is_some() {
            return Err(CommandRunnerError::UnexpectedInvocation);
        }
        Ok(candidate)
    }

    fn native_proof_root(&self) -> Result<PathBuf, CommandRunnerError> {
        let parent = self
            .checkout
            .join(format!("target/release-native-proof/{VERSION}"));
        let mut roots = fs::read_dir(parent)
            .map_err(|_| CommandRunnerError::UnexpectedInvocation)?
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_name().to_str().is_some_and(|name| {
                    name.starts_with(".native-proof-") && name.ends_with(".tmp")
                })
            })
            .map(|entry| entry.path());
        let root = roots
            .next()
            .ok_or(CommandRunnerError::UnexpectedInvocation)?;
        if roots.next().is_some() {
            return Err(CommandRunnerError::UnexpectedInvocation);
        }
        Ok(root)
    }

    fn apply_late_mutation(&self) -> Result<(), CommandRunnerError> {
        let write = |path: &Path, bytes: &[u8]| {
            fs::write(path, bytes).map_err(|_| CommandRunnerError::UnexpectedInvocation)
        };
        match self.mutation {
            RunnerMutation::LateCargoLock => write(
                &self.checkout.join("Cargo.lock"),
                b"late Cargo.lock drift\n",
            )?,
            RunnerMutation::LateUiLock => write(
                &self.checkout.join("ui/package-lock.json"),
                b"late UI lock drift\n",
            )?,
            RunnerMutation::PostHashArtifactMutation => write(
                &self.candidate_temp()?.join("assets.win.json"),
                b"post-hash artifact mutation\n",
            )?,
            RunnerMutation::PostHashManifestMutation => write(
                &self
                    .candidate_temp()?
                    .join("solstone-windows-x86_64-pc-windows-msvc.rust-release-manifest.json"),
                b"post-hash manifest mutation\n",
            )?,
            RunnerMutation::ManifestDeltaEntryMissing => {
                let manifest = self
                    .candidate_temp()?
                    .join("solstone-windows-x86_64-pc-windows-msvc.rust-release-manifest.json");
                let mut value: Value = serde_json::from_slice(
                    &fs::read(&manifest).map_err(|_| CommandRunnerError::UnexpectedInvocation)?,
                )
                .map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
                value["artifacts"]
                    .as_array_mut()
                    .ok_or(CommandRunnerError::UnexpectedInvocation)?
                    .retain(|artifact| {
                        artifact["filename"]
                            .as_str()
                            .is_none_or(|name| !name.ends_with("-delta.nupkg"))
                    });
                write(
                    &manifest,
                    &serde_json::to_vec(&value)
                        .map_err(|_| CommandRunnerError::UnexpectedInvocation)?,
                )?;
            }
            _ => {}
        }
        Ok(())
    }

    fn run_git(&self, args: &[String]) -> Result<CommandOutput, CommandRunnerError> {
        if args.len() < 3 || args[0] != "-C" {
            return Err(CommandRunnerError::UnexpectedInvocation);
        }
        let repository = Path::new(&args[1]);
        let tail: Vec<&str> = args[2..].iter().map(String::as_str).collect();
        if repository == self.checkout {
            match tail.as_slice() {
                ["cat-file", "-e", expression] if *expression == format!("{COMMIT}^{{commit}}") => {
                    if self.mutation == RunnerMutation::SourceObjectAbsent {
                        Ok(Self::failure(b"object absent"))
                    } else {
                        Ok(Self::output(Vec::new()))
                    }
                }
                ["merge-base", "--is-ancestor", commit, "HEAD"] if *commit == COMMIT => {
                    if self.mutation == RunnerMutation::SourceWrongLineage {
                        Ok(Self::failure(b"wrong lineage"))
                    } else {
                        Ok(Self::output(Vec::new()))
                    }
                }
                ["rev-parse", "HEAD"] => {
                    let read = self.source_head_reads.fetch_add(1, Ordering::Relaxed);
                    if read == 1 {
                        self.apply_late_mutation()?;
                    }
                    if matches!(
                        self.mutation,
                        RunnerMutation::SourceWrongHead | RunnerMutation::DetachedBoxMismatch
                    ) || (self.mutation == RunnerMutation::LateHead && read == 1)
                    {
                        Ok(Self::output(
                            b"fedcba9876543210fedcba9876543210fedcba98\n".to_vec(),
                        ))
                    } else {
                        Ok(Self::output(format!("{COMMIT}\n")))
                    }
                }
                ["symbolic-ref", "HEAD"] => {
                    let read = self.source_ref_reads.fetch_add(1, Ordering::Relaxed);
                    if self.mutation == RunnerMutation::SourceDetached {
                        Ok(Self::failure(b"detached"))
                    } else if self.mutation == RunnerMutation::SourceWrongRef
                        || (self.mutation == RunnerMutation::LateRef && read == 1)
                    {
                        Ok(Self::output(b"refs/heads/release-wrong\n".to_vec()))
                    } else {
                        Ok(Self::output(b"refs/heads/main\n".to_vec()))
                    }
                }
                ["status", "--porcelain=v1", "-z", "--untracked-files=all", "--ignore-submodules=none"] =>
                {
                    let read = self.source_status_reads.fetch_add(1, Ordering::Relaxed);
                    let dirty = match self.mutation {
                        RunnerMutation::SourceDirty => Some(b" M Cargo.toml\0".as_slice()),
                        RunnerMutation::SourceUntracked => Some(b"?? private.tmp\0".as_slice()),
                        RunnerMutation::SourceUnmerged => Some(b"UU Cargo.toml\0".as_slice()),
                        RunnerMutation::SourceSubmodule => {
                            Some(b" M crates/submodule\0".as_slice())
                        }
                        RunnerMutation::LateStatus if read == 1 => {
                            Some(b" M Cargo.toml\0".as_slice())
                        }
                        _ => None,
                    };
                    Ok(Self::output(dirty.unwrap_or_default().to_vec()))
                }
                ["ls-files", "--error-unmatch", "--", "Cargo.lock"] => {
                    if self.mutation == RunnerMutation::CargoLockUntracked {
                        Ok(Self::failure(b"not tracked"))
                    } else {
                        Ok(Self::output(Vec::new()))
                    }
                }
                ["ls-files", "--error-unmatch", "--", "ui/package-lock.json"] => {
                    if self.mutation == RunnerMutation::UiLockUntracked {
                        Ok(Self::failure(b"not tracked"))
                    } else {
                        Ok(Self::output(Vec::new()))
                    }
                }
                _ => Err(CommandRunnerError::UnexpectedInvocation),
            }
        } else if repository == self.advisory_repository {
            match tail.as_slice() {
                ["status", "--porcelain=v1", "-z", "--untracked-files=all", "--ignored"] => {
                    if self.mutation == RunnerMutation::AdvisoryDirty {
                        Ok(Self::output(b"?? injected-advisory\0".to_vec()))
                    } else {
                        Ok(Self::output(Vec::new()))
                    }
                }
                ["remote", "get-url", "origin"] => {
                    if self.mutation == RunnerMutation::AdvisorySourceMismatch {
                        Ok(Self::output(
                            b"https://github.com/other/advisory-db\n".to_vec(),
                        ))
                    } else {
                        Ok(Self::output(format!("{RUSTSEC_SOURCE_ID}\n")))
                    }
                }
                ["rev-parse", "HEAD^{commit}"] => Ok(Self::output(format!("{ADVISORY_COMMIT}\n"))),
                ["rev-parse", "--is-shallow-repository"] => Ok(Self::output(
                    if self.mutation == RunnerMutation::AdvisoryShallow {
                        b"true\n".to_vec()
                    } else {
                        b"false\n".to_vec()
                    },
                )),
                ["archive", "--format=tar", "HEAD"] => Ok(Self::output(
                    if self.mutation == RunnerMutation::AdvisoryArchiveMismatch {
                        b"swapped advisory archive".to_vec()
                    } else {
                        ADVISORY_ARCHIVE.to_vec()
                    },
                )),
                _ => Err(CommandRunnerError::UnexpectedInvocation),
            }
        } else {
            Err(CommandRunnerError::UnexpectedInvocation)
        }
    }

    fn run_cargo(
        &self,
        args: &[String],
        env: Option<&BTreeMap<String, String>>,
    ) -> Result<CommandOutput, CommandRunnerError> {
        match args.first().map(String::as_str) {
            Some("metadata") => Ok(
                if self.mutation == RunnerMutation::VersionAuthorityFailure {
                    Self::failure(b"metadata authority unavailable")
                } else {
                    Self::output(
                        serde_json::to_vec(&json!({
                            "packages": [{
                                "name": "solstone-windows-app",
                                "version": VERSION
                            }]
                        }))
                        .expect("render metadata"),
                    )
                },
            ),
            Some("deny") => Ok(if self.mutation == RunnerMutation::AdvisoryCommandFailure {
                Self::failure(b"offline advisory rejection")
            } else {
                Self::output(Vec::new())
            }),
            Some("build") => {
                let environment = env.ok_or(CommandRunnerError::UnexpectedInvocation)?;
                if !environment.contains_key("VCToolsVersion")
                    || !environment.contains_key("WindowsSDKVersion")
                {
                    return Err(CommandRunnerError::UnexpectedInvocation);
                }
                if self.mutation == RunnerMutation::CargoBuildNoOutput {
                    return Ok(Self::output(Vec::new()));
                }
                let cargo_target = environment
                    .get("CARGO_TARGET_DIR")
                    .ok_or(CommandRunnerError::UnexpectedInvocation)?;
                fs::create_dir_all(Path::new(cargo_target).join("release"))
                    .map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
                fs::write(
                    Path::new(cargo_target).join("release/solstone-windows-app.exe"),
                    UNSIGNED_APP_BYTES,
                )
                .map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
                Ok(Self::output(Vec::new()))
            }
            _ => Err(CommandRunnerError::UnexpectedInvocation),
        }
    }

    fn run_vpk(&self, args: &[String]) -> Result<CommandOutput, CommandRunnerError> {
        let stage = argument_after(args, "--packDir")?;
        let output = argument_after(args, "--outputDir")?;
        let signed = args.iter().any(|arg| arg == "--signTemplate");
        if signed {
            fs::write(
                Path::new(stage).join("solstone-windows-app.exe"),
                SIGNED_APP_BYTES,
            )
            .map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
        }
        let app_bytes = fs::read(Path::new(stage).join("solstone-windows-app.exe"))
            .map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
        emit_velopack_output(
            Path::new(output),
            &app_bytes,
            signed,
            self.reverse_output_order,
            self.mutation,
        )?;
        if self.mutation == RunnerMutation::StagedExecutableDiverges {
            fs::write(
                Path::new(stage).join("solstone-windows-app.exe"),
                b"divergent staged executable",
            )
            .map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
        }
        Ok(Self::output(Vec::new()))
    }
}

impl CommandRunner for FakeReleaseRunner {
    fn record_phase(&self, phase: &'static str) -> Result<(), CommandRunnerError> {
        self.events
            .lock()
            .map_err(|_| CommandRunnerError::FakeStatePoisoned)?
            .push(WitnessEvent::Phase(phase.to_owned()));
        if phase == xtask::release_finalizer::PHASE_7_EVIDENCE
            && self.mutation == RunnerMutation::HistoricalCandidateLeak
        {
            fs::write(
                self.candidate_temp()?.join("Solstone-0.2.10-full.nupkg"),
                b"leaked historical package",
            )
            .map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
        }
        if phase == xtask::release_finalizer::PHASE_8_PROMOTION {
            if self.mutation == RunnerMutation::Phase8ArtifactMutation {
                fs::write(
                    self.candidate_temp()?.join("assets.win.json"),
                    b"phase-eight post-hash mutation",
                )
                .map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
            }
            match self.mutation {
                RunnerMutation::PromotionTargetRace => {
                    fs::create_dir_all(
                        self.checkout
                            .join(format!("target/release-candidate/{VERSION}")),
                    )
                    .map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
                }
                RunnerMutation::ReceiptTargetRace => {
                    fs::write(
                        self.checkout.join(format!(
                            "target/release-evidence/{VERSION}/rust-release-finalization.json"
                        )),
                        b"partial invalid receipt",
                    )
                    .map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
                }
                _ => {}
            }
        }
        if phase == xtask::native_release_proof::STEP_5_ROOT_READY {
            let proof_root = self.native_proof_root()?;
            match self.native_proof_mutation {
                NativeProofMutation::NonemptyProofRoot => {
                    fs::write(proof_root.join("unexpected-private-state"), b"private")
                        .map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
                }
                NativeProofMutation::PreexistingInstalledApp
                | NativeProofMutation::PreexistingMatchingInstalledApp => {
                    let installed = proof_root.join("Solstone/current/solstone-windows-app.exe");
                    fs::create_dir_all(
                        installed
                            .parent()
                            .ok_or(CommandRunnerError::UnexpectedInvocation)?,
                    )
                    .map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
                    let bytes = if self.native_proof_mutation
                        == NativeProofMutation::PreexistingMatchingInstalledApp
                    {
                        SIGNED_APP_BYTES
                    } else {
                        b"preexisting divergent app"
                    };
                    fs::write(installed, bytes)
                        .map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
                }
                _ => {}
            }
        }
        if phase == xtask::native_release_proof::STEP_11_RECEIPT_STAGED
            && self.native_proof_mutation == NativeProofMutation::ReceiptPromotionRace
        {
            fs::write(
                self.checkout.join(format!(
                    "target/release-evidence/{VERSION}/windows-native-proof.json"
                )),
                b"partial promotion race",
            )
            .map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
        }
        Ok(())
    }

    fn run(
        &self,
        program: &Path,
        args: &[String],
        _stdin: Option<&[u8]>,
        env: Option<&BTreeMap<String, String>>,
    ) -> Result<CommandOutput, CommandRunnerError> {
        self.events
            .lock()
            .map_err(|_| CommandRunnerError::FakeStatePoisoned)?
            .push(WitnessEvent::Invocation {
                program: program.to_path_buf(),
                args: args.to_vec(),
            });
        match program.to_str() {
            Some(GIT) => self.run_git(args),
            Some(CARGO) => self.run_cargo(args, env),
            Some(NPM) if args.starts_with(&["--prefix".to_owned(), "ui".to_owned()]) => Ok(
                if self.mutation == RunnerMutation::NpmCiFailure
                    && args.iter().any(|arg| arg == "ci")
                {
                    Self::failure(b"offline npm cache missing")
                } else {
                    Self::output(Vec::new())
                },
            ),
            Some(VPK) => self.run_vpk(args),
            Some(POWERSHELL)
                if args
                    .iter()
                    .any(|arg| arg.ends_with("packaging/preflight-release-tools.ps1")) =>
            {
                if !args.iter().any(|arg| arg == "-Sign") || env.is_some() {
                    return Err(CommandRunnerError::UnexpectedInvocation);
                }
                Ok(Self::output(native_proof_selection_record(
                    self.native_proof_mutation,
                )))
            }
            Some(POWERSHELL)
                if args
                    .iter()
                    .any(|arg| arg == "packaging/signing/preflight-auth.ps1") =>
            {
                Ok(if self.mutation == RunnerMutation::SigningAuthFailure {
                    Self::failure(b"signing authentication unavailable")
                } else {
                    Self::output(Vec::new())
                })
            }
            Some(SIGNTOOL) => Ok(if self.mutation == RunnerMutation::SignToolFailure {
                Self::failure(b"signature verification failed")
            } else {
                Self::output(accepted_signtool_grammar().into_bytes())
            }),
            Some(POWERSHELL) if args.iter().any(|arg| arg == "scripts/smoke.ps1") => {
                self.run_native_smoke(args, env)
            }
            Some(program) if Path::new(program) == self.native_setup_path() => {
                self.run_native_installer(args, env)
            }
            Some(program) if program.ends_with("/Solstone/current/solstone-windows-app.exe") => {
                if args != ["--dump-state"] || env.is_none() {
                    return Err(CommandRunnerError::UnexpectedInvocation);
                }
                match self.native_proof_mutation {
                    NativeProofMutation::DumpStateMalformed => Ok(Self::output(b"{".to_vec())),
                    NativeProofMutation::DumpStateWrongVersion => Ok(Self::output(
                        serde_json::to_vec(&json!({
                            "version": "0.2.10",
                            "app_state": "idle"
                        }))
                        .map_err(|_| CommandRunnerError::UnexpectedInvocation)?,
                    )),
                    _ => Ok(Self::output(
                        serde_json::to_vec(&json!({"version": VERSION, "app_state": "idle"}))
                            .map_err(|_| CommandRunnerError::UnexpectedInvocation)?,
                    )),
                }
            }
            _ => Err(CommandRunnerError::UnexpectedInvocation),
        }
    }
}

impl FakeReleaseRunner {
    fn native_setup_path(&self) -> PathBuf {
        self.checkout.join(format!(
            "target/release-candidate/{VERSION}/solstone-setup-{VERSION}.exe"
        ))
    }

    fn run_native_installer(
        &self,
        args: &[String],
        env: Option<&BTreeMap<String, String>>,
    ) -> Result<CommandOutput, CommandRunnerError> {
        if args.len() != 3 || args[0] != "--silent" || args[1] != "--installto" {
            return Err(CommandRunnerError::UnexpectedInvocation);
        }
        let install_root = PathBuf::from(&args[2]);
        let local_app_data = install_root
            .parent()
            .ok_or(CommandRunnerError::UnexpectedInvocation)?;
        if env
            .and_then(|values| values.get("LOCALAPPDATA"))
            .map(String::as_str)
            != local_app_data.to_str()
        {
            return Err(CommandRunnerError::UnexpectedInvocation);
        }
        let installed = install_root.join("current/solstone-windows-app.exe");
        match self.native_proof_mutation {
            NativeProofMutation::InstallerFailure => return Ok(Self::failure(b"install failed")),
            NativeProofMutation::InstallerTimeout => {
                return Ok(CommandOutput {
                    status: 124,
                    stdout: Vec::new(),
                    stderr: b"install timed out".to_vec(),
                });
            }
            NativeProofMutation::InstallerSkipped => {
                return Ok(CommandOutput {
                    status: 3,
                    stdout: b"install skipped".to_vec(),
                    stderr: Vec::new(),
                });
            }
            NativeProofMutation::InstallerNoOp => return Ok(Self::output(Vec::new())),
            NativeProofMutation::InstallerMissingApp => {
                fs::create_dir_all(install_root.join("current"))
                    .map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
                return Ok(Self::output(Vec::new()));
            }
            _ => {}
        }
        fs::create_dir_all(
            installed
                .parent()
                .ok_or(CommandRunnerError::UnexpectedInvocation)?,
        )
        .map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
        let app_bytes = if self.native_proof_mutation == NativeProofMutation::InstalledAppDiverges {
            b"divergent installed executable".as_slice()
        } else {
            SIGNED_APP_BYTES
        };
        fs::write(installed, app_bytes).map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
        Ok(Self::output(Vec::new()))
    }

    fn run_native_smoke(
        &self,
        args: &[String],
        env: Option<&BTreeMap<String, String>>,
    ) -> Result<CommandOutput, CommandRunnerError> {
        let installed = PathBuf::from(argument_after(args, "-AppExe")?);
        let expected_version = argument_after(args, "-ExpectedVersion")?;
        let expected_sha256 = argument_after(args, "-ExpectedSha256")?;
        let dotnet = argument_after(args, "-DotnetPath")?;
        if expected_version != VERSION
            || expected_sha256 != hex_sha256(SIGNED_APP_BYTES)
            || dotnet != DOTNET
            || args.iter().filter(|arg| *arg == "-AppExe").count() != 1
            || args
                .iter()
                .filter(|arg| *arg == "-DisableInstalledFallback")
                .count()
                != 1
            || fs::read(&installed).map_err(|_| CommandRunnerError::UnexpectedInvocation)?
                != SIGNED_APP_BYTES
        {
            return Err(CommandRunnerError::UnexpectedInvocation);
        }
        let local_app_data = installed
            .ancestors()
            .nth(3)
            .ok_or(CommandRunnerError::UnexpectedInvocation)?;
        if env
            .and_then(|values| values.get("LOCALAPPDATA"))
            .map(String::as_str)
            != local_app_data.to_str()
        {
            return Err(CommandRunnerError::UnexpectedInvocation);
        }
        match self.native_proof_mutation {
            NativeProofMutation::SmokeFailure => Ok(Self::failure(b"health/render failed")),
            NativeProofMutation::SmokeMissingOk => {
                Ok(Self::output(b"health/render gate passed\n".to_vec()))
            }
            NativeProofMutation::SmokeMutatesArtifact => {
                fs::write(
                    self.checkout.join(format!(
                        "target/release-candidate/{VERSION}/assets.win.json"
                    )),
                    b"mutated during smoke",
                )
                .map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
                Ok(Self::output(b"SMOKE_OK\n".to_vec()))
            }
            NativeProofMutation::SmokeMutatesManifest => {
                mutate_companion_after_smoke(&self.checkout)?;
                Ok(Self::output(b"SMOKE_OK\n".to_vec()))
            }
            _ => Ok(Self::output(
                b"health/render gate passed\nSMOKE_OK\n".to_vec(),
            )),
        }
    }
}

pub fn request(mode: SelectionMode, delta: bool) -> FinalizeRequest {
    FinalizeRequest {
        expected_release_commit: COMMIT.to_owned(),
        sign_mode: mode,
        selection_record: selection_record(mode),
        delta_base_fulls: if delta {
            vec!["Solstone-0.2.10-full.nupkg".to_owned()]
        } else {
            Vec::new()
        },
    }
}

pub fn selection_record(mode: SelectionMode) -> Vec<u8> {
    serde_json::to_vec(&selection_value(mode)).expect("render fake selection")
}

fn native_proof_selection_record(mutation: NativeProofMutation) -> Vec<u8> {
    let mut value = selection_value(SelectionMode::Signed);
    let argv = value["actions"]["native_smoke"]["argv"]
        .as_array_mut()
        .expect("native smoke argv");
    let remove_pair = |argv: &mut Vec<Value>, flag: &str| {
        let index = argv
            .iter()
            .position(|argument| argument.as_str() == Some(flag))
            .expect("native smoke flag");
        argv.drain(index..=index + 1);
    };
    match mutation {
        NativeProofMutation::SmokeMissingAppArgument => remove_pair(argv, "-AppExe"),
        NativeProofMutation::SmokeMissingVersionArgument => remove_pair(argv, "-ExpectedVersion"),
        NativeProofMutation::SmokeMissingHashArgument => remove_pair(argv, "-ExpectedSha256"),
        NativeProofMutation::SmokeFallbackEnabled => {
            argv.retain(|argument| argument.as_str() != Some("-DisableInstalledFallback"));
        }
        NativeProofMutation::SmokeWrongHashTemplate => {
            let value = argv
                .iter_mut()
                .find(|argument| argument.as_str() == Some("{expected_sha256}"))
                .expect("expected hash placeholder");
            *value = Value::String("0".repeat(64));
        }
        NativeProofMutation::SmokeWrongVersionTemplate => {
            let value = argv
                .iter_mut()
                .find(|argument| argument.as_str() == Some("{expected_version}"))
                .expect("expected version placeholder");
            *value = Value::String("0.2.10".to_owned());
        }
        _ => {}
    }
    serde_json::to_vec(&value).expect("render native proof selection")
}

fn mutate_companion_after_smoke(checkout: &Path) -> Result<(), CommandRunnerError> {
    let path = checkout
        .join(format!("target/release-candidate/{VERSION}"))
        .join(companion_basename());
    let bytes = fs::read(&path).map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
    let manifest =
        validate_manifest_bytes(&bytes).map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
    let mut evidence = ReleaseEvidence::from(manifest);
    evidence.dependency_policy.advisory_checked_at = "2026-07-21T12:00:01Z".to_owned();
    let changed =
        render_release_evidence(&evidence).map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
    fs::write(path, changed).map_err(|_| CommandRunnerError::UnexpectedInvocation)
}

pub fn checkout_facts(checkout: &FakeReleaseCheckout) -> CheckoutFacts {
    let binding = SourceBinding {
        commit: COMMIT.to_owned(),
        checkout_ref: "refs/heads/main".to_owned(),
        cargo_lock_sha256: hex_sha256(
            &fs::read(checkout.root().join("Cargo.lock")).expect("read fake Cargo.lock"),
        ),
        ui_package_lock_sha256: hex_sha256(
            &fs::read(checkout.root().join("ui/package-lock.json")).expect("read fake UI lock"),
        ),
    };
    gather_checkout_facts_from_binding(checkout.root(), VERSION, &binding)
        .expect("gather fake checkout facts")
}

pub fn advisory_tree_sha256() -> String {
    hex_sha256(ADVISORY_ARCHIVE)
}

fn advisory_tree_sha256_static() -> &'static str {
    static DIGEST: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DIGEST.get_or_init(advisory_tree_sha256).as_str()
}

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has workspace parent")
}

fn copy_workspace_file(root: &Path, relative: &str) {
    let destination = root.join(relative);
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).expect("create copied-file parent");
    }
    fs::copy(workspace_root().join(relative), destination).expect("copy workspace fixture file");
}

fn action(program: &str, argv: &[&str]) -> Value {
    json!({"program": program, "argv": argv})
}

fn selection_value(mode: SelectionMode) -> Value {
    let mut tools = json!({
        "rustc": {"path": "/fake-tools/rustc", "version": "1.96.0", "host": "x86_64-pc-windows-msvc"},
        "cargo": {"path": CARGO, "version": "1.96.0"},
        "cargo-deny": {"path": "/fake-tools/cargo-deny", "version": "0.20.2"},
        "dotnet": {"path": DOTNET, "version": "8.0.422"},
        "vpk": {"path": VPK, "version": "1.2.0", "packageId": "vpk"},
        "node": {"path": "/fake-tools/node", "version": "24.16.0"},
        "npm": {"path": NPM, "version": "11.13.0"},
        "msvc-cl": {
            "path": "/fake-tools/VC/bin/cl.exe", "compilerVersion": "19.44.35228",
            "toolsetVersion": "14.44.35207", "host": "x64", "target": "x64",
            "vcvarsallPath": "/fake-tools/VC/vcvarsall.bat",
            "vcvarsVersionArg": "-vcvars_ver=14.44.35207",
            "installationPath": "/fake-tools/VisualStudio"
        },
        "windows-sdk": {"path": "/fake-tools/WindowsKits", "version": "10.0.26100.0"},
        "powershell": {"path": POWERSHELL, "version": "5.1"}
    });
    let mut actions = json!({
        "npm_ci": action(NPM, &["--prefix", "ui", "ci", "--offline"]),
        "npm_build": action(NPM, &["--prefix", "ui", "run", "build"]),
        "cargo_release_build": action(CARGO, &["build", "--locked", "-p", "solstone-windows-app", "--release", "--features", "custom-protocol"]),
        "vpk_pack": action(VPK, &["pack", "--packId", "Solstone", "--packVersion", "{version}", "--packDir", "{stage_dir}", "--mainExe", "solstone-windows-app.exe", "--outputDir", "{output_dir}", "--packTitle", "sol", "--packAuthors", "sol pbc", "--icon", "src-tauri/icons/icon.ico", "--channel", "win", "--framework", "webview2", "--releaseNotes", "{release_notes}"]),
        "cargo_deny_advisories": action(CARGO, &["deny", "--locked", "--offline", "--config", "{advisory_config}", "check", "advisories"]),
        "native_smoke": action(POWERSHELL, &["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "scripts/smoke.ps1", "-AppExe", "{installed_exe}", "-ExpectedVersion", "{expected_version}", "-ExpectedSha256", "{expected_sha256}", "-DisableInstalledFallback", "-DotnetPath", "{dotnet_path}"])
    });
    if mode == SelectionMode::Signed {
        tools["smctl"] = json!({"path": SMCTL, "version": "1.64.2"});
        tools["signtool"] = json!({"path": SIGNTOOL, "version": "10.0.26100.7705", "originalFilename": "SIGNTOOL.EXE"});
        actions["signing_auth_preflight"] = action(
            POWERSHELL,
            &[
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                "packaging/signing/preflight-auth.ps1",
                "-SmctlPath",
                "{smctl_path}",
            ],
        );
        actions["smctl_sign"] = action(
            SMCTL,
            &[
                "sign",
                "--keypair-alias",
                "{keypair_alias}",
                "--input",
                "{file}",
            ],
        );
        actions["signtool_verify"] = action(SIGNTOOL, &["verify", "/pa", "/all", "/v", "{file}"]);
    }
    json!({
        "schema": "solstone.release-tool-selection.v1",
        "mode": if mode == SelectionMode::Signed { "signed" } else { "unsigned" },
        "tools": tools,
        "actions": actions,
        "msvc_environment": {
            "PATH": "/fake-tools/VC/bin;/fake-tools/WindowsKits/bin",
            "INCLUDE": "/fake-tools/VC/include", "LIB": "/fake-tools/VC/lib",
            "LIBPATH": "/fake-tools/VC/libpath", "VCINSTALLDIR": "/fake-tools/VC",
            "VCToolsInstallDir": "/fake-tools/VC/Tools/MSVC/14.44.35207",
            "VCToolsVersion": "14.44.35207", "UniversalCRTSdkDir": "/fake-tools/WindowsKits",
            "UCRTVersion": "10.0.26100.0", "WindowsSdkDir": "/fake-tools/WindowsKits",
            "WindowsSdkBinPath": "/fake-tools/WindowsKits/bin",
            "WindowsLibPath": "/fake-tools/WindowsKits/UnionMetadata",
            "WindowsSDKVersion": "10.0.26100.0"
        }
    })
}

fn argument_after<'a>(args: &'a [String], name: &str) -> Result<&'a str, CommandRunnerError> {
    let index = args
        .iter()
        .position(|arg| arg == name)
        .ok_or(CommandRunnerError::UnexpectedInvocation)?;
    args.get(index + 1)
        .map(String::as_str)
        .ok_or(CommandRunnerError::UnexpectedInvocation)
}

fn emit_velopack_output(
    output: &Path,
    app_bytes: &[u8],
    signed: bool,
    reverse_order: bool,
    mutation: RunnerMutation,
) -> Result<(), CommandRunnerError> {
    let historical: Vec<String> = fs::read_dir(output)
        .map_err(|_| CommandRunnerError::UnexpectedInvocation)?
        .map(|entry| {
            entry
                .map_err(|_| CommandRunnerError::UnexpectedInvocation)?
                .file_name()
                .into_string()
                .map_err(|_| CommandRunnerError::UnexpectedInvocation)
        })
        .collect::<Result<_, _>>()?;
    let has_delta = !historical.is_empty();
    let canonical_names = BundleNames::for_version(VERSION);
    let full_name = canonical_names.full_package().to_owned();
    let delta_name = canonical_names.delta_package().to_owned();
    let portable_name = canonical_names.portable().to_owned();
    let assets_name = canonical_names.assets().to_owned();
    let releases_name = canonical_names.releases().to_owned();
    let feed_name = canonical_names.release_feed().to_owned();
    let default_setup_name = default_velopack_setup_basename();
    let full = match mutation {
        RunnerMutation::NupkgExecutableDiverges => build_zip(
            "lib/app/solstone-windows-app.exe",
            b"divergent nupkg executable",
        ),
        RunnerMutation::NupkgMemberMissing => build_zip("lib/app/not-the-app.exe", app_bytes),
        RunnerMutation::NupkgMemberDuplicate => build_zip_members(&[
            ("lib/app/solstone-windows-app.exe", app_bytes),
            ("lib/app/SOLSTONE-windows-app.exe", app_bytes),
        ]),
        _ => build_zip("lib/app/solstone-windows-app.exe", app_bytes),
    };
    let portable = if mutation == RunnerMutation::PortableExecutableDiverges {
        build_zip(
            "current/solstone-windows-app.exe",
            b"divergent portable executable",
        )
    } else {
        build_zip("current/solstone-windows-app.exe", app_bytes)
    };
    let delta = format!("inert delta for {VERSION}").into_bytes();
    let setup = if signed {
        b"inert signed setup executable".as_slice()
    } else {
        b"inert unsigned setup executable".as_slice()
    };
    let mut assets = assets_bytes(
        &full_name,
        has_delta,
        &delta_name,
        default_setup_name,
        &portable_name,
    );
    if matches!(
        mutation,
        RunnerMutation::AssetsMissingInstaller
            | RunnerMutation::AssetsMalformed
            | RunnerMutation::AssetsDuplicateInstaller
            | RunnerMutation::AssetsChangedInstaller
            | RunnerMutation::DeltaAssetsMissing
    ) {
        let mut records: Vec<Value> = serde_json::from_slice(&assets)
            .map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
        match mutation {
            RunnerMutation::AssetsMalformed => {
                assets = b"{".to_vec();
            }
            RunnerMutation::AssetsMissingInstaller => {
                records.retain(|record| record["Type"] != "Installer");
            }
            RunnerMutation::AssetsDuplicateInstaller => records.push(json!({
                "RelativeFileName": default_setup_name, "Type": "Installer"
            })),
            RunnerMutation::AssetsChangedInstaller => {
                if let Some(record) = records
                    .iter_mut()
                    .find(|record| record["Type"] == "Installer")
                {
                    record["RelativeFileName"] = Value::String(setup_exe_name(VERSION));
                }
            }
            RunnerMutation::DeltaAssetsMissing => {
                records.retain(|record| record["Type"] != "Delta");
            }
            _ => {}
        }
        if mutation != RunnerMutation::AssetsMalformed {
            assets = serde_json::to_vec(&records)
                .map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
        }
    }
    let feed_has_delta = has_delta && mutation != RunnerMutation::DeltaFeedMissing;
    let feed = feed_bytes(
        &full_name,
        &full,
        feed_has_delta,
        &delta_name,
        &delta,
        &historical,
        output,
    )?;
    let releases = if mutation == RunnerMutation::ReleasesFullMissing {
        releases_bytes(
            "Solstone-0.2.9-full.nupkg",
            b"missing current",
            &historical,
            output,
        )?
    } else {
        releases_bytes(&full_name, &full, &historical, output)?
    };
    let mut files: Vec<(String, &[u8])> = vec![
        (assets_name, &assets),
        (releases_name, &releases),
        (feed_name, &feed),
        (full_name.clone(), &full),
        (portable_name.clone(), &portable),
        (default_setup_name.to_owned(), setup),
    ];
    if has_delta && mutation != RunnerMutation::DeltaPackageMissing {
        files.push((delta_name.clone(), &delta));
    }
    if reverse_order {
        files.reverse();
    }
    for (name, bytes) in files {
        if mutation == RunnerMutation::VpkMissingOutput && name == portable_name {
            continue;
        }
        fs::write(output.join(&name), bytes)
            .map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
    }
    if mutation == RunnerMutation::VpkExtraOutput {
        fs::write(output.join("unexpected-vpk-output.bin"), b"unexpected")
            .map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
    }
    if mutation == RunnerMutation::VpkDefaultSetupConflict {
        fs::write(
            output.join(setup_exe_name(VERSION)),
            b"conflicting versioned setup",
        )
        .map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
    }
    Ok(())
}

fn build_zip(member: &str, bytes: &[u8]) -> Vec<u8> {
    build_zip_members(&[(member, bytes)])
}

fn build_zip_members(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o644);
    for (member, bytes) in members {
        writer
            .start_file(*member, options)
            .expect("start inert member");
        writer.write_all(bytes).expect("write inert member");
    }
    writer.finish().expect("finish inert archive").into_inner()
}

fn assets_bytes(full: &str, has_delta: bool, delta: &str, setup: &str, portable: &str) -> Vec<u8> {
    let mut records = vec![
        json!({"RelativeFileName": full, "Type": "Full"}),
        json!({"RelativeFileName": setup, "Type": "Installer"}),
        json!({"RelativeFileName": portable, "Type": "Portable"}),
    ];
    if has_delta {
        records.insert(1, json!({"RelativeFileName": delta, "Type": "Delta"}));
    }
    serde_json::to_vec(&records).expect("render assets")
}

fn feed_bytes(
    full_name: &str,
    full: &[u8],
    has_delta: bool,
    delta_name: &str,
    delta: &[u8],
    historical: &[String],
    output: &Path,
) -> Result<Vec<u8>, CommandRunnerError> {
    let mut assets = Vec::new();
    for name in historical {
        let bytes =
            fs::read(output.join(name)).map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
        let version = name
            .strip_prefix("Solstone-")
            .and_then(|value| value.strip_suffix("-full.nupkg"))
            .ok_or(CommandRunnerError::UnexpectedInvocation)?;
        assets.push(feed_record(version, "Full", name, &bytes));
    }
    assets.push(feed_record(VERSION, "Full", full_name, full));
    if has_delta {
        assets.push(feed_record(VERSION, "Delta", delta_name, delta));
    }
    serde_json::to_vec(&json!({"Assets": assets}))
        .map_err(|_| CommandRunnerError::UnexpectedInvocation)
}

fn feed_record(version: &str, kind: &str, name: &str, bytes: &[u8]) -> Value {
    json!({
        "PackageId": "Solstone", "Version": version, "Type": kind, "FileName": name,
        "SHA1": hex_sha1(bytes), "SHA256": hex_sha256(bytes), "Size": bytes.len(),
        "NotesMarkdown": "", "NotesHTML": ""
    })
}

fn releases_bytes(
    full_name: &str,
    full: &[u8],
    historical: &[String],
    output: &Path,
) -> Result<Vec<u8>, CommandRunnerError> {
    let mut rows = Vec::new();
    for name in historical {
        let bytes =
            fs::read(output.join(name)).map_err(|_| CommandRunnerError::UnexpectedInvocation)?;
        rows.push(format!("{} {} {}", hex_sha1(&bytes), name, bytes.len()));
    }
    rows.push(format!("{} {} {}", hex_sha1(full), full_name, full.len()));
    rows.sort();
    let mut bytes = vec![0xef, 0xbb, 0xbf];
    bytes.extend_from_slice(rows.join("\n").as_bytes());
    bytes.push(b'\n');
    Ok(bytes)
}

fn accepted_signtool_grammar() -> String {
    format!(
        concat!(
            "Verifying: solstone-setup-0.2.11.exe\n",
            "Signature Index: 0 (Primary Signature)\n",
            "Hash of file (sha256): AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n",
            "Signing Certificate Chain:\n",
            "Issued to: Public Root CA\nIssued by: Public Root CA\nExpires: 2035\nSHA1 hash: 1111111111111111111111111111111111111111\n",
            "Issued to: sol pbc\nIssued by: Public Code Signing CA\nExpires: 2027\nSHA1 hash: {}\n",
            "The signature is timestamped: Tue Jul 21 12:00:00 2026\n",
            "Timestamp protocol: RFC3161\nTimestamp Verified by:\n",
            "Issued to: Public Timestamp Root\nIssued by: Public Timestamp Root\nExpires: 2030\nSHA1 hash: 2222222222222222222222222222222222222222\n",
            "Successfully verified: solstone-setup-0.2.11.exe\n",
            "Number of signatures successfully Verified: 1\nNumber of warnings: 0\nNumber of errors: 0\n"
        ),
        PUBLIC_LEAF_UPPER
    )
}

fn hex_sha1(bytes: &[u8]) -> String {
    Sha1::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
