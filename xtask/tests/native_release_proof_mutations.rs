// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#[allow(dead_code)]
mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{json, Value};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use support::{
    action_uses_script, build_velopack_nupkg, build_velopack_portable, checkout_facts, request,
    FakeReleaseCheckout, FakeReleaseRunner, NativeProofMutation, WitnessEvent, CHECKED_AT,
    POWERSHELL, VERSION,
};
use xtask::native_release_proof::{
    prove_native, NativeProofError, NativeProofRuntime, STEP_10_REVALIDATE, STEP_11_RECEIPT,
    STEP_11_RECEIPT_STAGED, STEP_1_CLASSIFY, STEP_2_IDENTITY, STEP_3_TOOLS, STEP_4_CONTAINERS,
    STEP_5_ROOT_READY, STEP_6_INSTALL, STEP_7_INSTALLED_IDENTITY, STEP_8_DUMP_STATE, STEP_9_SMOKE,
};
use xtask::release_clock::{Clock, ClockError, FixedClock, UtcTimestamp};
use xtask::release_container::{ContainerKind, ReleaseContainerError};
use xtask::release_finalizer::finalize;
use xtask::release_receipt::{
    render_finalization_receipt, render_windows_native_proof_receipt, FinalizationReceipt,
    WindowsNativeProofReceipt,
};
use xtask::release_selection::SelectionMode;
use xtask::rust_release_manifest::{
    companion_basename, render_release_evidence, validate_manifest_bytes, BundleNames,
    ReleaseEvidence,
};

const PROVED_AT: &str = "2026-07-21T13:00:00Z";

#[derive(Clone, Copy)]
struct SeamExpectation {
    resolver: bool,
    installer: bool,
    smoke: bool,
}

struct PreparedProof {
    checkout: FakeReleaseCheckout,
    runner: FakeReleaseRunner,
    candidate: PathBuf,
    facts: xtask::rust_release_manifest::CheckoutFacts,
    event_start: usize,
}

#[test]
fn invalid_candidate_and_receipt_binding_fail_before_native_actions() {
    run_case(
        "invalid-candidate",
        SelectionMode::Signed,
        NativeProofMutation::None,
        |prepared| {
            fs::write(prepared.candidate.join("unknown.bin"), b"unknown")
                .expect("write unknown candidate file");
        },
        NativeProofError::InitialClassification,
        "strict whole-directory classification",
        STEP_1_CLASSIFY,
        SeamExpectation::none(),
    );
    run_case(
        "missing-finalization-receipt",
        SelectionMode::Signed,
        NativeProofMutation::None,
        |prepared| {
            fs::remove_file(finalization_receipt_path(prepared))
                .expect("remove finalization receipt");
        },
        NativeProofError::FinalizationReceipt,
        "finalization receipt",
        STEP_2_IDENTITY,
        SeamExpectation::none(),
    );
    run_case(
        "mismatched-finalization-receipt",
        SelectionMode::Signed,
        NativeProofMutation::None,
        |prepared| {
            mutate_finalization_receipt(prepared, |receipt| {
                receipt.companion_manifest.sha256 = "0".repeat(64);
            });
        },
        NativeProofError::FinalizationReceiptMismatch,
        "does not identify this candidate",
        STEP_2_IDENTITY,
        SeamExpectation::none(),
    );
    run_case(
        "unsigned-candidate",
        SelectionMode::Unsigned,
        NativeProofMutation::None,
        |_| {},
        NativeProofError::UnsignedCandidate,
        "refuses an unsigned candidate",
        STEP_3_TOOLS,
        SeamExpectation::none(),
    );
}

#[test]
fn resolver_stdout_encoding_fails_before_native_actions() {
    run_case(
        "resolver-stdout-legacy-codepage",
        SelectionMode::Signed,
        NativeProofMutation::ResolverStdoutLegacyCodepageBytes,
        |_| {},
        NativeProofError::ToolResolverEncoding,
        "preflight stdout",
        STEP_3_TOOLS,
        SeamExpectation {
            resolver: true,
            installer: false,
            smoke: false,
        },
    );
}

