// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use xtask::observer_contract::{self, UnsafePathReason, VerifyError};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TempTree {
    root: PathBuf,
}

impl TempTree {
    fn good() -> Self {
        let root = std::env::temp_dir().join(format!(
            "solstone-observer-contract-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("create isolated test root");
        copy_tree(&repo_root().join("contracts/observer-client"), &root);
        Self { root }
    }

    fn bundle(&self) -> PathBuf {
        self.root.join("bundle")
    }

    fn adoption(&self) -> PathBuf {
        self.root.join("adoption.json")
    }

    fn verify(&self) -> Result<observer_contract::VerifyReport, VerifyError> {
        observer_contract::verify(&self.bundle(), &self.adoption())
    }

    fn json(&self, relative: &str) -> Value {
        serde_json::from_slice(&fs::read(self.root.join(relative)).expect("read JSON"))
            .expect("parse JSON")
    }

    fn write_json(&self, relative: &str, value: &Value) {
        fs::write(
            self.root.join(relative),
            serde_json::to_vec_pretty(value).expect("serialize JSON"),
        )
        .expect("write JSON");
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove isolated test root");
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a workspace parent")
        .to_path_buf()
}

fn copy_tree(source: &Path, destination: &Path) {
    for entry in fs::read_dir(source).expect("read source tree") {
        let entry = entry.expect("read source entry");
        let target = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(entry.path()).expect("source metadata");
        if metadata.is_dir() {
            fs::create_dir(&target).expect("create destination directory");
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy source file");
        }
    }
}

fn manifest_path_mutation(path: &str) -> VerifyError {
    let tree = TempTree::good();
    let mut manifest = tree.json("bundle/manifest.json");
    manifest["files"][0]["path"] = json!(path);
    tree.write_json("bundle/manifest.json", &manifest);
    tree.verify().expect_err("mutated path must fail")
}

#[test]
fn observer_contract_good_tree_verifies() {
    let tree = TempTree::good();
    let report = tree.verify().expect("known-good tree verifies");
    assert_eq!(report.operation_count, 8);
    assert_eq!(report.fixture_count, 29);
    assert_eq!(report.vector_count, 17);
}

#[test]
fn observer_contract_rejects_unsafe_manifest_paths_by_class() {
    assert!(matches!(
        manifest_path_mutation("/absolute.json"),
        VerifyError::UnsafePath {
            reason: UnsafePathReason::Absolute,
            ..
        }
    ));
    assert!(matches!(
        manifest_path_mutation(""),
        VerifyError::UnsafePath {
            reason: UnsafePathReason::Empty,
            ..
        }
    ));
    assert!(matches!(
        manifest_path_mutation("NUL.json"),
        VerifyError::UnsafePath {
            reason: UnsafePathReason::ReservedName,
            ..
        }
    ));
    assert!(matches!(
        manifest_path_mutation("bad./file.json"),
        VerifyError::UnsafePath {
            reason: UnsafePathReason::TrailingDotOrSpace,
            ..
        }
    ));
    assert!(matches!(
        manifest_path_mutation("bad:name.json"),
        VerifyError::UnsafePath {
            reason: UnsafePathReason::NonPortableName,
            ..
        }
    ));
    assert!(matches!(
        manifest_path_mutation("bad//name.json"),
        VerifyError::UnsafePath {
            reason: UnsafePathReason::EmptyComponent,
            ..
        }
    ));
    assert!(matches!(
        manifest_path_mutation("../escape.json"),
        VerifyError::Traversal { .. }
    ));
    assert!(matches!(
        manifest_path_mutation("fixtures\\wire-behavior.json"),
        VerifyError::Backslash { .. }
    ));
    assert!(matches!(
        manifest_path_mutation("bad\nname.json"),
        VerifyError::ControlChar { .. }
    ));
}

#[test]
fn observer_contract_rejects_duplicate_and_case_colliding_manifest_paths() {
    let tree = TempTree::good();
    let mut manifest = tree.json("bundle/manifest.json");
    let entry = manifest["files"][0].clone();
    manifest["files"].as_array_mut().unwrap().push(entry);
    tree.write_json("bundle/manifest.json", &manifest);
    assert!(matches!(
        tree.verify(),
        Err(VerifyError::DuplicatePath { .. })
    ));

    let tree = TempTree::good();
    let mut manifest = tree.json("bundle/manifest.json");
    let mut entry = manifest["files"][0].clone();
    entry["path"] = json!("CONSUMER-AUDIT.JSON");
    manifest["files"].as_array_mut().unwrap().push(entry);
    tree.write_json("bundle/manifest.json", &manifest);
    assert!(matches!(
        tree.verify(),
        Err(VerifyError::CaseCollision { .. })
    ));
}

#[test]
fn observer_contract_rejects_missing_unlisted_and_extra_files() {
    let tree = TempTree::good();
    fs::remove_file(tree.bundle().join("consumer-audit.json")).unwrap();
    assert!(matches!(
        tree.verify(),
        Err(VerifyError::MissingFile { .. })
    ));

    let tree = TempTree::good();
    fs::write(tree.bundle().join("unlisted.json"), b"{}\n").unwrap();
    assert!(matches!(
        tree.verify(),
        Err(VerifyError::UnlistedFile { .. })
    ));

    let tree = TempTree::good();
    fs::create_dir(tree.bundle().join("unused")).unwrap();
    assert!(matches!(tree.verify(), Err(VerifyError::ExtraFile { .. })));
}

#[test]
fn observer_contract_rejects_declared_extra_and_missing_inventory() {
    let tree = TempTree::good();
    let mut manifest = tree.json("bundle/manifest.json");
    manifest["files"].as_array_mut().unwrap().push(json!({
        "path": "extra.json",
        "sha256": "0000000000000000000000000000000000000000000000000000000000000000"
    }));
    tree.write_json("bundle/manifest.json", &manifest);
    fs::write(tree.bundle().join("extra.json"), b"\n").unwrap();
    assert!(matches!(tree.verify(), Err(VerifyError::ExtraFile { .. })));

    let tree = TempTree::good();
    let mut manifest = tree.json("bundle/manifest.json");
    manifest["files"].as_array_mut().unwrap().remove(0);
    tree.write_json("bundle/manifest.json", &manifest);
    assert!(matches!(
        tree.verify(),
        Err(VerifyError::ManifestInventoryMismatch { .. })
    ));
}

#[cfg(unix)]
#[test]
fn observer_contract_rejects_symlinks_special_types_and_executable_modes() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::os::unix::net::UnixListener;

    let tree = TempTree::good();
    let file = tree.bundle().join("consumer-audit.json");
    fs::remove_file(&file).unwrap();
    symlink("manifest.json", &file).unwrap();
    assert!(matches!(
        tree.verify(),
        Err(VerifyError::NonRegularFile { .. })
    ));

    let tree = TempTree::good();
    let file = tree.bundle().join("consumer-audit.json");
    fs::remove_file(&file).unwrap();
    fs::create_dir(&file).unwrap();
    assert!(matches!(
        tree.verify(),
        Err(VerifyError::NonRegularFile { .. })
    ));

    let tree = TempTree::good();
    let socket = tree.bundle().join("unexpected.sock");
    let _listener = UnixListener::bind(&socket).unwrap();
    assert!(matches!(
        tree.verify(),
        Err(VerifyError::NonRegularFile { .. })
    ));

    let tree = TempTree::good();
    let file = tree.bundle().join("consumer-audit.json");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        tree.verify(),
        Err(VerifyError::InvalidFileMode { .. })
    ));
}

