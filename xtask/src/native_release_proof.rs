// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Strict native install and smoke proof for one finalized Windows candidate.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use semver::Version;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::artifact_fs::{ContainedRoot, UnixModePolicy};
use crate::release_clock::Clock;
use crate::release_container::ExecutableContainerReader;
use crate::release_exec::CommandRunner;
use crate::release_finalizer_fs::create_contained_directory;
use crate::release_receipt::{
    candidate_relative_path, finalization_receipt_relative_path, render_finalization_receipt,
    stage_windows_native_proof_receipt, CompanionManifestReceipt, FinalizationReceipt,
    WindowsNativeProofReceipt, WINDOWS_NATIVE_PROOF_SCHEMA,
};
use crate::release_selection::{ReleaseToolSelection, SelectedAction, SelectionMode};
use crate::rust_release_manifest::{
    self, companion_basename, BundleNames, CheckoutFacts, Manifest, PackagedExecutableEvidence,
    PRODUCT, TARGET_TRIPLE,
};

const INSTALLED_EXECUTABLE: &str = "current/solstone-windows-app.exe";
const PROOF_ROOT: &str = "target/release-native-proof";
const PROOF_TEMP_ATTEMPTS: usize = 16;

pub const STEP_1_CLASSIFY: &str = "native-proof.step-1.classify";
pub const STEP_2_IDENTITY: &str = "native-proof.step-2.identity";
pub const STEP_3_TOOLS: &str = "native-proof.step-3.tools";
pub const STEP_4_CONTAINERS: &str = "native-proof.step-4.containers";
pub const STEP_5_INSTALL_ROOT: &str = "native-proof.step-5.install-root";
pub const STEP_5_ROOT_READY: &str = "native-proof.step-5.install-root-ready";
pub const STEP_6_INSTALL: &str = "native-proof.step-6.install";
pub const STEP_7_INSTALLED_IDENTITY: &str = "native-proof.step-7.installed-identity";
pub const STEP_8_DUMP_STATE: &str = "native-proof.step-8.dump-state";
pub const STEP_9_SMOKE: &str = "native-proof.step-9.smoke";
pub const STEP_10_REVALIDATE: &str = "native-proof.step-10.revalidate";
pub const STEP_11_RECEIPT: &str = "native-proof.step-11.receipt";
pub const STEP_11_RECEIPT_STAGED: &str = "native-proof.step-11.receipt-staged";

static NEXT_PROOF_NONCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug)]
pub struct NativeProofRuntime<'a> {
    pub checkout_root: &'a Path,
    pub facts: &'a CheckoutFacts,
    pub powershell_bootstrap: &'a OsStr,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeProofResult {
    pub version: String,
    pub manifest_sha256: String,
    pub receipt_relative_path: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeProofError {
    PhaseTransition,
    InitialClassification,
    CheckoutContainment,
    CandidateIdentity,
    ManifestIdentity,
    FinalizationReceipt,
    FinalizationReceiptMismatch,
    UnsignedCandidate,
    ToolResolver,
    ToolSelection,
    ToolProjectionMismatch,
    ContainerBaseline,
    ProofRoot,
    PreexistingInstalledApp,
    SetupInvocation,
    SetupFailed,
    InstalledAppMissing,
    InstalledAppInvalid,
    InstalledBaselineMismatch,
    DumpStateInvocation,
    DumpStateFailed,
    DumpStateMalformed,
    DumpStateVersionMismatch,
    SmokeInvocation,
    SmokeFailed,
    SmokeEvidenceMissing,
    PostSmokeClassification,
    CandidateMutated,
    Clock,
    Receipt,
}

