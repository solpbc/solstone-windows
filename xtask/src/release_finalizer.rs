// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Source-bound build-to-finalize release transaction.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::artifact_fs::{self, ContainedRoot, UnixModePolicy};
use crate::release_advisory::{run_advisory_check, AdvisoryProvenance};
use crate::release_clock::Clock;
use crate::release_container::{compare_executable_baseline, ExecutableContainerReader};
use crate::release_exec::CommandRunner;
use crate::release_finalizer_fs::{
    create_candidate_temp, create_contained_directory, DeletionPlan, ReleaseCleanupCatalog,
};
use crate::release_receipt::{
    candidate_relative_path, stage_finalization_receipt, AdvisoryDatabaseReceipt, CandidateReceipt,
    CompanionManifestReceipt, FinalizationReceipt, FINALIZATION_RECEIPT_SCHEMA,
};
use crate::release_selection::{
    ManifestSafeToolProjection, ReleaseToolSelection, SelectedAction, SelectionMode,
};
use crate::release_signing::{verify_release_signing, SigningPolicy, SigningVerificationRequest};
use crate::release_source_binding::{SourceBinding, SourceBindingVerifier};
use crate::rust_release_manifest::{
    self, companion_basename, ArtifactEvidence, BundleNames, DependencyPolicy, ReleaseEvidence,
    RustEvidence, TargetEvidence, PRODUCT, TARGET_FEATURES, TARGET_PROFILE, TARGET_TRIPLE,
};
use crate::version_gate;

const STAGED_EXECUTABLE: &str = "solstone-windows-app.exe";
const SIGNING_POLICY: &str = "packaging/signing-policy.json";