#[test]
fn setup_source_is_exactly_the_candidate_canonical_setup() {
    run_case(
        "missing-canonical-setup",
        SelectionMode::Signed,
        NativeProofMutation::None,
        |prepared| {
            fs::remove_file(canonical_setup(prepared)).expect("remove canonical setup");
        },
        NativeProofError::InitialClassification,
        "strict whole-directory classification",
        STEP_1_CLASSIFY,
        SeamExpectation::none(),
    );
    run_case(
        "outside-attractive-setup",
        SelectionMode::Signed,
        NativeProofMutation::None,
        |prepared| {
            fs::remove_file(canonical_setup(prepared)).expect("remove canonical setup");
            let outside = prepared
                .checkout
                .root()
                .join(format!("solstone-setup-{VERSION}.exe"));
            fs::write(&outside, b"attractive outside setup").expect("write outside setup");
            assert!(outside.is_file());
        },
        NativeProofError::InitialClassification,
        "strict whole-directory classification",
        STEP_1_CLASSIFY,
        SeamExpectation::none(),
    );
}

#[test]
fn isolated_install_root_and_installer_fail_closed() {
    for (label, mutation, error, subject, installer) in [
        (
            "nonempty-proof-root",
            NativeProofMutation::NonemptyProofRoot,
            NativeProofError::ProofRoot,
            "newly empty isolated install root",
            false,
        ),
        (
            "preexisting-installed-app",
            NativeProofMutation::PreexistingInstalledApp,
            NativeProofError::PreexistingInstalledApp,
            "already contains the canonical app",
            false,
        ),
        (
            "preexisting-matching-noop-install",
            NativeProofMutation::PreexistingMatchingInstalledApp,
            NativeProofError::PreexistingInstalledApp,
            "already contains the canonical app",
            false,
        ),
        (
            "successful-noop-installer",
            NativeProofMutation::InstallerNoOp,
            NativeProofError::InstalledAppMissing,
            "without creating the canonical app",
            true,
        ),
        (
            "failed-installer",
            NativeProofMutation::InstallerFailure,
            NativeProofError::SetupFailed,
            "setup exited nonzero",
            true,
        ),
        (
            "timed-out-installer",
            NativeProofMutation::InstallerTimeout,
            NativeProofError::SetupFailed,
            "setup exited nonzero",
            true,
        ),
        (
            "skipped-installer",
            NativeProofMutation::InstallerSkipped,
            NativeProofError::SetupFailed,
            "setup exited nonzero",
            true,
        ),
        (
            "missing-post-install-app",
            NativeProofMutation::InstallerMissingApp,
            NativeProofError::InstalledAppMissing,
            "without creating the canonical app",
            true,
        ),
    ] {
        let last_step = if installer {
            STEP_6_INSTALL
        } else {
            STEP_5_ROOT_READY
        };
        run_case(
            label,
            SelectionMode::Signed,
            mutation,
            |_| {},
            error,
            subject,
            last_step,
            SeamExpectation {
                resolver: true,
                installer,
                smoke: false,
            },
        );
    }
}

#[test]
fn every_executable_identity_source_is_bound_to_the_manifest() {
    // Pure ZIP-name, exact-duplicate, and member-kind rejection lives in
    // release_container_baseline.rs. These cases prove engine integration.
    run_case(
        "installed-app-diverges",
        SelectionMode::Signed,
        NativeProofMutation::InstalledAppDiverges,
        |_| {},
        NativeProofError::InstalledBaselineMismatch,
        "installed app disagrees",
        STEP_7_INSTALLED_IDENTITY,
        SeamExpectation::installed_without_smoke(),
    );
    for (label, mutation) in [
        ("nupkg-app-diverges", ContainerMutation::NupkgDiverges),
        ("portable-app-diverges", ContainerMutation::PortableDiverges),
        (
            "nupkg-portable-differ",
            ContainerMutation::BothContainersDiffer,
        ),
        (
            "manifest-baseline-diverges",
            ContainerMutation::ManifestBaselineDiverges,
        ),
    ] {
        run_case(
            label,
            SelectionMode::Signed,
            NativeProofMutation::None,
            |prepared| mutate_container_identity(prepared, mutation),
            NativeProofError::ContainerBaseline,
            "manifest baseline",
            STEP_4_CONTAINERS,
            SeamExpectation {
                resolver: true,
                installer: false,
                smoke: false,
            },
        );
    }
    for (label, mutation, error, subject) in [
        (
            "nupkg-member-missing",
            ContainerMutation::NupkgMemberMissing,
            NativeProofError::ExecutableContainer(ReleaseContainerError::MissingCanonicalMember {
                container: ContainerKind::Nupkg,
            }),
            "missing the exact canonical app member",
        ),
        (
            "nupkg-member-case-collision",
            ContainerMutation::NupkgMemberCaseCollision,
            NativeProofError::ExecutableContainer(ReleaseContainerError::EntryCaseCollision {
                container: ContainerKind::Nupkg,
            }),
            "ASCII case-folding entry collisions",
        ),
    ] {
        run_case(
            label,
            SelectionMode::Signed,
            NativeProofMutation::None,
            |prepared| mutate_container_identity(prepared, mutation),
            error,
            subject,
            STEP_4_CONTAINERS,
            SeamExpectation {
                resolver: true,
                installer: false,
                smoke: false,
            },
        );
    }
    for (label, mutation, error, subject) in [
        (
            "nupkg-container-read-failure",
            NativeProofMutation::NupkgContainerReadFailure,
            NativeProofError::ExecutableRead(ContainerKind::Nupkg),
            "full nupkg could not be stable-read",
        ),
        (
            "portable-container-read-failure",
            NativeProofMutation::PortableContainerReadFailure,
            NativeProofError::ExecutableRead(ContainerKind::Portable),
            "portable ZIP could not be stable-read",
        ),
    ] {
        run_case(
            label,
            SelectionMode::Signed,
            mutation,
            |_| {},
            error,
            subject,
            STEP_4_CONTAINERS,
            SeamExpectation {
                resolver: true,
                installer: false,
                smoke: false,
            },
        );
    }
}