impl fmt::Display for NativeProofError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PhaseTransition => write!(
                formatter,
                "native proof step ordering could not be witnessed; recreate the command runner and retry the proof"
            ),
            Self::InitialClassification => write!(
                formatter,
                "native proof candidate failed strict whole-directory classification; restore the exact finalized 7/8-file candidate and retry"
            ),
            Self::CheckoutContainment => write!(
                formatter,
                "native proof checkout containment failed; use one real checkout without links or reparse points"
            ),
            Self::CandidateIdentity => write!(
                formatter,
                "native proof release directory is not the finalized versioned candidate path; pass target/release-candidate/<VERSION> and retry"
            ),
            Self::ManifestIdentity => write!(
                formatter,
                "native proof companion manifest could not be stable-read and identified; restore immutable candidate bytes and retry"
            ),
            Self::FinalizationReceipt => write!(
                formatter,
                "native proof finalization receipt is missing, malformed, or non-canonical; restore target/release-evidence/<VERSION>/rust-release-finalization.json"
            ),
            Self::FinalizationReceiptMismatch => write!(
                formatter,
                "native proof finalization receipt does not identify this candidate; restore the receipt emitted with these exact candidate bytes"
            ),
            Self::UnsignedCandidate => write!(
                formatter,
                "native proof refuses an unsigned candidate; finalize with approved signing and retry the signed candidate"
            ),
            Self::ToolResolver => write!(
                formatter,
                "native proof signed tool preflight could not produce one selection record; restore the configured PowerShell bootstrap and rerun preflight"
            ),
            Self::ToolSelection => write!(
                formatter,
                "native proof signed tool selection is invalid; rerun the current signed release-tool preflight and use its record unchanged"
            ),
            Self::ToolProjectionMismatch => write!(
                formatter,
                "native proof selected tools do not match the candidate's signed tool map; restore the pinned build-box tools used to finalize the candidate"
            ),
            Self::ContainerBaseline => write!(
                formatter,
                "native proof nupkg or portable executable disagrees with the manifest baseline; restore both finalized containers and retry"
            ),
            Self::ProofRoot => write!(
                formatter,
                "native proof could not create one newly empty isolated install root; remove unsafe links or stale permissions beneath target and retry"
            ),
            Self::PreexistingInstalledApp => write!(
                formatter,
                "native proof isolated root already contains the canonical app; use a newly empty proof root and retry"
            ),
            Self::SetupInvocation => write!(
                formatter,
                "native proof could not invoke the candidate's canonical setup executable; restore that exact setup and retry"
            ),
            Self::SetupFailed => write!(
                formatter,
                "native proof setup exited nonzero; repair the signed installer and retry in a new isolated root"
            ),
            Self::InstalledAppMissing => write!(
                formatter,
                "native proof setup reported success without creating the canonical app; rebuild the installer and retry"
            ),
            Self::InstalledAppInvalid => write!(
                formatter,
                "native proof installed app is not one stable regular file; remove links or reparse points and retry a clean install"
            ),
            Self::InstalledBaselineMismatch => write!(
                formatter,
                "native proof installed app disagrees with the manifest and container baseline; rebuild and re-finalize both containers"
            ),
            Self::DumpStateInvocation => write!(
                formatter,
                "native proof could not invoke the explicitly installed app with --dump-state; repair that installed binary and retry"
            ),
            Self::DumpStateFailed => write!(
                formatter,
                "native proof installed app --dump-state exited nonzero; repair the candidate app and retry"
            ),
            Self::DumpStateMalformed => write!(
                formatter,
                "native proof installed app returned malformed --dump-state JSON; restore the canonical health output and retry"
            ),
            Self::DumpStateVersionMismatch => write!(
                formatter,
                "native proof installed app version differs from the candidate version; rebuild the source-bound candidate and retry"
            ),
            Self::SmokeInvocation => write!(
                formatter,
                "native proof could not invoke the selected explicit-binary smoke action; restore the selected PowerShell and smoke script"
            ),
            Self::SmokeFailed => write!(
                formatter,
                "native proof explicit-binary health/render smoke failed; inspect the isolated installed app and rerun after repair"
            ),
            Self::SmokeEvidenceMissing => write!(
                formatter,
                "native proof smoke did not emit literal SMOKE_OK; restore the load-bearing health/render gate and retry"
            ),
            Self::PostSmokeClassification => write!(
                formatter,
                "native proof candidate failed strict validation after smoke; restore immutable finalized candidate bytes and retry"
            ),
            Self::CandidateMutated => write!(
                formatter,
                "native proof companion manifest changed during install or smoke; discard the mutated candidate and re-finalize"
            ),
            Self::Clock => write!(
                formatter,
                "native proof UTC proof time could not be obtained; restore the system clock and retry"
            ),
            Self::Receipt => write!(
                formatter,
                "native proof receipt could not be atomically written; restore the contained evidence directory and retry"
            ),
        }
    }
}

impl std::error::Error for NativeProofError {}

