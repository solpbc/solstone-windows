// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

mod support;

use std::collections::BTreeMap;
use std::fs::{self, FileTimes};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use sha2::{Digest, Sha256};
use support::{
    checkout_facts, request, selection_record, FakeReleaseCheckout, FakeReleaseRunner,
    RunnerMutation, WitnessEvent, ADVISORY_MIRROR_LOCATOR, CHECKED_AT, COMMIT, MINISIGN,
    SIGNED_APP_BYTES, SIGNTOOL, SMCTL, UNSIGNED_APP_BYTES, VERSION, VPK,
};
use xtask::artifact_fs::{walk_directory, UnixModePolicy};
use xtask::release_advisory::{AdvisoryError, MIRROR_COHORT_ID};
use xtask::release_clock::FixedClock;
use xtask::release_container::{ContainerKind, ReleaseContainerError};
use xtask::release_finalizer::{
    finalize, ExecutableReadSource, FinalizeError, FinalizeRequest, PHASE_1_REQUEST_SOURCE,
    PHASE_2_CLEANUP, PHASE_3_ADVISORY_PREFLIGHT, PHASE_4_BUILD, PHASE_5_VELOPACK,
    PHASE_6_BASELINE_CANDIDATE, PHASE_7_EVIDENCE, PHASE_8_PROMOTION,
};
use xtask::release_receipt::{
    render_windows_native_proof_receipt, FinalizationReceipt, WindowsNativeProofReceipt,
    FINALIZATION_RECEIPT_SCHEMA_V2, WINDOWS_NATIVE_PROOF_SCHEMA,
};
use xtask::release_selection::{ReleaseToolSelection, SelectionMode};
use xtask::release_signing::{SigningError, SigningGrammarStage};
use xtask::release_source_binding::{LockFile, SourceBindingError};
use xtask::rust_release_manifest::{
    companion_basename, expected_artifact_names, validate_manifest_bytes,
    validate_release_dir_with_facts, PackagedExecutableEvidence, TARGET_TRIPLE,
};

#[test]
fn finalize_cli_accepts_the_entrypoint_selected_absolute_git_path() {
    let checkout = FakeReleaseCheckout::new("cli-git", false);
    let git = checkout.root().join("fake-git");
    fs::write(&git, b"inert absolute Git selection").expect("write fake Git selection");
    let tools = checkout.root().join("fake-cli-tools");
    fs::create_dir(&tools).expect("create fake CLI tools");
    fs::write(tools.join("minisign"), b"inert minisign selection")
        .expect("write fake minisign selection");
    let mut child = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "rust-release-manifest",
            "finalize",
            "--expected-release-commit",
            COMMIT,
        ])
        .env("GIT", &git)
        .env("PATH", &tools)
        .env("SOLSTONE_ADVISORY_TREE_SHA256", "a".repeat(64))
        .env("SOLSTONE_ADVISORY_MIRROR_LOCATOR", ADVISORY_MIRROR_LOCATOR)
        .env("SOLSTONE_ADVISORY_RECEIPT", checkout.freshness_receipt())
        .env("SOLSTONE_ADVISORY_MIRROR_PUB", checkout.mirror_public_key())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn real finalize CLI");
    child
        .stdin
        .take()
        .expect("open finalize stdin")
        .write_all(b"{}")
        .expect("write synthetic selection");
    let output = child.wait_with_output().expect("wait for finalize CLI");
    let stderr = String::from_utf8(output.stderr).expect("CLI stderr is UTF-8");
    assert!(!output.status.success());
    assert!(stderr.contains("release-tool selection record is invalid"));
    assert!(!stderr.contains("GIT is not"));
}

#[test]
fn unsigned_happy_path_promotes_exact_bundle_without_signer() {
    let checkout = FakeReleaseCheckout::new("unsigned", false);
    let runner = FakeReleaseRunner::new(&checkout, false);
    let clock = FixedClock::new(CHECKED_AT).expect("create fixed clock");
    let result = finalize(
        checkout.runtime(None),
        &request(SelectionMode::Unsigned, false),
        &runner,
        &clock,
    )
    .expect("finalize unsigned release");

    assert_eq!(result.signing_mode, "unsigned");
    assert_promoted_bundle(&checkout, false, "unsigned");
    let events = runner.events();
    assert_witness_order(&events);
    assert_child_process_path_witnesses(&events);
    assert!(events.iter().all(|event| match event {
        WitnessEvent::Invocation { program, args, .. } => {
            program != Path::new(SMCTL)
                && program != Path::new(SIGNTOOL)
                && !args.iter().any(|arg| arg == "--signTemplate")
                && !args
                    .iter()
                    .any(|arg| arg == "packaging/signing/preflight-auth.ps1")
        }
        WitnessEvent::Phase(_) => true,
    }));
}