pub const PHASE_1_REQUEST_SOURCE: &str = "finalize.phase-1.request-source";
pub const PHASE_2_CLEANUP: &str = "finalize.phase-2.cleanup";
pub const PHASE_3_ADVISORY_PREFLIGHT: &str = "finalize.phase-3.advisory-preflight";
pub const PHASE_4_BUILD: &str = "finalize.phase-4.build";
pub const PHASE_5_VELOPACK: &str = "finalize.phase-5.velopack";
pub const PHASE_6_BASELINE_CANDIDATE: &str = "finalize.phase-6.baseline-candidate";
pub const PHASE_7_EVIDENCE: &str = "finalize.phase-7.evidence";
pub const PHASE_8_PROMOTION: &str = "finalize.phase-8.promotion";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalizeRequest {
    pub expected_release_commit: String,
    pub sign_mode: SelectionMode,
    pub selection_record: Vec<u8>,
    pub delta_base_fulls: Vec<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct FinalizeRuntime<'a> {
    pub checkout_root: &'a Path,
    pub git_program: &'a Path,
    pub advisory_tree_sha256: &'a str,
    pub signing_keypair_alias: Option<&'a str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalizeResult {
    pub version: String,
    pub candidate_relative_path: String,
    pub receipt_relative_path: String,
    pub manifest_sha256: String,
    pub signing_mode: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FinalizeError {
    PhaseTransition,
    RequestInvalid,
    SelectionInvalid,
    SelectionModeMismatch,
    VersionAuthority,
    SourceBinding,
    Cleanup,
    TransactionDirectory,
    Advisory,
    ReleaseNotes,
    SigningRuntime,
    ActionFailed { action: &'static str },
    BuildArtifact,
    DeltaSeed,
    VelopackInventory,
    DeltaSeedChanged,
    AssetsReconciliation,
    LedgerReconciliation,
    SigningVerification,
    ExecutableBaseline,
    CandidateAssembly,
    EvidenceConstruction,
    ManifestValidation,
    ReceiptStaging,
    SourceReverification,
    CandidatePromotion,
    ReceiptPromotion,
    FailureCleanup,
}

impl fmt::Display for FinalizeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PhaseTransition => write!(
                formatter,
                "release finalizer phase transition could not be witnessed; recreate the command runner and restart the transaction"
            ),
            Self::RequestInvalid => write!(
                formatter,
                "release finalizer request is incomplete or non-canonical; pass the full expected commit and reviewed runtime evidence"
            ),
            Self::SelectionInvalid => write!(
                formatter,
                "release-tool selection record is invalid; rerun preflight and pass its exact JSON bytes on stdin"
            ),
            Self::SelectionModeMismatch => write!(
                formatter,
                "requested signing mode disagrees with the selection record; rerun preflight in the requested mode"
            ),
            Self::VersionAuthority => write!(
                formatter,
                "selected Cargo could not establish the metadata version authority; restore the locked checkout and selected Cargo"
            ),
            Self::SourceBinding => write!(
                formatter,
                "release source binding failed; restore the exact clean expected commit, allowed branch, and both tracked locks"
            ),
            Self::Cleanup => write!(
                formatter,
                "release cleanup confinement or execution failed; remediate the reported catalog path and restart before building"
            ),
            Self::TransactionDirectory => write!(
                formatter,
                "release transaction directories are not newly empty and contained; run confined cleanup and restart"
            ),
            Self::Advisory => write!(
                formatter,
                "release advisory gate failed; refresh the isolated reviewed snapshot or remediate cargo-deny and restart"
            ),
            Self::ReleaseNotes => write!(
                formatter,
                "CHANGELOG lacks one nonempty section for the cargo-metadata release version; cut the reviewed release notes and restart"
            ),
            Self::SigningRuntime => write!(
                formatter,
                "signed finalization lacks a safe keypair alias runtime input; provide the build-box SM_KEYPAIR_ALIAS and restart"
            ),
            Self::ActionFailed { action } => write!(
                formatter,
                "selected release action {action} failed; repair that selected tool or its inputs and restart the whole transaction"
            ),
            Self::BuildArtifact => write!(
                formatter,
                "the release build did not produce one stable contained app executable; repair the selected Cargo build and restart"
            ),
            Self::DeltaSeed => write!(
                formatter,
                "an explicit delta-base full package could not be copied exactly; restore the allowlisted historical package and restart"
            ),
            Self::VelopackInventory => write!(
                formatter,
                "Velopack output differs from the explicit seeds plus exact current output set; clear transaction output and restart"
            ),
            Self::DeltaSeedChanged => write!(
                formatter,
                "Velopack changed an allowlisted historical full package; restore the immutable seed and restart"
            ),
            Self::AssetsReconciliation => write!(
                formatter,
                "assets.win.json does not contain exactly one default Installer record to version; rebuild with pinned Velopack"
            ),
            Self::LedgerReconciliation => write!(
                formatter,
                "Velopack ledgers disagree about the current full or delta artifacts; rebuild the complete output in one transaction"
            ),
            Self::SigningVerification => write!(
                formatter,
                "final setup signing verification failed; restore the approved signing identity, trust, and RFC 3161 timestamp"
            ),
            Self::ExecutableBaseline => write!(
                formatter,
                "staged, nupkg, and portable executable bytes disagree; rebuild both containers in this transaction"
            ),
            Self::CandidateAssembly => write!(
                formatter,
                "candidate assembly failed before evidence rendering; discard the temporary candidate and restart"
            ),
            Self::EvidenceConstruction => write!(
                formatter,
                "release evidence could not be constructed canonically; restore verified artifact, tool, and advisory inputs"
            ),
            Self::ManifestValidation => write!(
                formatter,
                "the assembled candidate failed strict release-manifest validation; discard it and rebuild the full bundle"
            ),
            Self::ReceiptStaging => write!(
                formatter,
                "the finalization receipt could not be staged atomically; restore the contained evidence directory and restart"
            ),
            Self::SourceReverification => write!(
                formatter,
                "release source or either lock changed before promotion; restore the initial binding and restart"
            ),
            Self::CandidatePromotion => write!(
                formatter,
                "the complete candidate could not be atomically promoted; clear the confined same-version target and restart"
            ),
            Self::ReceiptPromotion => write!(
                formatter,
                "the candidate promoted but its receipt did not; the candidate was withdrawn, so restore evidence storage and restart"
            ),
            Self::FailureCleanup => write!(
                formatter,
                "release finalization failed and confined temporary cleanup also refused; remediate containment before retrying"
            ),
        }
    }
}

impl std::error::Error for FinalizeError {}

