// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#[allow(dead_code)]
mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};
use support::{
    action_uses_script, checkout_facts, request, FakeReleaseCheckout, FakeReleaseRunner,
    WitnessEvent, CHECKED_AT, COMMIT, FAKE_TOOLS_ROOT, POWERSHELL, SIGNED_APP_BYTES, VERSION,
};
use xtask::native_release_proof::{
    prove_native, NativeProofRuntime, STEP_10_REVALIDATE, STEP_11_RECEIPT, STEP_11_RECEIPT_STAGED,
    STEP_1_CLASSIFY, STEP_2_IDENTITY, STEP_3_TOOLS, STEP_4_CONTAINERS, STEP_5_INSTALL_ROOT,
    STEP_5_ROOT_READY, STEP_6_INSTALL, STEP_7_INSTALLED_IDENTITY, STEP_8_DUMP_STATE, STEP_9_SMOKE,
};
use xtask::release_clock::FixedClock;
use xtask::release_finalizer::finalize;
use xtask::release_receipt::{
    render_windows_native_proof_receipt, WindowsNativeProofReceipt, WINDOWS_NATIVE_PROOF_SCHEMA,
};
use xtask::release_selection::SelectionMode;
use xtask::rust_release_manifest::{companion_basename, validate_manifest_bytes};

const PROVED_AT: &str = "2026-07-21T13:00:00Z";

#[test]
fn signed_candidate_installs_smokes_and_writes_atomic_private_clean_proof() {
    let checkout = FakeReleaseCheckout::new("private-host-account-credential", false);
    let runner = FakeReleaseRunner::new(&checkout, false);
    finalize(
        checkout.runtime(Some("test-keypair")),
        &request(SelectionMode::Signed, false),
        &runner,
        &FixedClock::new(CHECKED_AT).expect("create finalization clock"),
    )
    .expect("finalize signed candidate");

    let candidate = checkout
        .root()
        .join(format!("target/release-candidate/{VERSION}"));
    let before = flat_file_snapshot(&candidate);
    let manifest_filename = companion_basename();
    let manifest_bytes = before
        .get(&manifest_filename)
        .expect("candidate contains companion manifest")
        .clone();
    let manifest_sha256 = hex_sha256(&manifest_bytes);
    let manifest = validate_manifest_bytes(&manifest_bytes).expect("parse signed manifest");
    let facts = checkout_facts(&checkout);
    let proof_clock = FixedClock::new(PROVED_AT).expect("create proof clock");
    let result = prove_native(
        NativeProofRuntime {
            checkout_root: checkout.root(),
            facts: &facts,
            powershell_bootstrap: Path::new(POWERSHELL).as_os_str(),
        },
        &candidate,
        &runner,
        &proof_clock,
    )
    .expect("prove signed candidate");

    assert_eq!(result.version, VERSION);
    assert_eq!(result.manifest_sha256, manifest_sha256);
    assert_eq!(
        result.receipt_relative_path,
        format!("target/release-evidence/{VERSION}/windows-native-proof.json")
    );
    assert_eq!(proof_clock.calls(), 1);
    assert_eq!(flat_file_snapshot(&candidate), before);

    let final_manifest = fs::read(candidate.join(&manifest_filename)).expect("reread manifest");
    assert_eq!(final_manifest, manifest_bytes);
    assert_eq!(hex_sha256(&final_manifest), manifest_sha256);

    let receipt_path = checkout.root().join(&result.receipt_relative_path);
    let receipt_bytes = fs::read(&receipt_path).expect("read native proof receipt");
    let receipt: WindowsNativeProofReceipt =
        serde_json::from_slice(&receipt_bytes).expect("parse native proof receipt");
    assert_eq!(
        render_windows_native_proof_receipt(&receipt).expect("render canonical proof receipt"),
        receipt_bytes
    );
    assert_eq!(receipt.schema, WINDOWS_NATIVE_PROOF_SCHEMA);
    assert_eq!(receipt.version, VERSION);
    assert_eq!(receipt.source_commit, COMMIT);
    assert_eq!(receipt.companion_manifest.filename, manifest_filename);
    assert_eq!(receipt.companion_manifest.sha256, manifest_sha256);
    assert_eq!(
        receipt.setup_sha256,
        hex_sha256(
            before
                .get(&format!("solstone-setup-{VERSION}.exe"))
                .expect("candidate contains setup")
        )
    );
    assert_eq!(
        receipt.packaged_executable_sha256,
        manifest.packaged_executable.sha256
    );
    assert_eq!(
        receipt.installed_executable_sha256,
        hex_sha256(SIGNED_APP_BYTES)
    );
    assert_eq!(receipt.install_mode, "isolated-clean");
    assert!(receipt.installer_success);
    assert!(receipt.smoke_success);
    assert_eq!(receipt.proved_at, PROVED_AT);

    let rendered = String::from_utf8(receipt_bytes).expect("receipt is UTF-8");
    for private in [
        checkout.root().to_string_lossy().as_ref(),
        "private-host-account-credential",
        "test-keypair",
        FAKE_TOOLS_ROOT,
    ] {
        assert!(!rendered.contains(private), "receipt leaked private data");
    }
    assert_witness_order(&runner.events());
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
            let bytes = fs::read(entry.path()).expect("read candidate file");
            (name, bytes)
        })
        .collect()
}