#[test]
fn signed_happy_path_keeps_stage_unsigned_and_uses_signed_container_baseline() {
    let checkout = FakeReleaseCheckout::new("signed", false);
    let runner = FakeReleaseRunner::new(&checkout, false);
    let clock = FixedClock::new(CHECKED_AT).expect("create fixed clock");
    let result = finalize(
        checkout.runtime(Some("test-keypair")),
        &request(SelectionMode::Signed, false),
        &runner,
        &clock,
    )
    .expect("finalize signed release");

    assert_eq!(result.signing_mode, "signed-verified");
    let manifest = assert_promoted_bundle(&checkout, false, "signed-verified");
    assert_eq!(
        manifest.packaged_executable.sha256,
        hex_sha256(SIGNED_APP_BYTES)
    );
    assert_eq!(
        fs::read(checkout.root().join(format!(
            "target/release-finalizer/{VERSION}/vpk-stage/solstone-windows-app.exe"
        )))
        .expect("read transaction-bound staged executable"),
        UNSIGNED_APP_BYTES
    );
    let events = runner.events();
    assert_witness_order(&events);
    let signing_environment = ReleaseToolSelection::parse(&selection_record(SelectionMode::Signed))
        .expect("parse selected signing tools")
        .signing_child_env_overlay()
        .expect("construct signing child environment");
    let signing_environment_witnesses: Vec<&BTreeMap<String, String>> = events
        .iter()
        .filter_map(|event| match event {
            WitnessEvent::Invocation { program, args, env }
                if program == Path::new(VPK)
                    || args
                        .iter()
                        .any(|arg| arg == "packaging/signing/preflight-auth.ps1") =>
            {
                env.as_ref()
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        signing_environment_witnesses,
        vec![&signing_environment, &signing_environment]
    );
    assert!(events.iter().any(|event| matches!(
        event,
        WitnessEvent::Invocation { program, .. } if program == Path::new(SIGNTOOL)
    )));
    assert!(events.iter().any(|event| match event {
        WitnessEvent::Invocation { program, args, .. } if program == Path::new(VPK) => args
            .windows(2)
            .any(|pair| pair[0] == "--signTemplate" && pair[1].contains(SMCTL)),
        _ => false,
    }));
}

#[test]
fn explicit_delta_base_promotes_the_exact_eight_file_bundle() {
    let checkout = FakeReleaseCheckout::new("delta", true);
    let runner = FakeReleaseRunner::new(&checkout, false);
    let clock = FixedClock::new(CHECKED_AT).expect("create fixed clock");
    finalize(
        checkout.runtime(None),
        &request(SelectionMode::Unsigned, true),
        &runner,
        &clock,
    )
    .expect("finalize delta release");

    assert_promoted_bundle(&checkout, true, "unsigned");
    assert!(checkout
        .root()
        .join("Releases/Solstone-0.2.10-full.nupkg")
        .is_file());
}

#[test]
fn fixed_inputs_are_deterministic_across_roots_temp_names_and_iteration_order() {
    let first = FakeReleaseCheckout::new("determinism-a", false);
    let second = FakeReleaseCheckout::new("determinism-b", false);
    let first_runner = FakeReleaseRunner::new(&first, false);
    let second_runner = FakeReleaseRunner::new(&second, true);
    let clock = FixedClock::new(CHECKED_AT).expect("create shared fixed clock");
    let request = request(SelectionMode::Unsigned, false);

    finalize(first.runtime(None), &request, &first_runner, &clock)
        .expect("finalize first deterministic root");
    finalize(second.runtime(None), &request, &second_runner, &clock)
        .expect("finalize second deterministic root");

    let relative = format!(
        "target/release-candidate/{VERSION}/{}",
        companion_basename()
    );
    assert_eq!(
        fs::read(first.root().join(&relative)).expect("read first manifest"),
        fs::read(second.root().join(&relative)).expect("read second manifest")
    );
    assert_eq!(clock.calls(), 6);
}

#[test]
fn same_version_refinalization_replaces_receipt_only_after_candidate_promotion() {
    let checkout = FakeReleaseCheckout::new("same-version-replacement", false);
    let request = request(SelectionMode::Unsigned, false);
    let first_runner = FakeReleaseRunner::new(&checkout, false);
    finalize(
        checkout.runtime(None),
        &request,
        &first_runner,
        &FixedClock::new(CHECKED_AT).expect("create first clock"),
    )
    .expect("create valid prior candidate and receipt");
    let receipt_path = checkout.root().join(format!(
        "target/release-evidence/{VERSION}/rust-release-finalization.json"
    ));
    let prior_receipt = fs::read(&receipt_path).expect("read prior receipt");

    let second_runner = FakeReleaseRunner::new(&checkout, true);
    finalize(
        checkout.runtime(None),
        &request,
        &second_runner,
        &FixedClock::new("2026-07-21T12:30:00Z").expect("create second clock"),
    )
    .expect("replace same-version candidate and receipt");

    assert_promoted_bundle(&checkout, false, "unsigned");
    let replacement_receipt = fs::read(&receipt_path).expect("read replacement receipt");
    assert_ne!(replacement_receipt, prior_receipt);
    let parsed: FinalizationReceipt =
        serde_json::from_slice(&replacement_receipt).expect("parse replacement receipt");
    assert_eq!(parsed.advisory_checked_at, "2026-07-21T12:30:00Z");
}

#[test]
fn existing_native_proof_refuses_refinalization_before_deleting_prior_evidence() {
    let checkout = FakeReleaseCheckout::new("proof-preserves-prior", false);
    let request = request(SelectionMode::Signed, false);
    let runner = FakeReleaseRunner::new(&checkout, false);
    finalize(
        checkout.runtime(Some("test-keypair")),
        &request,
        &runner,
        &FixedClock::new(CHECKED_AT).expect("create first clock"),
    )
    .expect("create valid prior signed candidate and receipt");
    let candidate = checkout
        .root()
        .join(format!("target/release-candidate/{VERSION}"));
    let manifest_bytes = fs::read(candidate.join(companion_basename())).expect("read manifest");
    let manifest = validate_manifest_bytes(&manifest_bytes).expect("parse manifest");
    let setup_sha256 = hex_sha256(
        &fs::read(candidate.join(format!("solstone-setup-{VERSION}.exe"))).expect("read setup"),
    );
    let proof = WindowsNativeProofReceipt {
        schema: WINDOWS_NATIVE_PROOF_SCHEMA.to_owned(),
        product: manifest.product.clone(),
        version: manifest.version.clone(),
        target: TARGET_TRIPLE.to_owned(),
        source_commit: manifest.source_commit.clone(),
        companion_manifest: xtask::release_receipt::CompanionManifestReceipt {
            filename: companion_basename(),
            sha256: hex_sha256(&manifest_bytes),
        },
        setup_sha256,
        packaged_executable_sha256: manifest.packaged_executable.sha256.clone(),
        installed_executable_sha256: manifest.packaged_executable.sha256,
        install_mode: "isolated-clean".to_owned(),
        installer_success: true,
        smoke_success: true,
        proved_at: "2026-07-21T13:00:00Z".to_owned(),
    };
    let proof_path = checkout.root().join(format!(
        "target/release-evidence/{VERSION}/windows-native-proof.json"
    ));
    fs::write(
        &proof_path,
        render_windows_native_proof_receipt(&proof).expect("render proof"),
    )
    .expect("write prior proof");
    let before = snapshot_regular_files(checkout.root());

    let second_runner = FakeReleaseRunner::new(&checkout, false);
    let error = finalize(
        checkout.runtime(Some("test-keypair")),
        &request,
        &second_runner,
        &FixedClock::new(CHECKED_AT).expect("create second clock"),
    )
    .expect_err("native proof must refuse same-version refinalization");
    assert_eq!(error, FinalizeError::Cleanup);
    assert_eq!(
        second_runner
            .events()
            .iter()
            .filter_map(|event| match event {
                WitnessEvent::Phase(phase) => Some(phase.as_str()),
                WitnessEvent::Invocation { .. } => None,
            })
            .collect::<Vec<_>>(),
        vec![PHASE_1_REQUEST_SOURCE, PHASE_2_CLEANUP]
    );
    assert_eq!(snapshot_regular_files(checkout.root()), before);
}

// Pure source diagnostics are exhaustively typed in release_source_binding.rs;
// these cases prove the engine never reaches cleanup or a build action.
#[test]
fn source_binding_mutations_abort_phase_one_without_mutation_or_build() {
    let malformed = [
        ("expected-absent", "".to_owned()),
        ("expected-abbreviated", "01234567".to_owned()),
        ("expected-uppercase", COMMIT.to_ascii_uppercase()),
        ("expected-ref-expression", "refs/heads/main".to_owned()),
    ];
    for (label, expected) in malformed {
        let checkout = FakeReleaseCheckout::new(label, false);
        seed_precleanup_canary(&checkout);
        let runner = FakeReleaseRunner::new(&checkout, false);
        let mut request = request(SelectionMode::Unsigned, false);
        request.expected_release_commit = expected;
        assert_engine_failure(
            &checkout,
            &runner,
            request,
            FinalizeError::SourceBinding(SourceBindingError::InvalidExpectedCommit),
            PHASE_1_REQUEST_SOURCE,
            true,
            false,
        );
        assert_precleanup_canary(&checkout);
    }

    let cases = [
        (
            "object-absent",
            RunnerMutation::SourceObjectAbsent,
            SourceBindingError::LocalCommitMissing,
        ),
        (
            "wrong-lineage",
            RunnerMutation::SourceWrongLineage,
            SourceBindingError::WrongLineage,
        ),
        (
            "wrong-head",
            RunnerMutation::SourceWrongHead,
            SourceBindingError::HeadMismatch,
        ),
        (
            "wrong-ref",
            RunnerMutation::SourceWrongRef,
            SourceBindingError::CheckoutRefRejected,
        ),
        (
            "detached",
            RunnerMutation::SourceDetached,
            SourceBindingError::DetachedHead,
        ),
        (
            "detached-box-mismatch",
            RunnerMutation::DetachedBoxMismatch,
            SourceBindingError::HeadMismatch,
        ),
        (
            "dirty-tracked",
            RunnerMutation::SourceDirty,
            SourceBindingError::DirtyCheckout,
        ),
        (
            "dirty-untracked",
            RunnerMutation::SourceUntracked,
            SourceBindingError::DirtyCheckout,
        ),
        (
            "dirty-unmerged",
            RunnerMutation::SourceUnmerged,
            SourceBindingError::DirtyCheckout,
        ),
        (
            "dirty-submodule",
            RunnerMutation::SourceSubmodule,
            SourceBindingError::DirtyCheckout,
        ),
        (
            "cargo-lock-untracked",
            RunnerMutation::CargoLockUntracked,
            SourceBindingError::LockNotTracked {
                lock: LockFile::Cargo,
            },
        ),
        (
            "ui-lock-untracked",
            RunnerMutation::UiLockUntracked,
            SourceBindingError::LockNotTracked {
                lock: LockFile::UiPackage,
            },
        ),
    ];
    for (label, mutation, cause) in cases {
        let checkout = FakeReleaseCheckout::new(label, false);
        seed_precleanup_canary(&checkout);
        let runner = FakeReleaseRunner::with_mutation(&checkout, false, mutation);
        assert_engine_failure(
            &checkout,
            &runner,
            request(SelectionMode::Unsigned, false),
            FinalizeError::SourceBinding(cause),
            PHASE_1_REQUEST_SOURCE,
            true,
            false,
        );
        assert_precleanup_canary(&checkout);
    }
}

#[test]
fn source_binding_error_preserves_the_exact_cause_in_its_diagnostic() {
    assert_eq!(
        FinalizeError::SourceBinding(SourceBindingError::CheckoutContainment).to_string(),
        "release source binding failed: release checkout containment could not be established; use one real checkout directory without links or reparse points"
    );
}

// Pure record validation lives in release_selection.rs. Here the malformed
// record is sent through the transaction and must not select any executable.
#[test]
fn selection_mutations_abort_before_any_selected_action() {
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("malformed-selection", b"{".to_vec()),
        (
            "extra-tool",
            mutate_selection(SelectionMode::Unsigned, |value| {
                value["tools"]["unexpected"] = serde_json::json!({"path":format!("{}/extra", support::FAKE_TOOLS_ROOT),"version":"1"});
            }),
        ),
        (
            "missing-tool",
            mutate_selection(SelectionMode::Unsigned, |value| {
                value["tools"]
                    .as_object_mut()
                    .expect("tools object")
                    .remove("node");
            }),
        ),
        (
            "extra-action",
            mutate_selection(SelectionMode::Unsigned, |value| {
                value["actions"]["unexpected"] = serde_json::json!({"program":format!("{}/extra", support::FAKE_TOOLS_ROOT),"argv":[]});
            }),
        ),
        (
            "missing-action",
            mutate_selection(SelectionMode::Unsigned, |value| {
                value["actions"]
                    .as_object_mut()
                    .expect("actions object")
                    .remove("npm_build");
            }),
        ),
        (
            "action-tool-disagreement",
            mutate_selection(SelectionMode::Unsigned, |value| {
                value["actions"]["npm_ci"]["program"] =
                    serde_json::Value::String(format!("{}/not-npm", support::FAKE_TOOLS_ROOT));
            }),
        ),
        (
            "argv-drift",
            mutate_selection(SelectionMode::Unsigned, |value| {
                value["actions"]["npm_ci"]["argv"][3] =
                    serde_json::Value::String("--online".to_owned());
            }),
        ),
        (
            "undocumented-placeholder",
            mutate_selection(SelectionMode::Unsigned, |value| {
                value["actions"]["npm_build"]["argv"]
                    .as_array_mut()
                    .expect("argv array")
                    .push(serde_json::Value::String("{credential}".to_owned()));
            }),
        ),
        (
            "unsigned-signed-action",
            mutate_selection(SelectionMode::Unsigned, |value| {
                value["actions"]["smctl_sign"] = serde_json::json!({
                    "program": support::SMCTL,
                    "argv": ["sign", "--keypair-alias", "{keypair_alias}", "--input", "{file}"]
                });
            }),
        ),
        (
            "poison-path-relative-program",
            mutate_selection(SelectionMode::Unsigned, |value| {
                value["tools"]["cargo"]["path"] = serde_json::Value::String("cargo".to_owned());
                value["actions"]["cargo_release_build"]["program"] =
                    serde_json::Value::String("cargo".to_owned());
                value["actions"]["cargo_deny_advisories"]["program"] =
                    serde_json::Value::String("cargo".to_owned());
            }),
        ),
    ];
    for (label, selection_record) in cases {
        let checkout = FakeReleaseCheckout::new(label, false);
        let runner = FakeReleaseRunner::new(&checkout, false);
        let mut request = request(SelectionMode::Unsigned, false);
        request.selection_record = selection_record;
        assert_engine_failure(
            &checkout,
            &runner,
            request,
            FinalizeError::SelectionInvalid,
            PHASE_1_REQUEST_SOURCE,
            true,
            false,
        );
    }
}

#[test]
fn phase_one_through_three_gates_precede_all_byte_changing_actions() {
    let checkout = FakeReleaseCheckout::new("version-authority", false);
    let runner =
        FakeReleaseRunner::with_mutation(&checkout, false, RunnerMutation::VersionAuthorityFailure);
    assert_engine_failure(
        &checkout,
        &runner,
        request(SelectionMode::Unsigned, false),
        FinalizeError::VersionAuthority,
        PHASE_1_REQUEST_SOURCE,
        true,
        false,
    );

    let checkout = FakeReleaseCheckout::new("advisory-failure", false);
    let runner =
        FakeReleaseRunner::with_mutation(&checkout, false, RunnerMutation::AdvisoryCommandFailure);
    assert_engine_failure(
        &checkout,
        &runner,
        request(SelectionMode::Unsigned, false),
        FinalizeError::Advisory(AdvisoryError::CargoDenyFailed),
        PHASE_3_ADVISORY_PREFLIGHT,
        true,
        false,
    );

    let checkout = FakeReleaseCheckout::new("missing-release-notes", false);
    fs::write(checkout.root().join("CHANGELOG.md"), b"# Changelog\n")
        .expect("remove current release notes");
    let runner = FakeReleaseRunner::new(&checkout, false);
    assert_engine_failure(
        &checkout,
        &runner,
        request(SelectionMode::Unsigned, false),
        FinalizeError::ReleaseNotes,
        PHASE_3_ADVISORY_PREFLIGHT,
        true,
        false,
    );

    let checkout = FakeReleaseCheckout::new("signing-auth", false);
    let runner =
        FakeReleaseRunner::with_mutation(&checkout, false, RunnerMutation::SigningAuthFailure);
    let expected = FinalizeError::ActionFailed {
        action: "signing_auth_preflight",
        output_tail: "signing authentication unavailable".to_owned(),
        output_truncated: false,
    };
    let diagnostic = expected.to_string();
    assert!(diagnostic.contains("signing_auth_preflight"));
    assert!(diagnostic.contains("signing authentication unavailable"));
    assert_engine_failure(
        &checkout,
        &runner,
        request(SelectionMode::Signed, false),
        expected,
        PHASE_3_ADVISORY_PREFLIGHT,
        true,
        false,
    );
}

// release_finalizer_fs.rs exhaustively plants links at every catalog member.
// These representatives prove the engine executes that complete preflight
// before deleting even an unrelated safe member.
#[cfg(unix)]
#[test]
fn cleanup_confinement_mutations_refuse_whole_cleanup_and_preserve_outside_bytes() {
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;

    for position in ["leaf", "root", "ancestor"] {
        let checkout = FakeReleaseCheckout::new(&format!("cleanup-{position}"), false);
        let external = outside_directory(&checkout, position);
        fs::create_dir(&external).expect("create outside directory");
        let sentinel = external.join("sentinel.bin");
        let sentinel_bytes = format!("outside-{position}-sentinel").into_bytes();
        fs::write(&sentinel, &sentinel_bytes).expect("write outside sentinel");
        fs::create_dir_all(checkout.root().join("Releases")).expect("create Releases");
        fs::write(checkout.root().join("Releases/RELEASES"), b"must survive")
            .expect("write safe catalog member");
        match position {
            "leaf" => symlink(&sentinel, checkout.root().join("Releases/assets.win.json"))
                .expect("plant catalog leaf symlink"),
            "root" => {
                fs::create_dir_all(checkout.root().join("target/release-finalizer"))
                    .expect("create catalog parent");
                symlink(
                    &external,
                    checkout
                        .root()
                        .join(format!("target/release-finalizer/{VERSION}")),
                )
                .expect("plant catalog root symlink");
            }
            "ancestor" => {
                fs::create_dir_all(checkout.root().join("target")).expect("create target ancestor");
                symlink(&external, checkout.root().join("target/release-candidate"))
                    .expect("plant intermediate symlink");
            }
            _ => unreachable!(),
        }
        let runner = FakeReleaseRunner::new(&checkout, false);
        assert_engine_failure(
            &checkout,
            &runner,
            request(SelectionMode::Unsigned, false),
            FinalizeError::Cleanup,
            PHASE_2_CLEANUP,
            true,
            false,
        );
        assert_eq!(
            fs::read(checkout.root().join("Releases/RELEASES"))
                .expect("safe catalog member remains"),
            b"must survive"
        );
        assert_eq!(fs::read(&sentinel).expect("read sentinel"), sentinel_bytes);
        fs::remove_dir_all(&external).expect("remove outside fixture");
    }

    let checkout = FakeReleaseCheckout::new("cleanup-case-collision", false);
    let candidate_parent = checkout.root().join("target/release-candidate");
    fs::create_dir_all(&candidate_parent).expect("create candidate parent");
    for hex in [
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    ] {
        fs::create_dir(candidate_parent.join(format!(".{VERSION}.finalize-{hex}.tmp")))
            .expect("create colliding candidate temp");
    }
    let runner = FakeReleaseRunner::new(&checkout, false);
    assert_engine_failure(
        &checkout,
        &runner,
        request(SelectionMode::Unsigned, false),
        FinalizeError::Cleanup,
        PHASE_2_CLEANUP,
        true,
        false,
    );
    assert_eq!(
        fs::read_dir(candidate_parent).expect("read temps").count(),
        2
    );

    let checkout = FakeReleaseCheckout::new("cleanup-special-file", false);
    fs::create_dir(checkout.root().join("Releases")).expect("create Releases");
    let socket = checkout.root().join("Releases/assets.win.json");
    let listener = UnixListener::bind(&socket).expect("plant special file");
    fs::write(checkout.root().join("Releases/RELEASES"), b"must survive")
        .expect("write safe member");
    let runner = FakeReleaseRunner::new(&checkout, false);
    assert_engine_failure(
        &checkout,
        &runner,
        request(SelectionMode::Unsigned, false),
        FinalizeError::Cleanup,
        PHASE_2_CLEANUP,
        true,
        false,
    );
    assert!(socket.exists());
    assert!(checkout.root().join("Releases/RELEASES").exists());
    drop(listener);

    let checkout = FakeReleaseCheckout::new("cleanup-unknown", false);
    fs::create_dir(checkout.root().join("Releases")).expect("create Releases");
    fs::write(
        checkout.root().join("Releases/private-notes.txt"),
        b"unknown",
    )
    .expect("write unknown entry");
    fs::write(checkout.root().join("Releases/RELEASES"), b"must survive")
        .expect("write safe member");
    let runner = FakeReleaseRunner::new(&checkout, false);
    assert_engine_failure(
        &checkout,
        &runner,
        request(SelectionMode::Unsigned, false),
        FinalizeError::Cleanup,
        PHASE_2_CLEANUP,
        true,
        false,
    );
    assert!(checkout.root().join("Releases/private-notes.txt").exists());
    assert!(checkout.root().join("Releases/RELEASES").exists());

    let checkout = FakeReleaseCheckout::new("cleanup-non-allowlisted-history", false);
    let external = outside_directory(&checkout, "historical");
    fs::create_dir(&external).expect("create historical outside directory");
    let sentinel = external.join("historical-full.nupkg");
    fs::write(&sentinel, b"historical outside sentinel").expect("write historical sentinel");
    fs::create_dir(checkout.root().join("Releases")).expect("create Releases");
    symlink(
        &sentinel,
        checkout.root().join("Releases/Solstone-0.2.9-full.nupkg"),
    )
    .expect("plant non-allowlisted historical link");
    fs::write(checkout.root().join("Releases/RELEASES"), b"must survive")
        .expect("write safe member");
    let runner = FakeReleaseRunner::new(&checkout, false);
    assert_engine_failure(
        &checkout,
        &runner,
        request(SelectionMode::Unsigned, false),
        FinalizeError::Cleanup,
        PHASE_2_CLEANUP,
        true,
        false,
    );
    assert_eq!(
        fs::read(&sentinel).expect("read historical sentinel"),
        b"historical outside sentinel"
    );
    assert!(checkout.root().join("Releases/RELEASES").exists());
    fs::remove_dir_all(external).expect("remove historical outside fixture");
    // Live junction/reparse attributes remain a post-ship Windows exercise; the
    // shared verifier's Windows attribute seam is unit-tested in artifact_fs.
}

#[cfg(unix)]
#[test]
fn unsafe_preexisting_candidate_and_temp_are_not_treated_as_resumable_bytes() {
    use std::os::unix::fs::symlink;

    for kind in ["candidate", "candidate-temp"] {
        let checkout = FakeReleaseCheckout::new(kind, false);
        let external = outside_directory(&checkout, kind);
        fs::create_dir(&external).expect("create outside directory");
        let sentinel = external.join("sentinel");
        fs::write(&sentinel, b"outside remains").expect("write sentinel");
        let root = if kind == "candidate" {
            checkout
                .root()
                .join(format!("target/release-candidate/{VERSION}"))
        } else {
            checkout.root().join(format!(
                "target/release-candidate/.{VERSION}.finalize-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.tmp"
            ))
        };
        fs::create_dir_all(&root).expect("create pre-existing output");
        symlink(&sentinel, root.join("escaped-member")).expect("plant escaped member");
        let runner = FakeReleaseRunner::new(&checkout, false);
        assert_engine_failure(
            &checkout,
            &runner,
            request(SelectionMode::Unsigned, false),
            FinalizeError::Cleanup,
            PHASE_2_CLEANUP,
            true,
            false,
        );
        assert!(root.exists());
        assert_eq!(
            fs::read(&sentinel).expect("read sentinel"),
            b"outside remains"
        );
        fs::remove_dir_all(&external).expect("remove outside fixture");
    }
}

#[test]
fn proof_receipt_and_atomic_output_races_block_same_version_success() {
    let checkout = FakeReleaseCheckout::new("existing-native-proof", false);
    let evidence = checkout
        .root()
        .join(format!("target/release-evidence/{VERSION}"));
    fs::create_dir_all(&evidence).expect("create evidence directory");
    fs::write(
        evidence.join("windows-native-proof.json"),
        b"already proved",
    )
    .expect("write native proof");
    let runner = FakeReleaseRunner::new(&checkout, false);
    assert_engine_failure(
        &checkout,
        &runner,
        request(SelectionMode::Unsigned, false),
        FinalizeError::Cleanup,
        PHASE_2_CLEANUP,
        true,
        true,
    );
    assert!(evidence.join("windows-native-proof.json").exists());

    let checkout = FakeReleaseCheckout::new("partial-final-receipt", false);
    let receipt = checkout.root().join(format!(
        "target/release-evidence/{VERSION}/rust-release-finalization.json"
    ));
    fs::create_dir_all(receipt.parent().expect("receipt parent"))
        .expect("create evidence directory");
    fs::write(&receipt, b"partial receipt").expect("write partial receipt");
    let runner = FakeReleaseRunner::new(&checkout, false);
    assert_engine_failure(
        &checkout,
        &runner,
        request(SelectionMode::Unsigned, false),
        FinalizeError::ReceiptStaging,
        PHASE_7_EVIDENCE,
        false,
        true,
    );
    assert_eq!(
        fs::read(receipt).expect("read partial receipt"),
        b"partial receipt"
    );

    for (label, mutation, expected) in [
        (
            "promotion-race",
            RunnerMutation::PromotionTargetRace,
            FinalizeError::CandidatePromotion,
        ),
        (
            "receipt-race",
            RunnerMutation::ReceiptTargetRace,
            FinalizeError::ReceiptPromotion,
        ),
    ] {
        let checkout = FakeReleaseCheckout::new(label, false);
        let runner = FakeReleaseRunner::with_mutation(&checkout, false, mutation);
        assert_engine_failure(
            &checkout,
            &runner,
            request(SelectionMode::Unsigned, false),
            expected,
            PHASE_8_PROMOTION,
            false,
            mutation == RunnerMutation::ReceiptTargetRace,
        );
    }
}

#[test]
fn velopack_inventory_assets_and_ledger_mutations_never_reach_candidate_evidence() {
    // Ledger parsing itself is unit-tested with fixtures in rust_release_manifest.rs;
    // these mutations prove the engine reconciles before candidate assembly.
    let cases = [
        ("vpk-missing", RunnerMutation::VpkMissingOutput, false),
        ("vpk-extra", RunnerMutation::VpkExtraOutput, false),
        (
            "vpk-default-setup-survivor",
            RunnerMutation::VpkDefaultSetupConflict,
            false,
        ),
        (
            "assets-installer-missing",
            RunnerMutation::AssetsMissingInstaller,
            false,
        ),
        ("assets-malformed", RunnerMutation::AssetsMalformed, false),
        (
            "assets-installer-duplicate",
            RunnerMutation::AssetsDuplicateInstaller,
            false,
        ),
        (
            "assets-installer-changed",
            RunnerMutation::AssetsChangedInstaller,
            false,
        ),
        ("delta-feed-missing", RunnerMutation::DeltaFeedMissing, true),
        (
            "delta-assets-missing",
            RunnerMutation::DeltaAssetsMissing,
            true,
        ),
        (
            "delta-package-missing",
            RunnerMutation::DeltaPackageMissing,
            true,
        ),
        (
            "releases-full-missing",
            RunnerMutation::ReleasesFullMissing,
            false,
        ),
    ];
    for (label, mutation, delta) in cases {
        let checkout = FakeReleaseCheckout::new(label, delta);
        let runner = FakeReleaseRunner::with_mutation(&checkout, false, mutation);
        let error = run_engine_failure(&checkout, &runner, request(SelectionMode::Unsigned, delta));
        assert!(
            matches!(
                error,
                FinalizeError::VelopackInventory
                    | FinalizeError::AssetsReconciliation
                    | FinalizeError::LedgerReconciliation
            ),
            "{label}: unexpected error {error:?}"
        );
        assert_failure_contract(&checkout, &runner, &error, PHASE_5_VELOPACK, false, false);
    }
}

#[test]
fn archive_and_cross_container_mutations_fail_before_manifest_render() {
    // The precise ZIP-name and comparator errors live in
    // release_container_baseline.rs. The engine must stop in Phase 6.
    for (label, mutation) in [
        ("baseline-nupkg", RunnerMutation::NupkgExecutableDiverges),
        (
            "baseline-portable",
            RunnerMutation::PortableExecutableDiverges,
        ),
        ("nupkg-member-missing", RunnerMutation::NupkgMemberMissing),
        (
            "nupkg-member-case-collision",
            RunnerMutation::NupkgMemberCaseCollision,
        ),
        (
            "nupkg-stable-read",
            RunnerMutation::Phase6ContainerReadFailure,
        ),
    ] {
        let checkout = FakeReleaseCheckout::new(label, false);
        let runner = FakeReleaseRunner::with_mutation(&checkout, false, mutation);
        let error = run_engine_failure(&checkout, &runner, request(SelectionMode::Unsigned, false));
        let diagnostic = error.to_string();
        match (mutation, &error) {
            (
                RunnerMutation::NupkgExecutableDiverges
                | RunnerMutation::PortableExecutableDiverges,
                FinalizeError::ExecutableDivergence {
                    nupkg,
                    portable,
                    pre_pack: Some(pre_pack),
                },
            ) => {
                let foreign_bytes = if mutation == RunnerMutation::NupkgExecutableDiverges {
                    b"divergent nupkg executable".as_slice()
                } else {
                    b"divergent portable executable".as_slice()
                };
                assert!(diagnostic.contains(&nupkg.sha256));
                assert!(diagnostic.contains(&portable.sha256));
                assert!(diagnostic.contains(&hex_sha256(foreign_bytes)));
                assert!(diagnostic.contains(&hex_sha256(UNSIGNED_APP_BYTES)));
                assert_eq!(pre_pack.sha256, hex_sha256(UNSIGNED_APP_BYTES));
                assert!(diagnostic.contains("not an equality term"));
            }
            (
                RunnerMutation::NupkgMemberMissing,
                FinalizeError::ExecutableContainer(ReleaseContainerError::MissingCanonicalMember {
                    container: ContainerKind::Nupkg,
                }),
            ) => {}
            (
                RunnerMutation::NupkgMemberCaseCollision,
                FinalizeError::ExecutableContainer(ReleaseContainerError::EntryCaseCollision {
                    container: ContainerKind::Nupkg,
                }),
            ) => {}
            (
                RunnerMutation::Phase6ContainerReadFailure,
                FinalizeError::ExecutableRead(ExecutableReadSource::FullNupkg),
            ) => {}
            _ => panic!("{label}: unexpected error {error:?}"),
        }
        assert_failure_contract(
            &checkout,
            &runner,
            &error,
            PHASE_6_BASELINE_CANDIDATE,
            false,
            false,
        );
    }
}

#[test]
fn divergence_diagnostic_labels_optional_pre_pack_evidence_as_diagnostic_only() {
    let nupkg = PackagedExecutableEvidence {
        sha256: "a".repeat(64),
        bytes: 11,
    };
    let portable = PackagedExecutableEvidence {
        sha256: "b".repeat(64),
        bytes: 12,
    };
    let pre_pack = PackagedExecutableEvidence {
        sha256: "c".repeat(64),
        bytes: 13,
    };
    let available = FinalizeError::ExecutableDivergence {
        nupkg: nupkg.clone(),
        portable: portable.clone(),
        pre_pack: Some(pre_pack),
    }
    .to_string();
    assert!(available.contains("pre-pack diagnostic sha256="));
    assert!(available.contains("not an equality term"));

    let unavailable = FinalizeError::ExecutableDivergence {
        nupkg,
        portable,
        pre_pack: None,
    }
    .to_string();
    assert!(unavailable.contains("pre-pack diagnostic unavailable, not an equality term"));
    assert!(unavailable.ends_with("rebuild both containers in this transaction and retry"));
}

#[test]
fn a_historical_package_byte_cannot_leak_into_the_current_candidate() {
    let checkout = FakeReleaseCheckout::new("candidate-historical-leak", false);
    let runner =
        FakeReleaseRunner::with_mutation(&checkout, false, RunnerMutation::HistoricalCandidateLeak);
    assert_engine_failure(
        &checkout,
        &runner,
        request(SelectionMode::Unsigned, false),
        FinalizeError::ManifestValidation,
        PHASE_7_EVIDENCE,
        false,
        false,
    );

    let checkout = FakeReleaseCheckout::new("phase-eight-artifact-mutation", false);
    let runner =
        FakeReleaseRunner::with_mutation(&checkout, false, RunnerMutation::Phase8ArtifactMutation);
    assert_engine_failure(
        &checkout,
        &runner,
        request(SelectionMode::Unsigned, false),
        FinalizeError::ManifestValidation,
        PHASE_8_PROMOTION,
        false,
        false,
    );
}

#[test]
fn advisory_snapshot_mutations_never_earn_checked_at_or_start_a_build() {
    // At engine level, each rejected packet/snapshot must preserve its exact
    // AdvisoryError and leave fewer than the three clock observations required
    // to earn advisory_checked_at.
    for (label, mutation, expected) in [
        (
            "advisory-mirror-wrong-key",
            RunnerMutation::AdvisoryMirrorWrongKey,
            AdvisoryError::MirrorPublicKeyPinMismatch,
        ),
        (
            "advisory-mirror-signature",
            RunnerMutation::AdvisoryMirrorSignatureFailure,
            AdvisoryError::FreshnessSignatureInvalid,
        ),
        (
            "advisory-mirror-comment",
            RunnerMutation::AdvisoryMirrorMalformedComment,
            AdvisoryError::FreshnessTrustedCommentFields,
        ),
        (
            "advisory-mirror-body",
            RunnerMutation::AdvisoryMirrorBodyMismatch,
            AdvisoryError::FreshnessBodyMismatch,
        ),
        (
            "advisory-mirror-future",
            RunnerMutation::AdvisoryMirrorFuture,
            AdvisoryError::FreshnessUtcFuture,
        ),
        (
            "advisory-mirror-stale",
            RunnerMutation::AdvisoryMirrorStale,
            AdvisoryError::FreshnessStale,
        ),
        (
            "advisory-mirror-commit",
            RunnerMutation::AdvisoryMirrorCommitMismatch,
            AdvisoryError::FreshnessCommitMismatch,
        ),
        (
            "advisory-dirty",
            RunnerMutation::AdvisoryDirty,
            AdvisoryError::SnapshotDirty,
        ),
        (
            "advisory-source",
            RunnerMutation::AdvisorySourceMismatch,
            AdvisoryError::SourceMismatch,
        ),
        (
            "advisory-shallow",
            RunnerMutation::AdvisoryShallow,
            AdvisoryError::ShallowRepository,
        ),
        (
            "advisory-swapped-archive",
            RunnerMutation::AdvisoryArchiveMismatch,
            AdvisoryError::ArchiveDigestMismatch,
        ),
        (
            "advisory-deny-failure",
            RunnerMutation::AdvisoryCommandFailure,
            AdvisoryError::CargoDenyFailed,
        ),
    ] {
        let checkout = FakeReleaseCheckout::new(label, false);
        let runner = FakeReleaseRunner::with_mutation(&checkout, false, mutation);
        let clock = FixedClock::new(CHECKED_AT).expect("create fixed clock");
        let error = finalize(
            checkout.runtime(None),
            &request(SelectionMode::Unsigned, false),
            &runner,
            &clock,
        )
        .expect_err("mutated advisory snapshot must fail");
        assert_eq!(error, FinalizeError::Advisory(expected), "{label}");
        assert!(clock.calls() < 3, "{label}: checked_at was not earned");
        assert_failure_contract(
            &checkout,
            &runner,
            &error,
            PHASE_3_ADVISORY_PREFLIGHT,
            true,
            false,
        );
        if mutation == RunnerMutation::AdvisoryMirrorWrongKey {
            assert!(runner.events().iter().all(|event| !matches!(
                event,
                WitnessEvent::Invocation { program, .. } if program == Path::new(MINISIGN)
            )));
        }
        if mutation == RunnerMutation::AdvisoryMirrorSignatureFailure {
            assert!(runner.events().iter().all(|event| match event {
                WitnessEvent::Invocation { program, args, .. }
                    if program == Path::new(support::GIT) =>
                {
                    !args.iter().any(|arg| {
                        matches!(
                            arg.as_str(),
                            "origin" | "HEAD^{commit}" | "--is-shallow-repository"
                        )
                    })
                }
                WitnessEvent::Invocation { program, args, .. }
                    if program == Path::new(support::CARGO) =>
                {
                    args.first().map(String::as_str) != Some("deny")
                }
                _ => true,
            }));
        }
    }

    let checkout = FakeReleaseCheckout::new("advisory-db-missing", false);
    fs::remove_dir_all(checkout.root().join("target/release-advisory-db"))
        .expect("remove isolated advisory root");
    let runner = FakeReleaseRunner::new(&checkout, false);
    assert_engine_failure(
        &checkout,
        &runner,
        request(SelectionMode::Unsigned, false),
        FinalizeError::Advisory(AdvisoryError::DatabaseRootInvalid),
        PHASE_3_ADVISORY_PREFLIGHT,
        true,
        false,
    );

    let checkout = FakeReleaseCheckout::new("advisory-db-multiple", false);
    fs::create_dir(
        checkout
            .root()
            .join("target/release-advisory-db/advisory-db-aaaaaaaaaaaaaaaa"),
    )
    .expect("add second advisory repository");
    let runner = FakeReleaseRunner::new(&checkout, false);
    assert_engine_failure(
        &checkout,
        &runner,
        request(SelectionMode::Unsigned, false),
        FinalizeError::Advisory(AdvisoryError::RepositoryCount),
        PHASE_3_ADVISORY_PREFLIGHT,
        true,
        false,
    );

    let checkout = FakeReleaseCheckout::new("advisory-db-swapped", false);
    let repository = checkout.advisory_repository();
    fs::rename(
        repository,
        checkout
            .root()
            .join("target/release-advisory-db/swapped-default-cache"),
    )
    .expect("swap advisory repository");
    let runner = FakeReleaseRunner::new(&checkout, false);
    assert_engine_failure(
        &checkout,
        &runner,
        request(SelectionMode::Unsigned, false),
        FinalizeError::Advisory(AdvisoryError::RepositoryName),
        PHASE_3_ADVISORY_PREFLIGHT,
        true,
        false,
    );

    for (label, offset, expected) in [
        ("advisory-stale", -25_i64, AdvisoryError::SnapshotStale),
        ("advisory-future", 1_i64, AdvisoryError::AcquisitionFuture),
    ] {
        let checkout = FakeReleaseCheckout::new(label, false);
        let now = xtask::release_clock::UtcTimestamp::parse(CHECKED_AT)
            .expect("parse check time")
            .system_time();
        let acquired = if offset < 0 {
            now - Duration::from_secs(offset.unsigned_abs() * 60 * 60)
        } else {
            now + Duration::from_secs(offset.unsigned_abs() * 60 * 60)
        };
        fs::OpenOptions::new()
            .write(true)
            .open(checkout.advisory_repository().join(".git/FETCH_HEAD"))
            .expect("open FETCH_HEAD")
            .set_times(FileTimes::new().set_modified(acquired))
            .expect("set FETCH_HEAD mtime");
        let runner = FakeReleaseRunner::new(&checkout, false);
        assert_engine_failure(
            &checkout,
            &runner,
            request(SelectionMode::Unsigned, false),
            FinalizeError::Advisory(expected),
            PHASE_3_ADVISORY_PREFLIGHT,
            true,
            false,
        );
    }

    // Removing the explicit --offline/config contract or adding a network fetch
    // is rejected by selection before even a local advisory git inspection.
    for (label, replacement) in [
        ("advisory-implicit-db", "--default-db"),
        ("advisory-network-fetch", "fetch"),
    ] {
        let checkout = FakeReleaseCheckout::new(label, false);
        let runner = FakeReleaseRunner::new(&checkout, false);
        let mut request = request(SelectionMode::Unsigned, false);
        request.selection_record = mutate_selection(SelectionMode::Unsigned, |value| {
            value["actions"]["cargo_deny_advisories"]["argv"][2] =
                serde_json::Value::String(replacement.to_owned());
        });
        assert_engine_failure(
            &checkout,
            &runner,
            request,
            FinalizeError::SelectionInvalid,
            PHASE_1_REQUEST_SOURCE,
            true,
            false,
        );
        assert!(runner.events().iter().all(|event| match event {
            WitnessEvent::Invocation { args, .. } => !args
                .iter()
                .any(|arg| { matches!(arg.as_str(), "fetch" | "ls-remote" | "clone" | "pull") }),
            WitnessEvent::Phase(_) => true,
        }));
    }
}

#[test]
fn signing_failures_are_fail_closed_and_unsigned_cannot_select_a_signer() {
    // release_signing::every_signing_grammar_stage_is_reachable_through_real_parser covers every
    // grammar stage; this verifies one typed stage's engine boundary before evidence.
    let checkout = FakeReleaseCheckout::new("signtool-failure", false);
    let runner =
        FakeReleaseRunner::with_mutation(&checkout, false, RunnerMutation::SignToolFailure);
    assert_engine_failure(
        &checkout,
        &runner,
        request(SelectionMode::Signed, false),
        FinalizeError::SigningVerification(SigningError::NonzeroExit),
        PHASE_6_BASELINE_CANDIDATE,
        false,
        false,
    );

    let checkout = FakeReleaseCheckout::new("unsigned-signer-injection", false);
    let runner = FakeReleaseRunner::new(&checkout, false);
    let mut request = request(SelectionMode::Unsigned, false);
    request.selection_record = mutate_selection(SelectionMode::Unsigned, |value| {
        value["actions"]["signtool_verify"] = serde_json::json!({
            "program": support::SIGNTOOL,
            "argv": ["verify", "/pa", "/all", "/v", "{file}"]
        });
    });
    assert_engine_failure(
        &checkout,
        &runner,
        request,
        FinalizeError::SelectionInvalid,
        PHASE_1_REQUEST_SOURCE,
        true,
        false,
    );
    assert!(runner.events().iter().all(|event| !matches!(
        event,
        WitnessEvent::Invocation { program, .. } if program == Path::new(SIGNTOOL)
    )));
}

#[test]
fn signing_grammar_drift_promotes_only_the_typed_certificate_free_stage() {
    let checkout = FakeReleaseCheckout::new("signtool-grammar-drift", false);
    let runner =
        FakeReleaseRunner::with_mutation(&checkout, false, RunnerMutation::SignToolGrammarDrift);
    let error = run_engine_failure(&checkout, &runner, request(SelectionMode::Signed, false));
    assert_eq!(
        error,
        FinalizeError::SigningVerification(SigningError::GrammarDrift {
            stage: SigningGrammarStage::FileHashLine,
        })
    );
    let message = error.to_string();
    assert!(message.contains("final setup signing verification failed"));
    assert!(message.contains("file-hash line"));
    assert!(!message.contains("SYNTHETIC-SIGNTOOL-GRAMMAR-DRIFT"));
    assert!(!message.contains("Issued to:"));
    assert!(!message.contains("SHA1 hash:"));
    assert_failure_contract(
        &checkout,
        &runner,
        &error,
        PHASE_6_BASELINE_CANDIDATE,
        false,
        false,
    );
}

#[test]
fn late_source_lock_and_post_hash_mutations_block_promotion() {
    for (label, mutation, expected) in [
        (
            "late-head",
            RunnerMutation::LateHead,
            FinalizeError::SourceReverification(SourceBindingError::ReverifyHeadDrift),
        ),
        (
            "late-ref",
            RunnerMutation::LateRef,
            FinalizeError::SourceReverification(SourceBindingError::ReverifyRefDrift),
        ),
        (
            "late-status",
            RunnerMutation::LateStatus,
            FinalizeError::SourceReverification(SourceBindingError::ReverifyStatusDrift),
        ),
        (
            "late-cargo-lock",
            RunnerMutation::LateCargoLock,
            FinalizeError::SourceReverification(SourceBindingError::ReverifyLockDrift {
                lock: LockFile::Cargo,
            }),
        ),
        (
            "late-ui-lock",
            RunnerMutation::LateUiLock,
            FinalizeError::SourceReverification(SourceBindingError::ReverifyLockDrift {
                lock: LockFile::UiPackage,
            }),
        ),
        (
            "post-hash-artifact",
            RunnerMutation::PostHashArtifactMutation,
            FinalizeError::ManifestValidation,
        ),
        (
            "post-hash-manifest",
            RunnerMutation::PostHashManifestMutation,
            FinalizeError::ManifestValidation,
        ),
    ] {
        let checkout = FakeReleaseCheckout::new(label, false);
        let runner = FakeReleaseRunner::with_mutation(&checkout, false, mutation);
        assert_engine_failure(
            &checkout,
            &runner,
            request(SelectionMode::Unsigned, false),
            expected,
            PHASE_7_EVIDENCE,
            false,
            false,
        );
    }

    let checkout = FakeReleaseCheckout::new("manifest-delta-entry-missing", true);
    let runner = FakeReleaseRunner::with_mutation(
        &checkout,
        false,
        RunnerMutation::ManifestDeltaEntryMissing,
    );
    assert_engine_failure(
        &checkout,
        &runner,
        request(SelectionMode::Unsigned, true),
        FinalizeError::ManifestValidation,
        PHASE_7_EVIDENCE,
        false,
        false,
    );
}

#[test]
fn stale_build_output_and_missing_offline_npm_cache_cannot_be_reused() {
    let checkout = FakeReleaseCheckout::new("stale-executable", false);
    fs::write(
        checkout
            .root()
            .join("target/release/solstone-windows-app.exe"),
        b"stale executable from another commit",
    )
    .expect("write stale shared-target executable");
    let runner =
        FakeReleaseRunner::with_mutation(&checkout, false, RunnerMutation::CargoBuildNoOutput);
    assert_engine_failure(
        &checkout,
        &runner,
        request(SelectionMode::Unsigned, false),
        FinalizeError::BuildArtifact,
        PHASE_4_BUILD,
        false,
        false,
    );
    assert!(checkout
        .root()
        .join("target/release/solstone-windows-app.exe")
        .exists());

    let checkout = FakeReleaseCheckout::new("offline-npm-cache", false);
    let runner = FakeReleaseRunner::with_mutation(&checkout, false, RunnerMutation::NpmCiFailure);
    let expected = FinalizeError::ActionFailed {
        action: "npm_ci",
        output_tail: "offline npm cache missing".to_owned(),
        output_truncated: false,
    };
    let diagnostic = expected.to_string();
    assert!(diagnostic.contains("npm_ci"));
    assert!(diagnostic.contains("offline npm cache missing"));
    assert_engine_failure(
        &checkout,
        &runner,
        request(SelectionMode::Unsigned, false),
        expected,
        PHASE_4_BUILD,
        false,
        false,
    );
    assert!(runner.events().iter().all(|event| match event {
        WitnessEvent::Invocation { program, args, .. }
            if program == Path::new(support::NPM) && args.iter().any(|arg| arg == "run") =>
        {
            false
        }
        WitnessEvent::Invocation { program, args, .. }
            if (program == Path::new(support::CARGO)
                && args.first().map(String::as_str) == Some("build"))
                || program == Path::new(VPK) =>
        {
            false
        }
        _ => true,
    }));
}

#[test]
fn engine_diagnostics_and_receipts_do_not_leak_private_runtime_data() {
    let checkout = FakeReleaseCheckout::new("private-host-account", false);
    let runner =
        FakeReleaseRunner::with_mutation(&checkout, false, RunnerMutation::SourceObjectAbsent);
    let mut request = request(SelectionMode::Unsigned, false);
    request
        .selection_record
        .extend_from_slice(b"PRIVATE-CREDENTIAL-CANARY");
    let error = finalize(
        checkout.runtime(None),
        &request,
        &runner,
        &FixedClock::new(CHECKED_AT).expect("fixed clock"),
    )
    .expect_err("private canary request must fail");
    let diagnostic = error.to_string();
    assert!(!diagnostic.contains(checkout.root().to_string_lossy().as_ref()));
    assert!(!diagnostic.contains("PRIVATE-CREDENTIAL-CANARY"));
    assert!(!diagnostic.contains("private-host-account"));
    assert!(!diagnostic.contains("BEGIN CERTIFICATE"));
    assert_no_publishable_output(&checkout, false);
}

fn mutate_selection(mode: SelectionMode, mutation: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let mut value: serde_json::Value =
        serde_json::from_slice(&selection_record(mode)).expect("parse fake selection");
    mutation(&mut value);
    serde_json::to_vec(&value).expect("render mutated selection")
}

fn run_engine_failure(
    checkout: &FakeReleaseCheckout,
    runner: &FakeReleaseRunner,
    request: FinalizeRequest,
) -> FinalizeError {
    finalize(
        checkout.runtime(if request.sign_mode == SelectionMode::Signed {
            Some("test-keypair")
        } else {
            None
        }),
        &request,
        runner,
        &FixedClock::new(CHECKED_AT).expect("create fixed clock"),
    )
    .expect_err("engine mutation must fail closed")
}

#[allow(clippy::too_many_arguments)]
fn assert_engine_failure(
    checkout: &FakeReleaseCheckout,
    runner: &FakeReleaseRunner,
    request: FinalizeRequest,
    expected: FinalizeError,
    last_phase: &str,
    before_build: bool,
    allow_existing_receipt: bool,
) {
    let error = run_engine_failure(checkout, runner, request);
    assert_eq!(error, expected);
    assert_failure_contract(
        checkout,
        runner,
        &error,
        last_phase,
        before_build,
        allow_existing_receipt,
    );
}

fn assert_failure_contract(
    checkout: &FakeReleaseCheckout,
    runner: &FakeReleaseRunner,
    error: &FinalizeError,
    last_phase: &str,
    before_build: bool,
    allow_existing_receipt: bool,
) {
    let diagnostic = error.to_string();
    assert!(
        diagnostic.contains(';'),
        "diagnostic must name the failure then give remediation: {diagnostic}"
    );
    assert!(
        [
            "restore",
            "repair",
            "retry",
            "restart",
            "rerun",
            "rebuild",
            "discard",
            "clear",
            "provide",
            "pass",
            "refresh",
            "remediate",
            "provision",
            "reprovision",
            "use ",
            "check out",
        ]
        .iter()
        .any(|word| diagnostic.to_ascii_lowercase().contains(word)),
        "diagnostic lacks concrete remediation: {diagnostic}"
    );

    let all_phases = [
        PHASE_1_REQUEST_SOURCE,
        PHASE_2_CLEANUP,
        PHASE_3_ADVISORY_PREFLIGHT,
        PHASE_4_BUILD,
        PHASE_5_VELOPACK,
        PHASE_6_BASELINE_CANDIDATE,
        PHASE_7_EVIDENCE,
        PHASE_8_PROMOTION,
    ];
    let last = all_phases
        .iter()
        .position(|phase| *phase == last_phase)
        .expect("known expected phase");
    let events = runner.events();
    let observed: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            WitnessEvent::Phase(phase) => Some(phase.as_str()),
            WitnessEvent::Invocation { .. } => None,
        })
        .collect();
    assert_eq!(observed, all_phases[..=last]);

    if before_build {
        assert!(runner.events().iter().all(|event| match event {
            WitnessEvent::Invocation { program, args, .. } => {
                program != Path::new(support::NPM)
                    && program != Path::new(VPK)
                    && program != Path::new(SMCTL)
                    && program != Path::new(SIGNTOOL)
                    && !(program == Path::new(support::CARGO)
                        && args.first().map(String::as_str) == Some("build"))
                    && !args.iter().any(|arg| arg == "--signTemplate")
            }
            WitnessEvent::Phase(_) => true,
        }));
    }

    assert_no_publishable_output(checkout, allow_existing_receipt);
    if last >= 2 {
        assert!(
            !checkout
                .root()
                .join(format!("target/release-finalizer/{VERSION}"))
                .exists(),
            "transaction scratch must be removed after a mutating-phase failure"
        );
        let candidate_parent = checkout.root().join("target/release-candidate");
        if candidate_parent.is_dir() {
            let generated_temps: Vec<_> = fs::read_dir(candidate_parent)
                .expect("read candidate parent")
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry.file_name().to_str().is_some_and(|name| {
                        name.starts_with(&format!(".{VERSION}.finalize-")) && name.ends_with(".tmp")
                    })
                })
                .collect();
            assert!(
                generated_temps.is_empty(),
                "generated candidate temp must be removed"
            );
        }
    }
}