#[test]
fn installed_dump_state_version_is_exact_and_canonical() {
    for (label, mutation, error, subject) in [
        (
            "wrong-dump-state-version",
            NativeProofMutation::DumpStateWrongVersion,
            NativeProofError::DumpStateVersionMismatch,
            "version differs",
        ),
        (
            "malformed-dump-state-version",
            NativeProofMutation::DumpStateMalformed,
            NativeProofError::DumpStateMalformed,
            "malformed --dump-state JSON",
        ),
    ] {
        run_case(
            label,
            SelectionMode::Signed,
            mutation,
            |_| {},
            error,
            subject,
            STEP_8_DUMP_STATE,
            SeamExpectation::installed_without_smoke(),
        );
    }
}

#[test]
fn smoke_selection_and_green_evidence_are_fail_closed() {
    // Pure argv-template rejection lives in release_selection.rs tests. These
    // cases prove that each rejection propagates through the native-proof engine.
    for (label, mutation) in [
        (
            "smoke-missing-app-argument",
            NativeProofMutation::SmokeMissingAppArgument,
        ),
        (
            "smoke-missing-version-argument",
            NativeProofMutation::SmokeMissingVersionArgument,
        ),
        (
            "smoke-missing-hash-argument",
            NativeProofMutation::SmokeMissingHashArgument,
        ),
        (
            "smoke-fallback-attempt",
            NativeProofMutation::SmokeFallbackEnabled,
        ),
        (
            "smoke-wrong-hash-template",
            NativeProofMutation::SmokeWrongHashTemplate,
        ),
        (
            "smoke-wrong-version-template",
            NativeProofMutation::SmokeWrongVersionTemplate,
        ),
    ] {
        run_case(
            label,
            SelectionMode::Signed,
            mutation,
            |_| {},
            NativeProofError::ToolSelection,
            "signed tool selection is invalid",
            STEP_3_TOOLS,
            SeamExpectation {
                resolver: true,
                installer: false,
                smoke: false,
            },
        );
    }
    for (label, mutation, error, subject) in [
        (
            "smoke-nonzero",
            NativeProofMutation::SmokeFailure,
            NativeProofError::SmokeFailed,
            "health/render smoke failed",
        ),
        (
            "smoke-missing-literal-ok",
            NativeProofMutation::SmokeMissingOk,
            NativeProofError::SmokeEvidenceMissing,
            "did not emit literal SMOKE_OK",
        ),
    ] {
        run_case(
            label,
            SelectionMode::Signed,
            mutation,
            |_| {},
            error,
            subject,
            STEP_9_SMOKE,
            SeamExpectation::all(),
        );
    }
}