pub fn prove_native<R: CommandRunner + ?Sized, C: Clock + ?Sized>(
    runtime: NativeProofRuntime<'_>,
    release_dir: &Path,
    runner: &R,
    clock: &C,
) -> Result<NativeProofResult, NativeProofError> {
    record_step(runner, STEP_1_CLASSIFY)?;
    let report = rust_release_manifest::validate_release_dir_with_facts(release_dir, runtime.facts)
        .map_err(|_| NativeProofError::InitialClassification)?;

    record_step(runner, STEP_2_IDENTITY)?;
    let checkout = ContainedRoot::new(
        runtime.checkout_root,
        "native proof checkout",
        UnixModePolicy::AllowExecute,
    )
    .map_err(|_| NativeProofError::CheckoutContainment)?;
    let candidate = ContainedRoot::new(
        release_dir,
        "native proof candidate",
        UnixModePolicy::AllowExecute,
    )
    .map_err(|_| NativeProofError::CandidateIdentity)?;
    let expected_candidate_relative = candidate_relative_path(&runtime.facts.version)
        .map_err(|_| NativeProofError::CandidateIdentity)?;
    let expected_candidate = checkout.canonical_path().join(&expected_candidate_relative);
    if candidate.canonical_path() != expected_candidate {
        return Err(NativeProofError::CandidateIdentity);
    }
    let manifest_filename = companion_basename();
    let manifest_bytes = candidate
        .read(&manifest_filename, "native proof companion manifest")
        .map_err(|_| NativeProofError::ManifestIdentity)?;
    let manifest_sha256 = sha256_hex(&manifest_bytes);
    let manifest = rust_release_manifest::validate_manifest_bytes(&manifest_bytes)
        .map_err(|_| NativeProofError::ManifestIdentity)?;
    let finalization_receipt = read_matching_finalization_receipt(
        &checkout,
        &candidate,
        &manifest,
        &manifest_filename,
        &manifest_sha256,
        u64::try_from(report.artifact_count + 1)
            .map_err(|_| NativeProofError::FinalizationReceiptMismatch)?,
        &expected_candidate_relative,
    )?;

    record_step(runner, STEP_3_TOOLS)?;
    if manifest
        .native_tools
        .get("signing_mode")
        .map(String::as_str)
        != Some("signed-verified")
        || manifest.native_tools != runtime.facts.signed_native_tools
        || finalization_receipt.signing_mode != "signed-verified"
    {
        return Err(NativeProofError::UnsignedCandidate);
    }
    let selection_bytes = run_signed_tool_resolver(
        checkout.canonical_path(),
        runtime.powershell_bootstrap,
        runner,
    )?;
    let selection = ReleaseToolSelection::parse(&selection_bytes)
        .map_err(|_| NativeProofError::ToolSelection)?;
    if selection.mode != SelectionMode::Signed {
        return Err(NativeProofError::ToolSelection);
    }
    let projection = selection
        .sanitized_projection(checkout.canonical_path())
        .map_err(|_| NativeProofError::ToolSelection)?;
    if projection.native_tools != manifest.native_tools {
        return Err(NativeProofError::ToolProjectionMismatch);
    }

    record_step(runner, STEP_4_CONTAINERS)?;
    let names = BundleNames::for_version(&manifest.version);
    let nupkg = ExecutableContainerReader::read_nupkg(
        &candidate
            .read(names.full_package(), "native proof full nupkg")
            .map_err(|_| NativeProofError::ContainerBaseline)?,
    )
    .map_err(|_| NativeProofError::ContainerBaseline)?;
    let portable = ExecutableContainerReader::read_portable(
        &candidate
            .read(names.portable(), "native proof portable ZIP")
            .map_err(|_| NativeProofError::ContainerBaseline)?,
    )
    .map_err(|_| NativeProofError::ContainerBaseline)?;
    if nupkg != manifest.packaged_executable || portable != manifest.packaged_executable {
        return Err(NativeProofError::ContainerBaseline);
    }

    record_step(runner, STEP_5_INSTALL_ROOT)?;
    let local_app_data = create_proof_root(&checkout, &manifest.version)?;
    let install_root = local_app_data.join("Solstone");
    let installed_app = install_root.join(INSTALLED_EXECUTABLE);
    record_step(runner, STEP_5_ROOT_READY)?;
    require_absent(&installed_app)?;
    require_empty_proof_root(&local_app_data)?;

    record_step(runner, STEP_6_INSTALL)?;
    let setup_relative = names.setup();
    let setup_bytes = candidate
        .read(setup_relative, "native proof setup executable")
        .map_err(|_| NativeProofError::SetupInvocation)?;
    let setup_sha256 = sha256_hex(&setup_bytes);
    let setup_program = candidate.canonical_path().join(setup_relative);
    let install_root_text = path_text(&install_root, NativeProofError::SetupInvocation)?;
    let local_app_data_text = path_text(&local_app_data, NativeProofError::SetupInvocation)?;
    let isolated_env = BTreeMap::from([("LOCALAPPDATA".to_owned(), local_app_data_text)]);
    let install_output = runner
        .run(
            &setup_program,
            &[
                "--silent".to_owned(),
                "--installto".to_owned(),
                install_root_text,
            ],
            None,
            Some(&isolated_env),
        )
        .map_err(|_| NativeProofError::SetupInvocation)?;
    if install_output.status != 0 {
        return Err(NativeProofError::SetupFailed);
    }
    match fs::symlink_metadata(&installed_app) {
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Err(NativeProofError::InstalledAppMissing)
        }
        Err(_) => return Err(NativeProofError::InstalledAppInvalid),
    }

    record_step(runner, STEP_7_INSTALLED_IDENTITY)?;
    let installed_root = ContainedRoot::new(
        &install_root,
        "native proof install root",
        UnixModePolicy::AllowExecute,
    )
    .map_err(|_| NativeProofError::InstalledAppInvalid)?;
    let installed_bytes = installed_root
        .read(INSTALLED_EXECUTABLE, "native proof installed executable")
        .map_err(|_| NativeProofError::InstalledAppInvalid)?;
    let installed_evidence = executable_evidence(&installed_bytes)?;
    if installed_evidence != manifest.packaged_executable
        || installed_evidence != nupkg
        || installed_evidence != portable
    {
        return Err(NativeProofError::InstalledBaselineMismatch);
    }

    record_step(runner, STEP_8_DUMP_STATE)?;
    let dump = runner
        .run(
            &installed_app,
            &["--dump-state".to_owned()],
            None,
            Some(&isolated_env),
        )
        .map_err(|_| NativeProofError::DumpStateInvocation)?;
    if dump.status != 0 {
        return Err(NativeProofError::DumpStateFailed);
    }
    validate_dump_state_version(&dump.stdout, &manifest.version)?;

    record_step(runner, STEP_9_SMOKE)?;
    let smoke_args = substitute_action(
        &selection.actions.native_smoke,
        &BTreeMap::from([
            (
                "{installed_exe}",
                path_text(&installed_app, NativeProofError::SmokeInvocation)?,
            ),
            ("{expected_version}", manifest.version.clone()),
            (
                "{expected_sha256}",
                manifest.packaged_executable.sha256.clone(),
            ),
            (
                "{dotnet_path}",
                path_text(
                    &selection.tools.dotnet.path,
                    NativeProofError::SmokeInvocation,
                )?,
            ),
        ]),
    )?;
    let smoke = runner
        .run(
            &selection.actions.native_smoke.program,
            &smoke_args,
            None,
            Some(&isolated_env),
        )
        .map_err(|_| NativeProofError::SmokeInvocation)?;
    if smoke.status != 0 {
        return Err(NativeProofError::SmokeFailed);
    }
    if !has_literal_line(&smoke.stdout, "SMOKE_OK") {
        return Err(NativeProofError::SmokeEvidenceMissing);
    }

    record_step(runner, STEP_10_REVALIDATE)?;
    rust_release_manifest::validate_release_dir_with_facts(release_dir, runtime.facts)
        .map_err(|_| NativeProofError::PostSmokeClassification)?;
    let final_manifest_bytes = candidate
        .read(&manifest_filename, "native proof companion manifest")
        .map_err(|_| NativeProofError::CandidateMutated)?;
    if final_manifest_bytes != manifest_bytes
        || sha256_hex(&final_manifest_bytes) != manifest_sha256
    {
        return Err(NativeProofError::CandidateMutated);
    }

    record_step(runner, STEP_11_RECEIPT)?;
    let proved_at = clock
        .now()
        .map_err(|_| NativeProofError::Clock)?
        .as_str()
        .to_owned();
    let receipt = WindowsNativeProofReceipt {
        schema: WINDOWS_NATIVE_PROOF_SCHEMA.to_owned(),
        product: PRODUCT.to_owned(),
        version: manifest.version.clone(),
        target: TARGET_TRIPLE.to_owned(),
        source_commit: manifest.source_commit.clone(),
        companion_manifest: CompanionManifestReceipt {
            filename: manifest_filename,
            sha256: manifest_sha256.clone(),
        },
        setup_sha256,
        packaged_executable_sha256: manifest.packaged_executable.sha256.clone(),
        installed_executable_sha256: installed_evidence.sha256,
        install_mode: "isolated-clean".to_owned(),
        installer_success: true,
        smoke_success: true,
        proved_at,
    };
    let staged = stage_windows_native_proof_receipt(checkout.canonical_path(), &receipt)
        .map_err(|_| NativeProofError::Receipt)?;
    record_step(runner, STEP_11_RECEIPT_STAGED)?;
    let receipt_relative_path = staged.promote().map_err(|_| NativeProofError::Receipt)?;
    Ok(NativeProofResult {
        version: manifest.version,
        manifest_sha256,
        receipt_relative_path,
    })
}