fn assert_no_publishable_output(checkout: &FakeReleaseCheckout, allow_existing_receipt: bool) {
    let candidate_parent = checkout.root().join("target/release-candidate");
    if candidate_parent.exists() {
        assert!(
            !tree_contains_filename(&candidate_parent, &companion_basename()),
            "no companion manifest may survive a failed transaction"
        );
    }
    let receipt = checkout.root().join(format!(
        "target/release-evidence/{VERSION}/rust-release-finalization.json"
    ));
    if receipt.exists() {
        assert!(allow_existing_receipt, "green receipt survived failure");
        let bytes = fs::read(&receipt).expect("read existing receipt");
        assert!(
            serde_json::from_slice::<FinalizationReceipt>(&bytes).is_err(),
            "an allowed pre-existing receipt must not parse as green evidence"
        );
    }
    assert!(!checkout
        .root()
        .join(format!(
            "target/release-evidence/{VERSION}/.rust-release-finalization.json.tmp"
        ))
        .exists());
}

fn tree_contains_filename(root: &Path, filename: &str) -> bool {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return false,
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if entry.file_name() == filename {
            return true;
        }
        if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_dir())
            && tree_contains_filename(&path, filename)
        {
            return true;
        }
    }
    false
}

fn snapshot_regular_files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn visit(root: &Path, directory: &Path, files: &mut BTreeMap<String, Vec<u8>>) {
        let mut entries: Vec<_> = fs::read_dir(directory)
            .expect("read snapshot directory")
            .map(|entry| entry.expect("read snapshot entry"))
            .collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let kind = entry.file_type().expect("read snapshot file type");
            if kind.is_dir() {
                visit(root, &path, files);
            } else if kind.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .expect("snapshot path beneath root")
                    .to_string_lossy()
                    .replace('\\', "/");
                files.insert(relative, fs::read(path).expect("read snapshot file"));
            }
        }
    }

    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

