// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use xtask::release_advisory::RUSTSEC_SOURCE_ID;
use xtask::release_clock::{Clock, FixedClock};
use xtask::release_receipt::{
    render_finalization_receipt, render_windows_native_proof_receipt, stage_finalization_receipt,
    stage_windows_native_proof_receipt, AdvisoryDatabaseReceipt, CandidateReceipt,
    CompanionManifestReceipt, FinalizationReceipt, ReceiptError, WindowsNativeProofReceipt,
    FINALIZATION_RECEIPT_SCHEMA, WINDOWS_NATIVE_PROOF_SCHEMA,
};
use xtask::rust_release_manifest::{companion_basename, PRODUCT, TARGET_TRIPLE};

static NEXT_TEST_ROOT: AtomicU64 = AtomicU64::new(0);

struct TestCheckout {
    path: PathBuf,
}

impl TestCheckout {
    fn new(label: &str) -> Self {
        let nonce = NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "solstone-release-receipt-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create test checkout");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestCheckout {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).expect("remove test checkout");
    }
}

fn companion() -> CompanionManifestReceipt {
    CompanionManifestReceipt {
        filename: companion_basename(),
        sha256: "c".repeat(64),
    }
}

fn finalization_receipt() -> FinalizationReceipt {
    FinalizationReceipt {
        schema: FINALIZATION_RECEIPT_SCHEMA.to_owned(),
        product: PRODUCT.to_owned(),
        version: "0.2.11".to_owned(),
        target: TARGET_TRIPLE.to_owned(),
        source_commit: "1".repeat(40),
        cargo_lock_sha256: "a".repeat(64),
        ui_package_lock_sha256: "b".repeat(64),
        companion_manifest: companion(),
        candidate: CandidateReceipt {
            relative_path: "target/release-candidate/0.2.11".to_owned(),
            file_count: 7,
        },
        selection_record_sha256: "d".repeat(64),
        signing_mode: "signed-verified".to_owned(),
        advisory_database: AdvisoryDatabaseReceipt {
            source_id: RUSTSEC_SOURCE_ID.to_owned(),
            commit: "e".repeat(40),
            tree_sha256: "f".repeat(64),
            acquired_at: "2026-07-21T10:00:00Z".to_owned(),
        },
        advisory_checked_at: "2026-07-21T11:00:00Z".to_owned(),
    }
}

fn native_proof_receipt(proved_at: &str) -> WindowsNativeProofReceipt {
    WindowsNativeProofReceipt {
        schema: WINDOWS_NATIVE_PROOF_SCHEMA.to_owned(),
        product: PRODUCT.to_owned(),
        version: "0.2.11".to_owned(),
        target: TARGET_TRIPLE.to_owned(),
        source_commit: "1".repeat(40),
        companion_manifest: companion(),
        setup_sha256: "2".repeat(64),
        packaged_executable_sha256: "3".repeat(64),
        installed_executable_sha256: "3".repeat(64),
        install_mode: "isolated-clean".to_owned(),
        installer_success: true,
        smoke_success: true,
        proved_at: proved_at.to_owned(),
    }
}

#[test]
fn finalization_receipt_render_is_byte_exact_and_canonical() {
    let clock = FixedClock::new("2026-07-21T11:00:00Z").expect("create fixed clock");
    let checked_at = clock.now().expect("read advisory check time");
    let mut receipt = finalization_receipt();
    receipt.advisory_checked_at = checked_at.as_str().to_owned();
    let expected = concat!(
        "{\n",
        "  \"schema\": \"solstone.rust-release-finalization.v1\",\n",
        "  \"product\": \"solstone-windows\",\n",
        "  \"version\": \"0.2.11\",\n",
        "  \"target\": \"x86_64-pc-windows-msvc\",\n",
        "  \"source_commit\": \"1111111111111111111111111111111111111111\",\n",
        "  \"cargo_lock_sha256\": \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\n",
        "  \"ui_package_lock_sha256\": \"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\",\n",
        "  \"companion_manifest\": {\n",
        "    \"filename\": \"solstone-windows-x86_64-pc-windows-msvc.rust-release-manifest.json\",\n",
        "    \"sha256\": \"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc\"\n",
        "  },\n",
        "  \"candidate\": {\n",
        "    \"relative_path\": \"target/release-candidate/0.2.11\",\n",
        "    \"file_count\": 7\n",
        "  },\n",
        "  \"selection_record_sha256\": \"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd\",\n",
        "  \"signing_mode\": \"signed-verified\",\n",
        "  \"advisory_database\": {\n",
        "    \"source_id\": \"https://github.com/RustSec/advisory-db\",\n",
        "    \"commit\": \"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee\",\n",
        "    \"tree_sha256\": \"ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff\",\n",
        "    \"acquired_at\": \"2026-07-21T10:00:00Z\"\n",
        "  },\n",
        "  \"advisory_checked_at\": \"2026-07-21T11:00:00Z\"\n",
        "}\n"
    );
    assert_eq!(
        render_finalization_receipt(&receipt).expect("render finalization receipt"),
        expected.as_bytes()
    );
    assert_eq!(clock.calls(), 1);
}