#[test]
fn observer_contract_rejects_manifest_and_payload_digest_mutations() {
    let tree = TempTree::good();
    let mut bytes = fs::read(tree.bundle().join("manifest.json")).unwrap();
    bytes.push(b'\n');
    fs::write(tree.bundle().join("manifest.json"), bytes).unwrap();
    assert!(matches!(
        tree.verify(),
        Err(VerifyError::DigestMismatch { path, .. }) if path == "manifest.json"
    ));

    let tree = TempTree::good();
    let file = tree.bundle().join("consumer-audit.json");
    let mut bytes = fs::read(&file).unwrap();
    bytes.push(b'\n');
    fs::write(file, bytes).unwrap();
    assert!(matches!(
        tree.verify(),
        Err(VerifyError::DigestMismatch { path, .. }) if path == "consumer-audit.json"
    ));
}

#[test]
fn observer_contract_rejects_malformed_json_documents() {
    for relative in [
        "adoption.json",
        "bundle/manifest.json",
        "bundle/projection.openapi.json",
        "bundle/fixtures/wire-behavior.json",
        "bundle/vectors.json",
    ] {
        let tree = TempTree::good();
        fs::write(tree.root.join(relative), b"{").unwrap();
        assert!(
            matches!(tree.verify(), Err(VerifyError::MalformedJson { .. })),
            "{relative}"
        );
    }
}