fn seed_precleanup_canary(checkout: &FakeReleaseCheckout) {
    fs::create_dir_all(checkout.root().join("Releases")).expect("create Releases canary root");
    fs::write(checkout.root().join("Releases/RELEASES"), b"not cleaned")
        .expect("write cleanup canary");
}

fn assert_precleanup_canary(checkout: &FakeReleaseCheckout) {
    assert_eq!(
        fs::read(checkout.root().join("Releases/RELEASES")).expect("read cleanup canary"),
        b"not cleaned"
    );
}

fn outside_directory(checkout: &FakeReleaseCheckout, label: &str) -> std::path::PathBuf {
    checkout.root().with_file_name(format!(
        "{}-{label}-outside",
        checkout
            .root()
            .file_name()
            .and_then(|name| name.to_str())
            .expect("checkout basename")
    ))
}

fn assert_promoted_bundle(
    checkout: &FakeReleaseCheckout,
    has_delta: bool,
    signing_mode: &str,
) -> xtask::rust_release_manifest::Manifest {
    let candidate = checkout
        .root()
        .join(format!("target/release-candidate/{VERSION}"));
    let inventory = walk_directory(
        &candidate,
        "promoted candidate",
        UnixModePolicy::AllowExecute,
    )
    .expect("walk promoted candidate");
    let mut expected = expected_artifact_names(VERSION, has_delta);
    expected.insert(companion_basename());
    assert_eq!(inventory.files, expected);
    assert!(inventory.directories.is_empty());
    assert_eq!(inventory.files.len(), if has_delta { 8 } else { 7 });

    let manifest_bytes = fs::read(candidate.join(companion_basename())).expect("read manifest");
    let manifest = validate_manifest_bytes(&manifest_bytes).expect("parse promoted manifest");
    assert_eq!(
        manifest
            .native_tools
            .get("signing_mode")
            .map(String::as_str),
        Some(signing_mode)
    );
    let facts = checkout_facts(checkout);
    validate_release_dir_with_facts(&candidate, &facts).expect("strictly validate promoted bundle");

    let receipt_path = checkout.root().join(format!(
        "target/release-evidence/{VERSION}/rust-release-finalization.json"
    ));
    let receipt: FinalizationReceipt =
        serde_json::from_slice(&fs::read(receipt_path).expect("read finalization receipt"))
            .expect("parse finalization receipt");
    assert_eq!(receipt.schema, FINALIZATION_RECEIPT_SCHEMA_V2);
    assert_eq!(receipt.advisory_database.source_id, MIRROR_COHORT_ID);
    assert_eq!(receipt.signing_mode, signing_mode);
    assert_eq!(receipt.candidate.file_count, if has_delta { 8 } else { 7 });
    assert_eq!(
        receipt.advisory_checked_at,
        manifest.dependency_policy.advisory_checked_at
    );
    assert_eq!(
        receipt.companion_manifest.sha256,
        hex_sha256(&manifest_bytes)
    );
    manifest
}