#[test]
fn candidate_mutation_during_smoke_invalidates_proof() {
    run_case(
        "smoke-mutates-artifact",
        SelectionMode::Signed,
        NativeProofMutation::SmokeMutatesArtifact,
        |_| {},
        NativeProofError::PostSmokeClassification,
        "failed strict validation after smoke",
        STEP_10_REVALIDATE,
        SeamExpectation::all(),
    );
    run_case(
        "smoke-mutates-companion",
        SelectionMode::Signed,
        NativeProofMutation::SmokeMutatesManifest,
        |_| {},
        NativeProofError::CandidateMutated,
        "companion manifest changed",
        STEP_10_REVALIDATE,
        SeamExpectation::all(),
    );
}

#[test]
fn proof_receipt_and_clock_never_report_unearned_success() {
    run_case(
        "receipt-promotion-race",
        SelectionMode::Signed,
        NativeProofMutation::ReceiptPromotionRace,
        |_| {},
        NativeProofError::Receipt,
        "receipt could not be atomically written",
        STEP_11_RECEIPT_STAGED,
        SeamExpectation::all(),
    );
    for (label, bytes) in [
        ("preexisting-proof-receipt", b"{}".as_slice()),
        ("partial-proof-receipt", b"{\"schema\":".as_slice()),
    ] {
        run_case(
            label,
            SelectionMode::Signed,
            NativeProofMutation::None,
            |prepared| {
                fs::write(proof_receipt_path(prepared), bytes)
                    .expect("write preexisting proof receipt");
            },
            NativeProofError::Receipt,
            "receipt could not be atomically written",
            STEP_11_RECEIPT,
            SeamExpectation::all(),
        );
    }

    // The closed receipt type's pure privacy coverage lives in
    // release_receipt.rs. This engine case proves private input is not accepted
    // or echoed when receipt promotion is refused.
    let private_attempt = br#"{"schema":"solstone.windows-native-proof.v1","private_path":"C:\\Users\\private","host":"box-private","credential":"credential-private","certificate":"certificate-private"}"#;
    assert!(serde_json::from_slice::<WindowsNativeProofReceipt>(private_attempt).is_err());
    run_case(
        "unknown-private-proof-field",
        SelectionMode::Signed,
        NativeProofMutation::None,
        |prepared| {
            fs::write(proof_receipt_path(prepared), private_attempt)
                .expect("write private-field receipt attempt");
        },
        NativeProofError::Receipt,
        "receipt could not be atomically written",
        STEP_11_RECEIPT,
        SeamExpectation::all(),
    );

    let prepared = PreparedProof::new(
        "clock-failure",
        SelectionMode::Signed,
        NativeProofMutation::None,
    );
    let before = flat_file_snapshot(&prepared.candidate);
    let clock = FailingClock::default();
    let error = prepared
        .prove(&clock)
        .expect_err("failing injected clock must block proof");
    assert_eq!(error, NativeProofError::Clock);
    assert_actionable(&error, "UTC proof time");
    assert_private_diagnostic(&error, prepared.checkout.root());
    assert_eq!(clock.calls.load(Ordering::Relaxed), 1);
    assert_eq!(flat_file_snapshot(&prepared.candidate), before);
    assert_no_green_receipt(&prepared);
    let events = prepared.proof_events();
    assert_last_step(&events, STEP_11_RECEIPT);
    assert!(has_resolver(&events));
    assert!(has_installer(&events));
    assert!(has_smoke(&events));
}

impl SeamExpectation {
    const fn none() -> Self {
        Self {
            resolver: false,
            installer: false,
            smoke: false,
        }
    }

    const fn installed_without_smoke() -> Self {
        Self {
            resolver: true,
            installer: true,
            smoke: false,
        }
    }

    const fn all() -> Self {
        Self {
            resolver: true,
            installer: true,
            smoke: true,
        }
    }
}

impl PreparedProof {
    fn new(label: &str, mode: SelectionMode, mutation: NativeProofMutation) -> Self {
        let checkout = FakeReleaseCheckout::new(label, false);
        let runner = FakeReleaseRunner::with_native_proof_mutation(&checkout, mutation);
        finalize(
            checkout.runtime((mode == SelectionMode::Signed).then_some("test-keypair")),
            &request(mode, false),
            &runner,
            &FixedClock::new(CHECKED_AT).expect("create finalization clock"),
        )
        .expect("finalize mutation candidate");
        let candidate = checkout
            .root()
            .join(format!("target/release-candidate/{VERSION}"));
        let facts = checkout_facts(&checkout);
        let event_start = runner.events().len();
        Self {
            checkout,
            runner,
            candidate,
            facts,
            event_start,
        }
    }