fn assert_witness_order(events: &[WitnessEvent]) {
    let step_1 = phase_index(events, STEP_1_CLASSIFY);
    let step_2 = phase_index(events, STEP_2_IDENTITY);
    let step_3 = phase_index(events, STEP_3_TOOLS);
    let resolver = invocation_index(events, |program, args| {
        program == Path::new(POWERSHELL)
            && action_uses_script(args, Path::new("packaging/preflight-release-tools.ps1"))
    });
    let step_4 = phase_index(events, STEP_4_CONTAINERS);
    let step_5 = phase_index(events, STEP_5_INSTALL_ROOT);
    let root_ready = phase_index(events, STEP_5_ROOT_READY);
    let step_6 = phase_index(events, STEP_6_INSTALL);
    let installer = invocation_index(events, |program, args| {
        program.ends_with(format!("solstone-setup-{VERSION}.exe"))
            && args.first().map(String::as_str) == Some("--silent")
    });
    let step_7 = phase_index(events, STEP_7_INSTALLED_IDENTITY);
    let step_8 = phase_index(events, STEP_8_DUMP_STATE);
    let dump_state = invocation_index(events, |program, args| {
        program.ends_with("solstone-windows-app.exe") && args == ["--dump-state"]
    });
    let step_9 = phase_index(events, STEP_9_SMOKE);
    let smoke = invocation_index(events, |program, args| {
        program == Path::new(POWERSHELL)
            && args.iter().any(|arg| arg == "scripts/smoke.ps1")
            && args.iter().any(|arg| arg == "-DisableInstalledFallback")
    });
    let step_10 = phase_index(events, STEP_10_REVALIDATE);
    let step_11 = phase_index(events, STEP_11_RECEIPT);
    let receipt_staged = phase_index(events, STEP_11_RECEIPT_STAGED);
    let ordered = [
        step_1,
        step_2,
        step_3,
        resolver,
        step_4,
        step_5,
        root_ready,
        step_6,
        installer,
        step_7,
        step_8,
        dump_state,
        step_9,
        smoke,
        step_10,
        step_11,
        receipt_staged,
    ];
    assert!(ordered.windows(2).all(|pair| pair[0] < pair[1]));
}

fn phase_index(events: &[WitnessEvent], expected: &str) -> usize {
    events
        .iter()
        .position(|event| matches!(event, WitnessEvent::Phase(phase) if phase == expected))
        .unwrap_or_else(|| panic!("missing phase {expected}"))
}

fn invocation_index(
    events: &[WitnessEvent],
    predicate: impl Fn(&Path, &[String]) -> bool,
) -> usize {
    events
        .iter()
        .position(|event| match event {
            WitnessEvent::Invocation { program, args } => predicate(program, args),
            WitnessEvent::Phase(_) => false,
        })
        .expect("missing native proof invocation")
}

fn hex_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