#[derive(Clone, Debug)]
struct TransactionPaths {
    cargo_target_relative: String,
    cargo_target: PathBuf,
    stage: PathBuf,
    output: PathBuf,
    notes: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct AssetsRecord {
    #[serde(rename = "RelativeFileName")]
    relative_file_name: String,
    #[serde(rename = "Type")]
    asset_type: String,
}

pub fn finalize<R: CommandRunner + ?Sized, C: Clock + ?Sized>(
    runtime: FinalizeRuntime<'_>,
    request: &FinalizeRequest,
    runner: &R,
    clock: &C,
) -> Result<FinalizeResult, FinalizeError> {
    record_phase(runner, PHASE_1_REQUEST_SOURCE)?;
    let checkout = ContainedRoot::new(
        runtime.checkout_root,
        "release checkout",
        UnixModePolicy::AllowExecute,
    )
    .map_err(|_| FinalizeError::RequestInvalid)?;
    if request.selection_record.is_empty() || runtime.advisory_tree_sha256.len() != 64 {
        return Err(FinalizeError::RequestInvalid);
    }
    let selection = ReleaseToolSelection::parse(&request.selection_record)
        .map_err(|_| FinalizeError::SelectionInvalid)?;
    if selection.mode != request.sign_mode {
        return Err(FinalizeError::SelectionModeMismatch);
    }
    let safe_tools = selection
        .sanitized_projection(checkout.canonical_path())
        .map_err(|_| FinalizeError::SelectionInvalid)?;
    let version = version_gate::authoritative_version_with_runner(
        checkout.canonical_path(),
        &selection.tools.cargo.path,
        runner,
    )
    .map_err(|_| FinalizeError::VersionAuthority)?;
    let source_verifier =
        SourceBindingVerifier::new(checkout.canonical_path(), runtime.git_program, runner)
            .map_err(|_| FinalizeError::SourceBinding)?;
    let source = source_verifier
        .verify(&request.expected_release_commit)
        .map_err(|_| FinalizeError::SourceBinding)?;

    record_phase(runner, PHASE_2_CLEANUP)?;
    let catalog = ReleaseCleanupCatalog::for_version(checkout.canonical_path(), &version)
        .map_err(|_| FinalizeError::Cleanup)?;
    let plan = DeletionPlan::materialize(&catalog, &request.delta_base_fulls)
        .map_err(|_| FinalizeError::Cleanup)?;
    plan.execute().map_err(|_| FinalizeError::Cleanup)?;

    let transaction_result = run_mutating_transaction(
        &checkout,
        runtime,
        request,
        &selection,
        safe_tools,
        &version,
        &source,
        &source_verifier,
        runner,
        clock,
    );
    if transaction_result.is_err()
        && cleanup_failed_transaction(
            checkout.canonical_path(),
            &version,
            &request.delta_base_fulls,
        )
        .is_err()
    {
        return Err(FinalizeError::FailureCleanup);
    }
    transaction_result
}

#[allow(clippy::too_many_arguments)]
fn run_mutating_transaction<R: CommandRunner + ?Sized, C: Clock + ?Sized>(
    checkout: &ContainedRoot,
    runtime: FinalizeRuntime<'_>,
    request: &FinalizeRequest,
    selection: &ReleaseToolSelection,
    safe_tools: ManifestSafeToolProjection,
    version: &str,
    source: &SourceBinding,
    source_verifier: &SourceBindingVerifier<'_, R>,
    runner: &R,
    clock: &C,
) -> Result<FinalizeResult, FinalizeError> {
    record_phase(runner, PHASE_3_ADVISORY_PREFLIGHT)?;
    let paths = create_transaction_paths(checkout, version)?;
    let advisory = run_advisory_check(
        checkout.canonical_path(),
        version,
        runtime.git_program,
        runtime.advisory_tree_sha256,
        &selection.actions.cargo_deny_advisories,
        runner,
        clock,
    )
    .map_err(|_| FinalizeError::Advisory)?;
    materialize_release_notes(checkout, version, &paths.notes)?;
    if selection.mode == SelectionMode::Signed {
        let smctl = selection
            .tools
            .smctl
            .as_ref()
            .ok_or(FinalizeError::SelectionInvalid)?;
        run_action(
            selection
                .actions
                .signing_auth_preflight
                .as_ref()
                .ok_or(FinalizeError::SelectionInvalid)?,
            &BTreeMap::from([(
                "{smctl_path}",
                path_text(&smctl.path, FinalizeError::SigningRuntime)?,
            )]),
            None,
            "signing_auth_preflight",
            runner,
        )?;
    }

    record_phase(runner, PHASE_4_BUILD)?;
    run_action(
        &selection.actions.npm_ci,
        &BTreeMap::new(),
        None,
        "npm_ci",
        runner,
    )?;
    run_action(
        &selection.actions.npm_build,
        &BTreeMap::new(),
        None,
        "npm_build",
        runner,
    )?;
    let mut msvc_env = selection.msvc_env_overlay();
    msvc_env.insert(
        "CARGO_TARGET_DIR".to_owned(),
        path_text(&paths.cargo_target, FinalizeError::TransactionDirectory)?,
    );
    run_action(
        &selection.actions.cargo_release_build,
        &BTreeMap::new(),
        Some(&msvc_env),
        "cargo_release_build",
        runner,
    )?;
    copy_from_checkout(
        checkout,
        &format!(
            "{}/release/solstone-windows-app.exe",
            paths.cargo_target_relative
        ),
        &paths.stage,
        STAGED_EXECUTABLE,
        FinalizeError::BuildArtifact,
    )?;

    record_phase(runner, PHASE_5_VELOPACK)?;
    let seed_digests = seed_delta_bases(checkout, &paths.output, &request.delta_base_fulls)?;
    let mut vpk_args = substitute_args(
        &selection.actions.vpk_pack,
        &BTreeMap::from([
            ("{version}", version.to_owned()),
            (
                "{stage_dir}",
                path_text(&paths.stage, FinalizeError::TransactionDirectory)?,
            ),
            (
                "{output_dir}",
                path_text(&paths.output, FinalizeError::TransactionDirectory)?,
            ),
            (
                "{release_notes}",
                path_text(&paths.notes, FinalizeError::ReleaseNotes)?,
            ),
        ]),
    )?;
    if selection.mode == SelectionMode::Signed {
        let alias = runtime
            .signing_keypair_alias
            .filter(|value| safe_alias(value))
            .ok_or(FinalizeError::SigningRuntime)?;
        let smctl = selection
            .actions
            .smctl_sign
            .as_ref()
            .ok_or(FinalizeError::SelectionInvalid)?;
        let template = sign_template(smctl, alias)?;
        vpk_args.push("--signTemplate".to_owned());
        vpk_args.push(template);
    }
    run_program(
        &selection.actions.vpk_pack,
        &vpk_args,
        None,
        "vpk_pack",
        runner,
    )?;
    let has_delta = reconcile_vpk_output(version, &paths.output, &seed_digests)?;
    rust_release_manifest::validate_release_ledgers(&paths.output, version, has_delta)
        .map_err(|_| FinalizeError::LedgerReconciliation)?;

    record_phase(runner, PHASE_6_BASELINE_CANDIDATE)?;
    let names = BundleNames::for_version(version);
    let signing_mode = match selection.mode {
        SelectionMode::Unsigned => {
            verify_release_signing(SigningVerificationRequest::Unsigned, runner)
                .map_err(|_| FinalizeError::SigningVerification)?
                .signing_mode
        }
        SelectionMode::Signed => {
            let policy_bytes = checkout
                .read(SIGNING_POLICY, "signing policy")
                .map_err(|_| FinalizeError::SigningVerification)?;
            let policy = SigningPolicy::parse(&policy_bytes)
                .map_err(|_| FinalizeError::SigningVerification)?;
            verify_release_signing(
                SigningVerificationRequest::Signed {
                    policy: &policy,
                    candidate_root: &paths.output,
                    setup_relative: names.setup(),
                    selected_signtool: &selection
                        .tools
                        .signtool
                        .as_ref()
                        .ok_or(FinalizeError::SelectionInvalid)?
                        .path,
                    action: selection
                        .actions
                        .signtool_verify
                        .as_ref()
                        .ok_or(FinalizeError::SelectionInvalid)?,
                },
                runner,
            )
            .map_err(|_| FinalizeError::SigningVerification)?
            .signing_mode
        }
    };
    let output = ContainedRoot::new(
        &paths.output,
        "Velopack output",
        UnixModePolicy::AllowExecute,
    )
    .map_err(|_| FinalizeError::ExecutableBaseline)?;
    let stage = ContainedRoot::new(&paths.stage, "Velopack stage", UnixModePolicy::AllowExecute)
        .map_err(|_| FinalizeError::ExecutableBaseline)?;
    let nupkg_bytes = output
        .read(names.full_package(), "full nupkg")
        .map_err(|_| FinalizeError::ExecutableBaseline)?;
    let portable_bytes = output
        .read(names.portable(), "portable ZIP")
        .map_err(|_| FinalizeError::ExecutableBaseline)?;
    let staged_bytes = stage
        .read(STAGED_EXECUTABLE, "staged app executable")
        .map_err(|_| FinalizeError::ExecutableBaseline)?;
    let nupkg = ExecutableContainerReader::read_nupkg(&nupkg_bytes)
        .map_err(|_| FinalizeError::ExecutableBaseline)?;
    let portable = ExecutableContainerReader::read_portable(&portable_bytes)
        .map_err(|_| FinalizeError::ExecutableBaseline)?;
    let staged = executable_evidence(&staged_bytes)?;
    let packaged_executable = compare_executable_baseline(&nupkg, &portable, &staged)
        .map_err(|_| FinalizeError::ExecutableBaseline)?;

    let candidate = create_candidate_temp(checkout.canonical_path(), version)
        .map_err(|_| FinalizeError::CandidateAssembly)?;
    let artifact_names = names.artifact_names(has_delta);
    copy_candidate_artifacts(&output, &candidate.path(), &artifact_names)?;

    record_phase(runner, PHASE_7_EVIDENCE)?;
    let facts = rust_release_manifest::gather_checkout_facts_from_binding(
        checkout.canonical_path(),
        version,
        source,
    )
    .map_err(|_| FinalizeError::EvidenceConstruction)?;
    let artifacts = hash_candidate_artifacts(&candidate.path(), &artifact_names)?;
    let evidence = ReleaseEvidence {
        schema_version: 1,
        product: PRODUCT.to_owned(),
        version: version.to_owned(),
        source_commit: source.commit.clone(),
        source_dirty: false,
        cargo_lock_sha256: source.cargo_lock_sha256.clone(),
        rust: RustEvidence {
            rustc_verbose: safe_tools.rustc_verbose,
            cargo_version: safe_tools.cargo_version,
        },
        target: TargetEvidence::Compiled {
            triple: TARGET_TRIPLE.to_owned(),
            profile: TARGET_PROFILE.to_owned(),
            features: TARGET_FEATURES
                .iter()
                .map(|feature| (*feature).to_owned())
                .collect(),
        },
        native_tools: safe_tools.native_tools,
        dependency_policy: DependencyPolicy {
            cargo_deny_version: safe_tools.cargo_deny_version,
            deterministic_gate: "pass".to_owned(),
            advisory_checked_at: advisory.checked_at.clone(),
        },
        active_exceptions: facts.active_exceptions.clone(),
        packaged_executable,
        artifacts,
    };
    let manifest_bytes = rust_release_manifest::render_release_evidence(&evidence)
        .map_err(|_| FinalizeError::EvidenceConstruction)?;
    write_new_synced(
        &candidate.path().join(companion_basename()),
        &manifest_bytes,
        FinalizeError::EvidenceConstruction,
    )?;
    rust_release_manifest::validate_release_dir_with_facts(&candidate.path(), &facts)
        .map_err(|_| FinalizeError::ManifestValidation)?;
    let manifest_sha256 = sha256_hex(&manifest_bytes);
    let candidate_relative =
        candidate_relative_path(version).map_err(|_| FinalizeError::EvidenceConstruction)?;
    let receipt = finalization_receipt(
        version,
        source,
        &manifest_sha256,
        u64::try_from(artifact_names.len() + 1).map_err(|_| FinalizeError::EvidenceConstruction)?,
        &request.selection_record,
        signing_mode,
        &advisory,
    );
    let staged_receipt = stage_finalization_receipt(checkout.canonical_path(), &receipt)
        .map_err(|_| FinalizeError::ReceiptStaging)?;
    source_verifier
        .reverify(source)
        .map_err(|_| FinalizeError::SourceReverification)?;
    validate_candidate_unchanged(&candidate.path(), &facts, &manifest_bytes, &manifest_sha256)?;

    record_phase(runner, PHASE_8_PROMOTION)?;
    validate_candidate_unchanged(&candidate.path(), &facts, &manifest_bytes, &manifest_sha256)?;
    candidate
        .promote()
        .map_err(|_| FinalizeError::CandidatePromotion)?;
    let receipt_relative = staged_receipt
        .promote()
        .map_err(|_| FinalizeError::ReceiptPromotion)?;
    Ok(FinalizeResult {
        version: version.to_owned(),
        candidate_relative_path: candidate_relative,
        receipt_relative_path: receipt_relative,
        manifest_sha256,
        signing_mode: signing_mode.to_owned(),
    })
}

fn validate_candidate_unchanged(
    candidate: &Path,
    facts: &rust_release_manifest::CheckoutFacts,
    manifest_bytes: &[u8],
    manifest_sha256: &str,
) -> Result<(), FinalizeError> {
    rust_release_manifest::validate_release_dir_with_facts(candidate, facts)
        .map_err(|_| FinalizeError::ManifestValidation)?;
    let candidate_root = ContainedRoot::new(
        candidate,
        "candidate staging directory",
        UnixModePolicy::AllowExecute,
    )
    .map_err(|_| FinalizeError::ManifestValidation)?;
    let final_manifest_bytes = candidate_root
        .read(&companion_basename(), "companion manifest")
        .map_err(|_| FinalizeError::ManifestValidation)?;
    if final_manifest_bytes != manifest_bytes
        || sha256_hex(&final_manifest_bytes) != manifest_sha256
    {
        return Err(FinalizeError::ManifestValidation);
    }
    Ok(())
}

fn record_phase<R: CommandRunner + ?Sized>(
    runner: &R,
    phase: &'static str,
) -> Result<(), FinalizeError> {
    runner
        .record_phase(phase)
        .map_err(|_| FinalizeError::PhaseTransition)
}

fn create_transaction_paths(
    checkout: &ContainedRoot,
    version: &str,
) -> Result<TransactionPaths, FinalizeError> {
    let root_relative = format!("target/release-finalizer/{version}");
    if fs::symlink_metadata(checkout.canonical_path().join(&root_relative)).is_ok() {
        return Err(FinalizeError::TransactionDirectory);
    }
    create_contained_directory(checkout.path(), checkout.canonical_path(), &root_relative)
        .map_err(|_| FinalizeError::TransactionDirectory)?;
    let stage_relative = format!("{root_relative}/vpk-stage");
    let output_relative = format!("{root_relative}/vpk-output");
    let cargo_target_relative = format!("{root_relative}/cargo-target");
    for relative in [&stage_relative, &output_relative, &cargo_target_relative] {
        create_contained_directory(checkout.path(), checkout.canonical_path(), relative)
            .map_err(|_| FinalizeError::TransactionDirectory)?;
        let inventory = artifact_fs::walk_directory(
            &checkout.canonical_path().join(relative),
            "new release transaction directory",
            UnixModePolicy::AllowExecute,
        )
        .map_err(|_| FinalizeError::TransactionDirectory)?;
        if !inventory.files.is_empty() || !inventory.directories.is_empty() {
            return Err(FinalizeError::TransactionDirectory);
        }
    }
    let notes_relative = format!("{root_relative}/release-notes.md");
    Ok(TransactionPaths {
        cargo_target: checkout.canonical_path().join(&cargo_target_relative),
        cargo_target_relative,
        stage: checkout.canonical_path().join(&stage_relative),
        output: checkout.canonical_path().join(&output_relative),
        notes: checkout.canonical_path().join(&notes_relative),
    })
}

fn materialize_release_notes(
    checkout: &ContainedRoot,
    version: &str,
    destination: &Path,
) -> Result<(), FinalizeError> {
    let bytes = checkout
        .read("CHANGELOG.md", "CHANGELOG.md")
        .map_err(|_| FinalizeError::ReleaseNotes)?;
    let source = std::str::from_utf8(&bytes).map_err(|_| FinalizeError::ReleaseNotes)?;
    let header = format!("## [{version}]");
    let mut lines = source.lines();
    let mut found = false;
    let mut body = Vec::new();
    for line in lines.by_ref() {
        if line.starts_with(&header)
            && line
                .strip_prefix(&header)
                .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with(" - "))
        {
            found = true;
            break;
        }
    }
    if found {
        for line in lines {
            if line.starts_with("## [") {
                break;
            }
            body.push(line);
        }
    }
    while body.first().is_some_and(|line| line.trim().is_empty()) {
        body.remove(0);
    }
    while body.last().is_some_and(|line| line.trim().is_empty()) {
        body.pop();
    }
    if !found || body.is_empty() {
        return Err(FinalizeError::ReleaseNotes);
    }
    write_new_synced(
        destination,
        body.join("\n").as_bytes(),
        FinalizeError::ReleaseNotes,
    )
}