    fn prove<C: Clock + ?Sized>(&self, clock: &C) -> Result<(), NativeProofError> {
        prove_native(
            NativeProofRuntime {
                checkout_root: self.checkout.root(),
                facts: &self.facts,
                powershell_bootstrap: Path::new(POWERSHELL).as_os_str(),
            },
            &self.candidate,
            &self.runner,
            clock,
        )
        .map(|_| ())
    }

    fn proof_events(&self) -> Vec<WitnessEvent> {
        self.runner.events()[self.event_start..].to_vec()
    }
}

#[allow(clippy::too_many_arguments)]
fn run_case(
    label: &str,
    mode: SelectionMode,
    mutation: NativeProofMutation,
    mutate: impl FnOnce(&PreparedProof),
    expected_error: NativeProofError,
    subject: &str,
    last_step: &str,
    seams: SeamExpectation,
) {
    let prepared = PreparedProof::new(label, mode, mutation);
    mutate(&prepared);
    let before = flat_file_snapshot(&prepared.candidate);
    let error = prepared
        .prove(&FixedClock::new(PROVED_AT).expect("create proof clock"))
        .expect_err("native proof mutation must fail closed");
    assert_eq!(error, expected_error, "wrong error for {label}");
    assert_actionable(&error, subject);
    assert_private_diagnostic(&error, prepared.checkout.root());
    let events = prepared.proof_events();
    assert_last_step(&events, last_step);
    assert_eq!(
        has_resolver(&events),
        seams.resolver,
        "resolver seam for {label}"
    );
    assert_eq!(
        has_installer(&events),
        seams.installer,
        "installer seam for {label}"
    );
    assert_eq!(has_smoke(&events), seams.smoke, "smoke seam for {label}");
    assert_candidate_effect(label, mutation, &prepared.candidate, before);
    assert_no_green_receipt(&prepared);
}

fn assert_candidate_effect(
    label: &str,
    mutation: NativeProofMutation,
    candidate: &Path,
    mut before: BTreeMap<String, Vec<u8>>,
) {
    let expected = before.clone();
    let mut after = flat_file_snapshot(candidate);
    let names = BundleNames::for_version(VERSION);
    let intentionally_mutated = match mutation {
        NativeProofMutation::NupkgContainerReadFailure => Some(names.full_package().to_owned()),
        NativeProofMutation::PortableContainerReadFailure => Some(names.portable().to_owned()),
        NativeProofMutation::SmokeMutatesArtifact => Some("assets.win.json".to_owned()),
        NativeProofMutation::SmokeMutatesManifest => Some(companion_basename()),
        _ => None,
    };
    if let Some(name) = intentionally_mutated {
        let original = before.get(&name).expect("mutated file existed").clone();
        assert_ne!(
            before.remove(&name),
            after.remove(&name),
            "{label} did not inject its candidate mutation"
        );
        // The fake runner is the adversary in these cases. Restore only its
        // injected mutation after the proof has detected it, then enforce the same
        // byte-identical postcondition used by every other mutation case.
        fs::write(candidate.join(&name), original).expect("restore injected candidate mutation");
    }
    assert_eq!(
        after, before,
        "proof changed another candidate file for {label}"
    );
    assert_eq!(
        flat_file_snapshot(candidate),
        expected,
        "candidate was not byte-identical after {label}"
    );
}

fn assert_actionable(error: &NativeProofError, subject: &str) {
    let message = error.to_string();
    assert!(
        message.contains(subject),
        "diagnostic did not name subject: {message}"
    );
    assert!(
        ["retry", "restore", "rebuild", "repair", "use", "discard"]
            .iter()
            .any(|word| message.contains(word)),
        "diagnostic lacked remediation: {message}"
    );
}

fn assert_private_diagnostic(error: &NativeProofError, checkout: &Path) {
    let message = error.to_string();
    for private in [
        checkout.to_string_lossy().as_ref(),
        r"C:\Users\private",
        "box-private",
        "credential-private",
        "certificate-private",
    ] {
        assert!(
            !message.contains(private),
            "native-proof diagnostic leaked private input: {message}"
        );
    }
}

fn assert_last_step(events: &[WitnessEvent], expected: &str) {
    let last = events.iter().rev().find_map(|event| match event {
        WitnessEvent::Phase(phase) => Some(phase.as_str()),
        WitnessEvent::Invocation { .. } => None,
    });
    assert_eq!(last, Some(expected));
}