fn read_matching_finalization_receipt(
    checkout: &ContainedRoot,
    candidate: &ContainedRoot,
    manifest: &Manifest,
    manifest_filename: &str,
    manifest_sha256: &str,
    candidate_file_count: u64,
    candidate_relative: &str,
) -> Result<FinalizationReceipt, NativeProofError> {
    let relative = finalization_receipt_relative_path(&manifest.version)
        .map_err(|_| NativeProofError::FinalizationReceipt)?;
    let bytes = checkout
        .read(&relative, "native proof finalization receipt")
        .map_err(|_| NativeProofError::FinalizationReceipt)?;
    let receipt: FinalizationReceipt =
        serde_json::from_slice(&bytes).map_err(|_| NativeProofError::FinalizationReceipt)?;
    let canonical =
        render_finalization_receipt(&receipt).map_err(|_| NativeProofError::FinalizationReceipt)?;
    if canonical != bytes {
        return Err(NativeProofError::FinalizationReceipt);
    }
    let ui_lock = checkout
        .read("ui/package-lock.json", "native proof UI lock")
        .map_err(|_| NativeProofError::FinalizationReceiptMismatch)?;
    if receipt.product != manifest.product
        || receipt.version != manifest.version
        || receipt.target != TARGET_TRIPLE
        || receipt.source_commit != manifest.source_commit
        || receipt.cargo_lock_sha256 != manifest.cargo_lock_sha256
        || receipt.ui_package_lock_sha256 != sha256_hex(&ui_lock)
        || receipt.companion_manifest.filename != manifest_filename
        || receipt.companion_manifest.sha256 != manifest_sha256
        || receipt.candidate.relative_path != candidate_relative
        || receipt.candidate.file_count != candidate_file_count
        || receipt.advisory_checked_at != manifest.dependency_policy.advisory_checked_at
        || candidate.canonical_path() != checkout.canonical_path().join(candidate_relative)
    {
        return Err(NativeProofError::FinalizationReceiptMismatch);
    }
    Ok(receipt)
}