fn run_action<R: CommandRunner + ?Sized>(
    action: &SelectedAction,
    replacements: &BTreeMap<&str, String>,
    env: Option<&BTreeMap<String, String>>,
    name: &'static str,
    runner: &R,
) -> Result<(), FinalizeError> {
    let args = substitute_args(action, replacements)?;
    run_program(action, &args, env, name, runner)
}

fn run_program<R: CommandRunner + ?Sized>(
    action: &SelectedAction,
    args: &[String],
    env: Option<&BTreeMap<String, String>>,
    name: &'static str,
    runner: &R,
) -> Result<(), FinalizeError> {
    let output = runner
        .run(&action.program, args, None, env)
        .map_err(|_| FinalizeError::ActionFailed { action: name })?;
    if output.status != 0 {
        return Err(FinalizeError::ActionFailed { action: name });
    }
    Ok(())
}

fn substitute_args(
    action: &SelectedAction,
    replacements: &BTreeMap<&str, String>,
) -> Result<Vec<String>, FinalizeError> {
    action
        .argv
        .iter()
        .map(|arg| {
            if let Some(value) = replacements.get(arg.as_str()) {
                Ok(value.clone())
            } else if arg.contains('{') || arg.contains('}') {
                Err(FinalizeError::SelectionInvalid)
            } else {
                Ok(arg.clone())
            }
        })
        .collect()
}