#[test]
fn native_proof_render_is_byte_exact_and_time_is_injected() {
    let clock = FixedClock::new("2026-07-21T12:00:00Z").expect("create fixed clock");
    let proved_at = clock.now().expect("read proof time");
    let receipt = native_proof_receipt(proved_at.as_str());
    let expected = concat!(
        "{\n",
        "  \"schema\": \"solstone.windows-native-proof.v1\",\n",
        "  \"product\": \"solstone-windows\",\n",
        "  \"version\": \"0.2.11\",\n",
        "  \"target\": \"x86_64-pc-windows-msvc\",\n",
        "  \"source_commit\": \"1111111111111111111111111111111111111111\",\n",
        "  \"companion_manifest\": {\n",
        "    \"filename\": \"solstone-windows-x86_64-pc-windows-msvc.rust-release-manifest.json\",\n",
        "    \"sha256\": \"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc\"\n",
        "  },\n",
        "  \"setup_sha256\": \"2222222222222222222222222222222222222222222222222222222222222222\",\n",
        "  \"packaged_executable_sha256\": \"3333333333333333333333333333333333333333333333333333333333333333\",\n",
        "  \"installed_executable_sha256\": \"3333333333333333333333333333333333333333333333333333333333333333\",\n",
        "  \"install_mode\": \"isolated-clean\",\n",
        "  \"installer_success\": true,\n",
        "  \"smoke_success\": true,\n",
        "  \"proved_at\": \"2026-07-21T12:00:00Z\"\n",
        "}\n"
    );
    assert_eq!(
        render_windows_native_proof_receipt(&receipt).expect("render native-proof receipt"),
        expected.as_bytes()
    );
    assert_eq!(clock.calls(), 1);
}

#[test]
fn finalization_receipt_stages_then_atomically_replaces_existing_final_target() {
    let checkout = TestCheckout::new("atomic");
    let receipt = finalization_receipt();
    let expected = render_finalization_receipt(&receipt).expect("render receipt");
    let staged = stage_finalization_receipt(checkout.path(), &receipt).expect("stage receipt");
    assert_eq!(
        staged.staged_relative_path(),
        "target/release-evidence/0.2.11/.rust-release-finalization.json.tmp"
    );
    assert_eq!(
        staged.final_relative_path(),
        "target/release-evidence/0.2.11/rust-release-finalization.json"
    );
    assert_eq!(
        fs::read(checkout.path().join(staged.staged_relative_path())).expect("read staged receipt"),
        expected
    );
    let final_relative = staged.promote().expect("promote receipt");
    assert_eq!(
        fs::read(checkout.path().join(&final_relative)).expect("read final receipt"),
        expected
    );
    let mut replacement = receipt;
    replacement.advisory_checked_at = "2026-07-21T11:30:00Z".to_owned();
    let replacement_bytes =
        render_finalization_receipt(&replacement).expect("render replacement receipt");
    let replacement_stage =
        stage_finalization_receipt(checkout.path(), &replacement).expect("stage replacement");
    assert_eq!(
        fs::read(
            checkout
                .path()
                .join(replacement_stage.final_relative_path())
        )
        .expect("read prior final receipt before replacement"),
        expected
    );
    let replacement_relative = replacement_stage
        .promote()
        .expect("atomically replace finalization receipt");
    assert_eq!(
        fs::read(checkout.path().join(replacement_relative)).expect("read replacement receipt"),
        replacement_bytes
    );
}

#[test]
fn promotion_refuses_a_final_target_created_after_staging() {
    let checkout = TestCheckout::new("late-final");
    let receipt = native_proof_receipt("2026-07-21T12:00:00Z");
    let staged =
        stage_windows_native_proof_receipt(checkout.path(), &receipt).expect("stage proof");
    let staged_relative = staged.staged_relative_path();
    let final_path = checkout.path().join(staged.final_relative_path());
    fs::write(&final_path, b"pre-existing proof").expect("create competing final target");
    assert_eq!(
        staged.promote().expect_err("refuse competing final target"),
        ReceiptError::FinalTargetExists
    );
    assert_eq!(
        fs::read(&final_path).expect("read competing target"),
        b"pre-existing proof"
    );
    assert!(!checkout.path().join(staged_relative).exists());
}

#[test]
fn receipt_types_cannot_carry_private_source_data() {
    struct PrivateSourceData {
        selection_path: &'static str,
        host_account: &'static str,
        credential: &'static str,
        command_line: &'static str,
        environment: &'static str,
        certificate: &'static str,
    }

    let private = PrivateSourceData {
        selection_path: "/home/private/operator/tool-selection.json",
        host_account: "release-user@secret-build-host",
        credential: "DIGICERT_API_KEY=not-public",
        command_line: "smctl sign --keypair-alias private-alias",
        environment: "USERPROFILE=C:\\Users\\PrivateOperator",
        certificate: "private-certificate-material",
    };
    let mut bytes =
        render_finalization_receipt(&finalization_receipt()).expect("render finalization receipt");
    bytes.extend(
        render_windows_native_proof_receipt(&native_proof_receipt("2026-07-21T12:00:00Z"))
            .expect("render native-proof receipt"),
    );
    let rendered = String::from_utf8(bytes).expect("receipt JSON is UTF-8");
    for private_value in [
        private.selection_path,
        private.host_account,
        private.credential,
        private.command_line,
        private.environment,
        private.certificate,
    ] {
        assert!(!rendered.contains(private_value));
    }
}