fn run_signed_tool_resolver<R: CommandRunner + ?Sized>(
    checkout_root: &Path,
    bootstrap: &OsStr,
    runner: &R,
) -> Result<Vec<u8>, NativeProofError> {
    let program = resolve_bootstrap(bootstrap)?;
    let script = checkout_root.join("packaging/preflight-release-tools.ps1");
    let args = vec![
        "-NoProfile".to_owned(),
        "-ExecutionPolicy".to_owned(),
        "Bypass".to_owned(),
        "-File".to_owned(),
        path_text(&script, NativeProofError::ToolResolver)?,
        "-Sign".to_owned(),
    ];
    let output = runner
        .run(&program, &args, None, None)
        .map_err(|_| NativeProofError::ToolResolver)?;
    if output.status != 0 || !output.stderr.is_empty() {
        return Err(NativeProofError::ToolResolver);
    }
    let text = std::str::from_utf8(&output.stdout).map_err(|_| NativeProofError::ToolResolver)?;
    let record = text.trim_end_matches(['\r', '\n']);
    if record.is_empty() || record.contains(['\r', '\n']) {
        return Err(NativeProofError::ToolResolver);
    }
    Ok(record.as_bytes().to_vec())
}

fn resolve_bootstrap(value: &OsStr) -> Result<PathBuf, NativeProofError> {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        return Ok(path);
    }
    if path.components().count() != 1 {
        return Err(NativeProofError::ToolResolver);
    }
    let search = std::env::var_os("PATH").ok_or(NativeProofError::ToolResolver)?;
    for directory in std::env::split_paths(&search) {
        let candidate = directory.join(&path);
        if candidate.is_file() {
            return Ok(candidate);
        }
        #[cfg(windows)]
        if path.extension().is_none() {
            let executable = directory.join(format!("{}.exe", path.to_string_lossy()));
            if executable.is_file() {
                return Ok(executable);
            }
        }
    }
    Err(NativeProofError::ToolResolver)
}