fn has_resolver(events: &[WitnessEvent]) -> bool {
    events.iter().any(|event| match event {
        WitnessEvent::Invocation { program, args, .. } => {
            program == Path::new(POWERSHELL)
                && action_uses_script(args, Path::new("packaging/preflight-release-tools.ps1"))
        }
        WitnessEvent::Phase(_) => false,
    })
}

fn has_installer(events: &[WitnessEvent]) -> bool {
    events.iter().any(|event| match event {
        WitnessEvent::Invocation { program, args, .. } => {
            program.ends_with(format!("solstone-setup-{VERSION}.exe"))
                && args.first().map(String::as_str) == Some("--silent")
        }
        WitnessEvent::Phase(_) => false,
    })
}

fn has_smoke(events: &[WitnessEvent]) -> bool {
    events.iter().any(|event| match event {
        WitnessEvent::Invocation { program, args, .. } => {
            program == Path::new(POWERSHELL) && args.iter().any(|arg| arg == "scripts/smoke.ps1")
        }
        WitnessEvent::Phase(_) => false,
    })
}

fn assert_no_green_receipt(prepared: &PreparedProof) {
    let path = proof_receipt_path(prepared);
    let Ok(bytes) = fs::read(path) else {
        return;
    };
    let green = serde_json::from_slice::<WindowsNativeProofReceipt>(&bytes)
        .ok()
        .and_then(|receipt| render_windows_native_proof_receipt(&receipt).ok())
        .is_some_and(|canonical| canonical == bytes);
    assert!(!green, "mutation left a green proof receipt");
}

fn flat_file_snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fs::read_dir(root)
        .expect("read candidate")
        .map(|entry| {
            let entry = entry.expect("read candidate entry");
            let name = entry
                .file_name()
                .into_string()
                .expect("candidate name is UTF-8");
            assert!(entry.file_type().expect("read candidate kind").is_file());
            (name, fs::read(entry.path()).expect("read candidate file"))
        })
        .collect()
}

fn canonical_setup(prepared: &PreparedProof) -> PathBuf {
    prepared
        .candidate
        .join(format!("solstone-setup-{VERSION}.exe"))
}

fn finalization_receipt_path(prepared: &PreparedProof) -> PathBuf {
    prepared.checkout.root().join(format!(
        "target/release-evidence/{VERSION}/rust-release-finalization.json"
    ))
}

fn proof_receipt_path(prepared: &PreparedProof) -> PathBuf {
    prepared.checkout.root().join(format!(
        "target/release-evidence/{VERSION}/windows-native-proof.json"
    ))
}

fn mutate_finalization_receipt(
    prepared: &PreparedProof,
    mutate: impl FnOnce(&mut FinalizationReceipt),
) {
    let path = finalization_receipt_path(prepared);
    let mut receipt: FinalizationReceipt =
        serde_json::from_slice(&fs::read(&path).expect("read finalization receipt"))
            .expect("parse finalization receipt");
    mutate(&mut receipt);
    fs::write(
        path,
        render_finalization_receipt(&receipt).expect("render finalization receipt"),
    )
    .expect("write finalization receipt");
}

#[derive(Clone, Copy)]
enum ContainerMutation {
    NupkgDiverges,
    PortableDiverges,
    BothContainersDiffer,
    ManifestBaselineDiverges,
    NupkgMemberMissing,
    NupkgMemberCaseCollision,
}

fn mutate_container_identity(prepared: &PreparedProof, mutation: ContainerMutation) {
    match mutation {
        ContainerMutation::NupkgDiverges => replace_nupkg(
            prepared,
            build_velopack_nupkg(
                "lib/app/solstone-windows-app.exe",
                b"divergent nupkg app",
                false,
            ),
        ),
        ContainerMutation::PortableDiverges => {
            replace_portable(prepared, build_velopack_portable(b"divergent portable app"))
        }
        ContainerMutation::BothContainersDiffer => {
            replace_nupkg(
                prepared,
                build_velopack_nupkg("lib/app/solstone-windows-app.exe", b"nupkg app", false),
            );
            replace_portable(prepared, build_velopack_portable(b"portable different app"));
        }
        ContainerMutation::ManifestBaselineDiverges => {
            rewrite_manifest_and_receipt(prepared, |evidence| {
                evidence.packaged_executable.sha256 = "0".repeat(64);
            });
        }
        ContainerMutation::NupkgMemberMissing => replace_nupkg(
            prepared,
            build_velopack_nupkg("lib/app/other.exe", b"other", false),
        ),
        ContainerMutation::NupkgMemberCaseCollision => replace_nupkg(
            prepared,
            build_velopack_nupkg("lib/app/solstone-windows-app.exe", b"first", true),
        ),
    }
}