fn assert_witness_order(events: &[WitnessEvent]) {
    let phases = [
        PHASE_1_REQUEST_SOURCE,
        PHASE_2_CLEANUP,
        PHASE_3_ADVISORY_PREFLIGHT,
        PHASE_4_BUILD,
        PHASE_5_VELOPACK,
        PHASE_6_BASELINE_CANDIDATE,
        PHASE_7_EVIDENCE,
        PHASE_8_PROMOTION,
    ];
    let actual: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            WitnessEvent::Phase(phase) => Some(phase.as_str()),
            WitnessEvent::Invocation { .. } => None,
        })
        .collect();
    assert_eq!(actual, phases);

    let minisign = events
        .iter()
        .position(|event| matches!(
            event,
            WitnessEvent::Invocation { program, args, .. }
                if program == Path::new(MINISIGN) && args.first().map(String::as_str) == Some("-V")
        ))
        .expect("mirror signature verification witness");
    let advisory_origin = events
        .iter()
        .position(|event| matches!(
            event,
            WitnessEvent::Invocation { program, args, .. }
                if program == Path::new(support::GIT)
                    && args.ends_with(&["remote".to_owned(), "get-url".to_owned(), "origin".to_owned()])
        ))
        .expect("advisory origin witness");
    let cargo_deny = events
        .iter()
        .position(|event| {
            matches!(
                event,
                WitnessEvent::Invocation { program, args, .. }
                    if program == Path::new(support::CARGO)
                        && args.first().map(String::as_str) == Some("deny")
            )
        })
        .expect("cargo-deny witness");
    assert!(minisign < advisory_origin && advisory_origin < cargo_deny);

    let action_order: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            WitnessEvent::Invocation { program, args, .. }
                if program == Path::new(support::NPM) =>
            {
                if args.iter().any(|arg| arg == "ci") {
                    Some("npm_ci")
                } else {
                    Some("npm_build")
                }
            }
            WitnessEvent::Invocation { program, args, .. }
                if program == Path::new(support::CARGO)
                    && args.first().map(String::as_str) == Some("build") =>
            {
                Some("cargo_release_build")
            }
            WitnessEvent::Invocation { program, .. } if program == Path::new(VPK) => {
                Some("vpk_pack")
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        action_order,
        ["npm_ci", "npm_build", "cargo_release_build", "vpk_pack"]
    );
}

fn assert_child_process_path_witnesses(events: &[WitnessEvent]) {
    let cargo_target = events
        .iter()
        .find_map(|event| match event {
            WitnessEvent::Invocation {
                program,
                args,
                env: Some(env),
            } if program == Path::new(support::CARGO)
                && args.first().map(String::as_str) == Some("build") =>
            {
                env.get("CARGO_TARGET_DIR")
            }
            _ => None,
        })
        .expect("cargo build records CARGO_TARGET_DIR");
    assert!(!cargo_target.starts_with(r"\\?\"));

    let vpk_args = events
        .iter()
        .find_map(|event| match event {
            WitnessEvent::Invocation { program, args, .. } if program == Path::new(VPK) => {
                Some(args)
            }
            _ => None,
        })
        .expect("record vpk invocation");
    for flag in ["--packDir", "--outputDir", "--releaseNotes"] {
        let value = vpk_args
            .windows(2)
            .find_map(|pair| (pair[0] == flag).then_some(&pair[1]))
            .unwrap_or_else(|| panic!("vpk invocation records {flag}"));
        assert!(!value.starts_with(r"\\?\"), "{flag} leaked {value}");
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