#[test]
fn observer_contract_rejects_every_forbidden_adoption_metadata_class() {
    for field in [
        "generated_at",
        "hostname",
        "username",
        "temp_path",
        "internal_job_id",
        "rollout_state",
        "windows_commit",
    ] {
        let tree = TempTree::good();
        let mut adoption = tree.json("adoption.json");
        adoption[field] = json!("forbidden");
        tree.write_json("adoption.json", &adoption);
        assert!(matches!(
            tree.verify(),
            Err(VerifyError::ForbiddenAdoptionMetadata { field: actual }) if actual == field
        ));
    }
}

#[test]
fn observer_contract_rejects_adoption_scalar_shape_and_pin_mismatches() {
    for field in [
        "consumer_identifier",
        "authority_repository",
        "authority_commit",
        "bundle_semver",
        "archive_sha256",
        "authority_manifest_path",
        "authority_manifest_sha256",
    ] {
        let tree = TempTree::good();
        let mut adoption = tree.json("adoption.json");
        adoption[field] = json!("wrong");
        tree.write_json("adoption.json", &adoption);
        assert!(matches!(
            tree.verify(),
            Err(VerifyError::AdoptionFieldMismatch { field: actual }) if actual == field
        ));
    }

    let tree = TempTree::good();
    let mut adoption = tree.json("adoption.json");
    adoption["adopted_fixture_ids"] = json!(29);
    tree.write_json("adoption.json", &adoption);
    assert!(matches!(
        tree.verify(),
        Err(VerifyError::AdoptionShapeMismatch { .. })
    ));
}

#[test]
fn observer_contract_rejects_duplicate_unsorted_and_mismatched_coverage() {
    let tree = TempTree::good();
    let mut adoption = tree.json("adoption.json");
    let ids = adoption["adopted_vector_ids"].as_array_mut().unwrap();
    ids.push(ids[0].clone());
    tree.write_json("adoption.json", &adoption);
    assert!(matches!(
        tree.verify(),
        Err(VerifyError::AdoptionCoverageDuplicate { .. })
    ));

    let tree = TempTree::good();
    let mut adoption = tree.json("adoption.json");
    adoption["adopted_operation_ids"]
        .as_array_mut()
        .unwrap()
        .swap(0, 1);
    tree.write_json("adoption.json", &adoption);
    assert!(matches!(
        tree.verify(),
        Err(VerifyError::AdoptionCoverageUnsorted { .. })
    ));

    let tree = TempTree::good();
    let mut adoption = tree.json("adoption.json");
    adoption["adopted_fixture_ids"]
        .as_array_mut()
        .unwrap()
        .remove(0);
    tree.write_json("adoption.json", &adoption);
    assert!(matches!(
        tree.verify(),
        Err(VerifyError::AdoptionCoverageMismatch { .. })
    ));
}

#[test]
fn observer_contract_rejects_manifest_projection_and_adopted_set_mutations() {
    let tree = TempTree::good();
    let mut manifest = tree.json("bundle/manifest.json");
    manifest["observer_protocol_version"] = json!(3);
    tree.write_json("bundle/manifest.json", &manifest);
    assert!(matches!(
        tree.verify(),
        Err(VerifyError::ManifestFieldMismatch { field }) if field == "observer_protocol_version"
    ));

    let tree = TempTree::good();
    let mut projection = tree.json("bundle/projection.openapi.json");
    projection["paths"]["/sse/events"]
        .as_object_mut()
        .unwrap()
        .remove("get");
    tree.write_json("bundle/projection.openapi.json", &projection);
    assert!(matches!(
        tree.verify(),
        Err(VerifyError::ProjectionMismatch { .. })
    ));

    let tree = TempTree::good();
    let mut fixtures = tree.json("bundle/fixtures/wire-behavior.json");
    fixtures["fixtures"].as_array_mut().unwrap().remove(0);
    tree.write_json("bundle/fixtures/wire-behavior.json", &fixtures);
    assert!(matches!(
        tree.verify(),
        Err(VerifyError::FixtureSetMismatch { .. })
    ));

    let tree = TempTree::good();
    let mut vectors = tree.json("bundle/vectors.json");
    vectors["vectors"].as_array_mut().unwrap().remove(0);
    tree.write_json("bundle/vectors.json", &vectors);
    assert!(matches!(
        tree.verify(),
        Err(VerifyError::VectorSetMismatch { .. })
    ));
}