fn create_proof_root(checkout: &ContainedRoot, version: &str) -> Result<PathBuf, NativeProofError> {
    Version::parse(version).map_err(|_| NativeProofError::ProofRoot)?;
    let parent = format!("{PROOF_ROOT}/{version}");
    create_contained_directory(checkout.path(), checkout.canonical_path(), &parent)
        .map_err(|_| NativeProofError::ProofRoot)?;
    for _ in 0..PROOF_TEMP_ATTEMPTS {
        let relative = format!("{parent}/.native-proof-{}.tmp", proof_nonce());
        let path = checkout.path().join(&relative);
        match fs::create_dir(&path) {
            Ok(()) => {
                let root = ContainedRoot::new(
                    &path,
                    "native proof local app data",
                    UnixModePolicy::AllowExecute,
                )
                .map_err(|_| NativeProofError::ProofRoot)?;
                if root.canonical_path().starts_with(checkout.canonical_path())
                    && fs::read_dir(root.canonical_path())
                        .map_err(|_| NativeProofError::ProofRoot)?
                        .next()
                        .is_none()
                {
                    return Ok(root.canonical_path().to_path_buf());
                }
                return Err(NativeProofError::ProofRoot);
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(NativeProofError::ProofRoot),
        }
    }
    Err(NativeProofError::ProofRoot)
}

fn proof_nonce() -> String {
    let sequence = NEXT_PROOF_NONCE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let mut hasher = Sha256::new();
    hasher.update(std::process::id().to_le_bytes());
    hasher.update(sequence.to_le_bytes());
    hasher.update(nanos.to_le_bytes());
    format!("{:x}", hasher.finalize())[..32].to_owned()
}

fn require_absent(path: &Path) -> Result<(), NativeProofError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(NativeProofError::PreexistingInstalledApp),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(_) => Err(NativeProofError::ProofRoot),
    }
}

fn require_empty_proof_root(path: &Path) -> Result<(), NativeProofError> {
    let root = ContainedRoot::new(
        path,
        "native proof local app data",
        UnixModePolicy::AllowExecute,
    )
    .map_err(|_| NativeProofError::ProofRoot)?;
    if fs::read_dir(root.canonical_path())
        .map_err(|_| NativeProofError::ProofRoot)?
        .next()
        .is_some()
    {
        return Err(NativeProofError::ProofRoot);
    }
    Ok(())
}

fn executable_evidence(bytes: &[u8]) -> Result<PackagedExecutableEvidence, NativeProofError> {
    let bytes_count =
        u64::try_from(bytes.len()).map_err(|_| NativeProofError::InstalledAppInvalid)?;
    if bytes_count == 0 {
        return Err(NativeProofError::InstalledAppInvalid);
    }
    Ok(PackagedExecutableEvidence {
        sha256: sha256_hex(bytes),
        bytes: bytes_count,
    })
}

fn validate_dump_state_version(bytes: &[u8], version: &str) -> Result<(), NativeProofError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| NativeProofError::DumpStateMalformed)?;
    let observed = value
        .as_object()
        .and_then(|object| object.get("version"))
        .and_then(Value::as_str)
        .ok_or(NativeProofError::DumpStateMalformed)?;
    let parsed = Version::parse(observed).map_err(|_| NativeProofError::DumpStateMalformed)?;
    if parsed.to_string() != observed || observed != version {
        return Err(NativeProofError::DumpStateVersionMismatch);
    }
    Ok(())
}

fn substitute_action(
    action: &SelectedAction,
    replacements: &BTreeMap<&str, String>,
) -> Result<Vec<String>, NativeProofError> {
    action
        .argv
        .iter()
        .map(|argument| {
            if let Some(value) = replacements.get(argument.as_str()) {
                Ok(value.clone())
            } else if argument.contains('{') || argument.contains('}') {
                Err(NativeProofError::ToolSelection)
            } else {
                Ok(argument.clone())
            }
        })
        .collect()
}

fn has_literal_line(bytes: &[u8], expected: &str) -> bool {
    std::str::from_utf8(bytes).is_ok_and(|text| {
        text.lines()
            .any(|line| line.trim_end_matches('\r') == expected)
    })
}

fn record_step<R: CommandRunner + ?Sized>(
    runner: &R,
    step: &'static str,
) -> Result<(), NativeProofError> {
    runner
        .record_phase(step)
        .map_err(|_| NativeProofError::PhaseTransition)
}

fn path_text(path: &Path, error: NativeProofError) -> Result<String, NativeProofError> {
    path.to_str().map(str::to_owned).ok_or(error)
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