fn replace_nupkg(prepared: &PreparedProof, bytes: Vec<u8>) {
    let name = format!("Solstone-{VERSION}-full.nupkg");
    fs::write(prepared.candidate.join(&name), &bytes).expect("replace nupkg");

    let releases_path = prepared.candidate.join("RELEASES");
    let releases_bytes = fs::read(&releases_path).expect("read RELEASES");
    let (bom, body) = releases_bytes.split_at(3);
    assert_eq!(bom, [0xef, 0xbb, 0xbf]);
    let mut rows: Vec<String> = std::str::from_utf8(body)
        .expect("RELEASES is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect();
    let row = rows
        .iter_mut()
        .find(|row| row.split_whitespace().nth(1) == Some(name.as_str()))
        .expect("current nupkg RELEASES row");
    *row = format!("{} {name} {}", hex_sha1(&bytes), bytes.len());
    rows.sort();
    let mut changed_releases = bom.to_vec();
    changed_releases.extend_from_slice(rows.join("\n").as_bytes());
    changed_releases.push(b'\n');
    fs::write(&releases_path, changed_releases).expect("rewrite RELEASES");

    let feed_path = prepared.candidate.join("releases.win.json");
    let mut feed: Value =
        serde_json::from_slice(&fs::read(&feed_path).expect("read feed")).expect("parse feed");
    let record = feed["Assets"]
        .as_array_mut()
        .expect("feed assets")
        .iter_mut()
        .find(|record| record["FileName"].as_str() == Some(name.as_str()))
        .expect("current nupkg feed row");
    record["SHA1"] = json!(hex_sha1(&bytes));
    record["SHA256"] = json!(hex_sha256(&bytes));
    record["Size"] = json!(bytes.len());
    fs::write(&feed_path, serde_json::to_vec(&feed).expect("render feed")).expect("rewrite feed");
    refresh_manifest_artifacts(prepared, &[&name, "RELEASES", "releases.win.json"]);
}

fn replace_portable(prepared: &PreparedProof, bytes: Vec<u8>) {
    let name = "Solstone-win-Portable.zip";
    fs::write(prepared.candidate.join(name), bytes).expect("replace portable");
    refresh_manifest_artifacts(prepared, &[name]);
}

fn refresh_manifest_artifacts(prepared: &PreparedProof, names: &[&str]) {
    rewrite_manifest_and_receipt(prepared, |evidence| {
        for name in names {
            let bytes = fs::read(prepared.candidate.join(name)).expect("read changed artifact");
            let artifact = evidence
                .artifacts
                .iter_mut()
                .find(|artifact| artifact.path == *name)
                .expect("manifested changed artifact");
            artifact.sha256 = hex_sha256(&bytes);
            artifact.bytes = u64::try_from(bytes.len()).expect("artifact length");
        }
    });
}

fn rewrite_manifest_and_receipt(
    prepared: &PreparedProof,
    mutate: impl FnOnce(&mut ReleaseEvidence),
) {
    let manifest_path = prepared.candidate.join(companion_basename());
    let manifest = validate_manifest_bytes(&fs::read(&manifest_path).expect("read manifest"))
        .expect("parse manifest");
    let mut evidence = ReleaseEvidence::from(manifest);
    mutate(&mut evidence);
    let bytes = render_release_evidence(&evidence).expect("render changed manifest");
    fs::write(&manifest_path, &bytes).expect("write changed manifest");
    mutate_finalization_receipt(prepared, |receipt| {
        receipt.companion_manifest.sha256 = hex_sha256(&bytes);
    });
}

fn hex_sha1(bytes: &[u8]) -> String {
    format!("{:x}", Sha1::digest(bytes))
}

fn hex_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Default)]
struct FailingClock {
    calls: AtomicUsize,
}

impl Clock for FailingClock {
    fn now(&self) -> Result<UtcTimestamp, ClockError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Err(ClockError::OutOfRange)
    }
}