fn sign_template(action: &SelectedAction, alias: &str) -> Result<String, FinalizeError> {
    if action.argv.iter().map(String::as_str).ne([
        "sign",
        "--keypair-alias",
        "{keypair_alias}",
        "--input",
        "{file}",
    ]) {
        return Err(FinalizeError::SelectionInvalid);
    }
    let program = path_text(&action.program, FinalizeError::SigningRuntime)?;
    Ok(format!(
        "\"{program}\" sign --keypair-alias {alias} --input {{{{file}}}}"
    ))
}

fn safe_alias(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn copy_from_checkout(
    checkout: &ContainedRoot,
    source_relative: &str,
    destination_root: &Path,
    destination_name: &str,
    error: FinalizeError,
) -> Result<(), FinalizeError> {
    let bytes = checkout
        .read(source_relative, "release build executable")
        .map_err(|_| error.clone())?;
    if bytes.is_empty() {
        return Err(error);
    }
    write_new_synced(&destination_root.join(destination_name), &bytes, error)
}

fn seed_delta_bases(
    checkout: &ContainedRoot,
    output: &Path,
    basenames: &[String],
) -> Result<BTreeMap<String, String>, FinalizeError> {
    let mut digests = BTreeMap::new();
    for basename in basenames {
        let relative = format!("Releases/{basename}");
        let bytes = checkout
            .read(&relative, "delta-base full package")
            .map_err(|_| FinalizeError::DeltaSeed)?;
        write_new_synced(&output.join(basename), &bytes, FinalizeError::DeltaSeed)?;
        digests.insert(basename.clone(), sha256_hex(&bytes));
    }
    Ok(digests)
}

fn reconcile_vpk_output(
    version: &str,
    output_path: &Path,
    seed_digests: &BTreeMap<String, String>,
) -> Result<bool, FinalizeError> {
    let names = BundleNames::for_version(version);
    let output = ContainedRoot::new(output_path, "Velopack output", UnixModePolicy::AllowExecute)
        .map_err(|_| FinalizeError::VelopackInventory)?;
    let inventory = artifact_fs::walk_directory(
        output.path(),
        "Velopack output",
        UnixModePolicy::AllowExecute,
    )
    .map_err(|_| FinalizeError::VelopackInventory)?;
    if !inventory.directories.is_empty() {
        return Err(FinalizeError::VelopackInventory);
    }
    let has_delta = inventory.files.contains(names.delta_package());
    let mut expected = names.artifact_names(has_delta);
    expected.remove(names.setup());
    expected.insert(BundleNames::velopack_setup_exe().to_owned());
    expected.extend(seed_digests.keys().cloned());
    if inventory.files != expected {
        return Err(FinalizeError::VelopackInventory);
    }
    for (basename, expected_digest) in seed_digests {
        let bytes = output
            .read(basename, "delta-base full package")
            .map_err(|_| FinalizeError::DeltaSeedChanged)?;
        if sha256_hex(&bytes) != *expected_digest {
            return Err(FinalizeError::DeltaSeedChanged);
        }
    }
    fs::rename(
        output
            .canonical_path()
            .join(BundleNames::velopack_setup_exe()),
        output.canonical_path().join(names.setup()),
    )
    .map_err(|_| FinalizeError::VelopackInventory)?;
    rewrite_assets_installer(&output, names.assets(), names.setup())?;

    let final_inventory = artifact_fs::walk_directory(
        output.path(),
        "reconciled Velopack output",
        UnixModePolicy::AllowExecute,
    )
    .map_err(|_| FinalizeError::VelopackInventory)?;
    let mut final_expected = names.artifact_names(has_delta);
    final_expected.extend(seed_digests.keys().cloned());
    if !final_inventory.directories.is_empty() || final_inventory.files != final_expected {
        return Err(FinalizeError::VelopackInventory);
    }
    Ok(has_delta)
}

fn rewrite_assets_installer(
    output: &ContainedRoot,
    assets_name: &str,
    setup_name: &str,
) -> Result<(), FinalizeError> {
    let bytes = output
        .read(assets_name, "assets.win.json")
        .map_err(|_| FinalizeError::AssetsReconciliation)?;
    let mut records: Vec<AssetsRecord> =
        serde_json::from_slice(&bytes).map_err(|_| FinalizeError::AssetsReconciliation)?;
    let installers: Vec<usize> = records
        .iter()
        .enumerate()
        .filter(|(_, record)| record.asset_type == "Installer")
        .map(|(index, _)| index)
        .collect();
    let [installer_index] = installers.as_slice() else {
        return Err(FinalizeError::AssetsReconciliation);
    };
    let installer = &mut records[*installer_index];
    if installer.relative_file_name != BundleNames::velopack_setup_exe() {
        return Err(FinalizeError::AssetsReconciliation);
    }
    installer.relative_file_name = setup_name.to_owned();
    let rendered = serde_json::to_vec(&records).map_err(|_| FinalizeError::AssetsReconciliation)?;
    let mut file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(output.canonical_path().join(assets_name))
        .map_err(|_| FinalizeError::AssetsReconciliation)?;
    file.write_all(&rendered)
        .and_then(|()| file.sync_all())
        .map_err(|_| FinalizeError::AssetsReconciliation)
}

fn executable_evidence(
    bytes: &[u8],
) -> Result<rust_release_manifest::PackagedExecutableEvidence, FinalizeError> {
    let count = u64::try_from(bytes.len()).map_err(|_| FinalizeError::ExecutableBaseline)?;
    if count == 0 {
        return Err(FinalizeError::ExecutableBaseline);
    }
    Ok(rust_release_manifest::PackagedExecutableEvidence {
        sha256: sha256_hex(bytes),
        bytes: count,
    })
}

fn copy_candidate_artifacts(
    output: &ContainedRoot,
    candidate: &Path,
    names: &BTreeSet<String>,
) -> Result<(), FinalizeError> {
    for name in names {
        let bytes = output
            .read(name, name)
            .map_err(|_| FinalizeError::CandidateAssembly)?;
        write_new_synced(
            &candidate.join(name),
            &bytes,
            FinalizeError::CandidateAssembly,
        )?;
    }
    Ok(())
}

fn hash_candidate_artifacts(
    candidate: &Path,
    names: &BTreeSet<String>,
) -> Result<Vec<ArtifactEvidence>, FinalizeError> {
    let root = ContainedRoot::new(
        candidate,
        "candidate staging directory",
        UnixModePolicy::AllowExecute,
    )
    .map_err(|_| FinalizeError::EvidenceConstruction)?;
    names
        .iter()
        .map(|name| {
            let bytes = root
                .read(name, name)
                .map_err(|_| FinalizeError::EvidenceConstruction)?;
            Ok(ArtifactEvidence {
                path: name.clone(),
                sha256: sha256_hex(&bytes),
                bytes: u64::try_from(bytes.len())
                    .map_err(|_| FinalizeError::EvidenceConstruction)?,
            })
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn finalization_receipt(
    version: &str,
    source: &SourceBinding,
    manifest_sha256: &str,
    file_count: u64,
    selection_record: &[u8],
    signing_mode: &str,
    advisory: &AdvisoryProvenance,
) -> FinalizationReceipt {
    FinalizationReceipt {
        schema: FINALIZATION_RECEIPT_SCHEMA.to_owned(),
        product: PRODUCT.to_owned(),
        version: version.to_owned(),
        target: TARGET_TRIPLE.to_owned(),
        source_commit: source.commit.clone(),
        cargo_lock_sha256: source.cargo_lock_sha256.clone(),
        ui_package_lock_sha256: source.ui_package_lock_sha256.clone(),
        companion_manifest: CompanionManifestReceipt {
            filename: companion_basename(),
            sha256: manifest_sha256.to_owned(),
        },
        candidate: CandidateReceipt {
            relative_path: candidate_relative_path(version)
                .expect("finalizer version was already validated by cargo metadata"),
            file_count,
        },
        selection_record_sha256: sha256_hex(selection_record),
        signing_mode: signing_mode.to_owned(),
        advisory_database: AdvisoryDatabaseReceipt {
            source_id: advisory.source_id.clone(),
            commit: advisory.commit.clone(),
            tree_sha256: advisory.tree_sha256.clone(),
            acquired_at: advisory.acquired_at.clone(),
        },
        advisory_checked_at: advisory.checked_at.clone(),
    }
}

fn write_new_synced(path: &Path, bytes: &[u8], error: FinalizeError) -> Result<(), FinalizeError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| error.clone())?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| error)
}

fn cleanup_failed_transaction(
    checkout_root: &Path,
    version: &str,
    delta_base_fulls: &[String],
) -> Result<(), FinalizeError> {
    let catalog = ReleaseCleanupCatalog::for_version(checkout_root, version)
        .map_err(|_| FinalizeError::FailureCleanup)?;
    DeletionPlan::materialize(&catalog, delta_base_fulls)
        .and_then(DeletionPlan::execute)
        .map_err(|_| FinalizeError::FailureCleanup)
}

fn path_text(path: &Path, error: FinalizeError) -> Result<String, FinalizeError> {
    path.to_str().map(str::to_owned).ok_or(error)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
