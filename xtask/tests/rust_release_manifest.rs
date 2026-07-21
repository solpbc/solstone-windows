// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use xtask::rust_release_manifest::{
    self, companion_basename, CheckoutFacts, ClassificationMode, Manifest, ManifestError,
    ReleaseEvidence, TargetEvidence, MANIFEST_DISCLAIMER, PRODUCT, SCHEMA_SHA256, TARGET_FEATURES,
    TARGET_PROFILE, TARGET_TRIPLE,
};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

type ManifestMutation = Box<dyn Fn(&mut Value)>;
type EvidenceMutation = Box<dyn Fn(&mut ReleaseEvidence)>;

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "solstone-{label}-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("create isolated root");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove isolated root");
    }
}

struct TempTree {
    root: TempDir,
    facts: CheckoutFacts,
}

impl TempTree {
    fn good() -> Self {
        let root = TempDir::new("rust-release-manifest");
        copy_tree(&fixture_root().join("release-dir"), &root.0);
        // The fixture baseline is a fixed valid placeholder: these read-only
        // classifier tests verify artifact files, not container executable bytes.
        let manifest = read_manifest(&root.0.join(companion_basename()));
        let facts = facts_for(&manifest);
        Self { root, facts }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.0.join(name)
    }

    fn manifest_path(&self) -> PathBuf {
        self.path(&companion_basename())
    }

    fn manifest_value(&self) -> Value {
        serde_json::from_slice(&fs::read(self.manifest_path()).unwrap()).unwrap()
    }

    fn write_manifest_value(&self, value: &Value) {
        let mut bytes = serde_json::to_vec_pretty(value).unwrap();
        bytes.push(b'\n');
        fs::write(self.manifest_path(), bytes).unwrap();
    }

    fn write_json_artifact(&self, name: &str, value: &Value) {
        let mut bytes = serde_json::to_vec(value).unwrap();
        bytes.push(b'\n');
        fs::write(self.path(name), bytes).unwrap();
        self.sync_artifact(name);
    }

    fn sync_artifact(&self, name: &str) {
        let bytes = fs::read(self.path(name)).unwrap();
        let mut manifest = self.manifest_value();
        let artifact = manifest["artifacts"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|entry| entry["path"] == name)
            .expect("fixture artifact is listed");
        artifact["bytes"] = json!(bytes.len());
        artifact["sha256"] = json!(lower_hex(&Sha256::digest(&bytes)));
        self.write_manifest_value(&manifest);
    }

    fn validate_release(&self) -> Result<rust_release_manifest::ClassifierReport, ManifestError> {
        rust_release_manifest::validate_release_dir_with_facts(&self.root.0, &self.facts)
    }

    fn validate_manifest(&self) -> Result<rust_release_manifest::ClassifierReport, ManifestError> {
        rust_release_manifest::validate_manifest_with_facts(&self.manifest_path(), &self.facts)
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a workspace parent")
        .to_path_buf()
}

fn fixture_root() -> PathBuf {
    repo_root().join("xtask/tests/fixtures/rust-release-manifest")
}

fn copy_tree(source: &Path, destination: &Path) {
    for entry in fs::read_dir(source).expect("read fixture tree") {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(entry.path()).unwrap();
        if metadata.file_type().is_dir() {
            fs::create_dir(&target).unwrap();
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn read_manifest(path: &Path) -> Manifest {
    rust_release_manifest::validate_manifest_bytes(&fs::read(path).unwrap()).unwrap()
}

fn facts_for(manifest: &Manifest) -> CheckoutFacts {
    let projection = rust_release_manifest::project_release_toolchain(&repo_root()).unwrap();
    CheckoutFacts {
        product: PRODUCT.to_owned(),
        version: manifest.version.clone(),
        source_commit: manifest.source_commit.clone(),
        source_dirty: false,
        cargo_lock_sha256: manifest.cargo_lock_sha256.clone(),
        rustc_verbose: projection.rustc_verbose,
        cargo_version: projection.cargo_version,
        target_triple: TARGET_TRIPLE.to_owned(),
        target_profile: TARGET_PROFILE.to_owned(),
        target_features: TARGET_FEATURES
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        cargo_deny_version: projection.cargo_deny_version,
        active_exceptions: rust_release_manifest::read_active_exceptions(&repo_root()).unwrap(),
        unsigned_native_tools: projection.unsigned_native_tools,
        signed_native_tools: projection.signed_native_tools,
    }
}

fn lower_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;

    let mut output = String::new();
    for byte in bytes {
        write!(&mut output, "{byte:02x}").unwrap();
    }
    output
}

fn mutate_manifest<F>(mutate: F) -> ManifestError
where
    F: FnOnce(&mut Value),
{
    let tree = TempTree::good();
    let mut manifest = tree.manifest_value();
    mutate(&mut manifest);
    tree.write_manifest_value(&manifest);
    tree.validate_release().expect_err("mutation must fail")
}

fn add_current_delta(tree: &TempTree) {
    let name = "Solstone-0.2.11-delta.nupkg";
    let bytes = b"inert delta package bytes\n";
    fs::write(tree.path(name), bytes).unwrap();
    let sha1 = lower_hex(&Sha1::digest(bytes));
    let sha256 = lower_hex(&Sha256::digest(bytes));

    let mut feed: Value =
        serde_json::from_slice(&fs::read(tree.path("releases.win.json")).unwrap()).unwrap();
    feed["Assets"].as_array_mut().unwrap().push(json!({
        "PackageId":"Solstone","Version":"0.2.11","Type":"Delta",
        "FileName":name,"SHA1":sha1,"SHA256":sha256.clone(),"Size":bytes.len(),
        "NotesMarkdown":"","NotesHTML":""
    }));
    tree.write_json_artifact("releases.win.json", &feed);

    let mut assets: Value =
        serde_json::from_slice(&fs::read(tree.path("assets.win.json")).unwrap()).unwrap();
    assets
        .as_array_mut()
        .unwrap()
        .push(json!({"RelativeFileName":name,"Type":"Delta"}));
    tree.write_json_artifact("assets.win.json", &assets);

    let mut manifest = tree.manifest_value();
    manifest["artifacts"].as_array_mut().unwrap().push(json!({
        "path":name,"sha256":sha256,"bytes":bytes.len()
    }));
    manifest["artifacts"]
        .as_array_mut()
        .unwrap()
        .sort_by(|first, second| first["path"].as_str().cmp(&second["path"].as_str()));
    tree.write_manifest_value(&manifest);
}

fn schema_mutation<F>(mutate: F) -> ManifestError
where
    F: FnOnce(&mut Value),
{
    let mut manifest: Value = serde_json::from_slice(
        &fs::read(
            fixture_root()
                .join("release-dir")
                .join(companion_basename()),
        )
        .unwrap(),
    )
    .unwrap();
    mutate(&mut manifest);
    rust_release_manifest::validate_manifest_bytes(&serde_json::to_vec(&manifest).unwrap())
        .expect_err("schema mutation must fail")
}

#[test]
fn rust_release_manifest_schema_is_exact_and_compiles_unchanged() {
    let bytes = fs::read(repo_root().join("schemas/rust-release-manifest/v1.json")).unwrap();
    assert_eq!(bytes.len(), 4_780);
    assert_eq!(
        SCHEMA_SHA256,
        "82b5233a26131d9f35beb8a94a02f686556cde2a977614a75d5a7866ace75080"
    );
    assert_eq!(lower_hex(&Sha256::digest(&bytes)), SCHEMA_SHA256);
    rust_release_manifest::verify_vendored_schema(&repo_root()).unwrap();
    rust_release_manifest::validate_manifest_bytes(
        &fs::read(
            fixture_root()
                .join("release-dir")
                .join(companion_basename()),
        )
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn rust_release_manifest_schema_asserts_lookaheads_and_date_time() {
    for path in ["/absolute.bin", "safe/../escape.bin"] {
        assert_eq!(
            schema_mutation(|manifest| manifest["artifacts"][0]["path"] = json!(path)),
            ManifestError::SchemaViolation
        );
    }
    assert_eq!(
        schema_mutation(|manifest| {
            manifest["dependency_policy"]["advisory_checked_at"] = json!("not-a-date")
        }),
        ManifestError::SchemaViolation
    );
    for mutation in 0..3 {
        assert_eq!(
            schema_mutation(|manifest| match mutation {
                0 => manifest["packaged_executable"]["sha256"] = json!("A".repeat(64)),
                1 => manifest["packaged_executable"]["sha256"] = json!("3".repeat(63)),
                _ => manifest["packaged_executable"]["bytes"] = json!(0),
            }),
            ManifestError::SchemaViolation,
            "packaged executable mutation {mutation}"
        );
    }
    let mut valid: Value = serde_json::from_slice(
        &fs::read(
            fixture_root()
                .join("release-dir")
                .join(companion_basename()),
        )
        .unwrap(),
    )
    .unwrap();
    valid["dependency_policy"]["advisory_checked_at"] = json!("2026-07-20T12:34:56Z");
    rust_release_manifest::validate_manifest_bytes(&serde_json::to_vec(&valid).unwrap()).unwrap();
}

#[test]
fn rust_release_manifest_schema_rejects_required_and_unknown_field_classes() {
    for field in [
        "schema_version",
        "product",
        "version",
        "source_commit",
        "source_dirty",
        "cargo_lock_sha256",
        "rust",
        "target",
        "native_tools",
        "dependency_policy",
        "active_exceptions",
        "packaged_executable",
        "artifacts",
    ] {
        assert_eq!(
            schema_mutation(|manifest| {
                manifest.as_object_mut().unwrap().remove(field);
            }),
            ManifestError::SchemaViolation,
            "{field}"
        );
    }
    for mutation in 0..20 {
        assert_eq!(
            schema_mutation(|manifest| match mutation {
                0 => {
                    manifest["rust"]
                        .as_object_mut()
                        .unwrap()
                        .remove("rustc_verbose");
                }
                1 => {
                    manifest["rust"]
                        .as_object_mut()
                        .unwrap()
                        .remove("cargo_version");
                }
                2 => {
                    manifest["target"].as_object_mut().unwrap().remove("kind");
                }
                3 => {
                    manifest["target"].as_object_mut().unwrap().remove("triple");
                }
                4 => {
                    manifest["target"]
                        .as_object_mut()
                        .unwrap()
                        .remove("profile");
                }
                5 => {
                    manifest["target"]
                        .as_object_mut()
                        .unwrap()
                        .remove("features");
                }
                6 => {
                    manifest["dependency_policy"]
                        .as_object_mut()
                        .unwrap()
                        .remove("cargo_deny_version");
                }
                7 => {
                    manifest["dependency_policy"]
                        .as_object_mut()
                        .unwrap()
                        .remove("deterministic_gate");
                }
                8 => {
                    manifest["dependency_policy"]
                        .as_object_mut()
                        .unwrap()
                        .remove("advisory_checked_at");
                }
                9 => {
                    manifest["artifacts"][0]
                        .as_object_mut()
                        .unwrap()
                        .remove("path");
                }
                10 => {
                    manifest["artifacts"][0]
                        .as_object_mut()
                        .unwrap()
                        .remove("sha256");
                }
                11 => {
                    manifest["artifacts"][0]
                        .as_object_mut()
                        .unwrap()
                        .remove("bytes");
                }
                12 => manifest["unknown"] = json!(true),
                13 => manifest["rust"]["unknown"] = json!(true),
                14 => manifest["target"]["unknown"] = json!(true),
                15 => manifest["dependency_policy"]["unknown"] = json!(true),
                16 => manifest["artifacts"][0]["unknown"] = json!(true),
                17 => {
                    manifest["packaged_executable"]
                        .as_object_mut()
                        .unwrap()
                        .remove("sha256");
                }
                18 => {
                    manifest["packaged_executable"]
                        .as_object_mut()
                        .unwrap()
                        .remove("bytes");
                }
                _ => manifest["packaged_executable"]["unknown"] = json!(true),
            }),
            ManifestError::SchemaViolation,
            "mutation {mutation}"
        );
    }
}

#[test]
fn rust_release_manifest_good_modes_accept_and_report_boundaries() {
    let tree = TempTree::good();
    let manifest = tree.validate_manifest().unwrap();
    assert_eq!(manifest.mode, ClassificationMode::SiblingBytesOnly);
    assert_eq!(manifest.disclaimer, Some(MANIFEST_DISCLAIMER));
    let release = tree.validate_release().unwrap();
    assert_eq!(release.mode, ClassificationMode::CompleteCurrentBundle);
    assert_eq!(release.artifact_count, 6);
}

#[test]
fn rust_release_manifest_accepts_exact_signed_and_current_delta_shapes() {
    let tree = TempTree::good();
    let mut manifest = tree.manifest_value();
    manifest["native_tools"] = serde_json::to_value(&tree.facts.signed_native_tools).unwrap();
    tree.write_manifest_value(&manifest);
    tree.validate_release().unwrap();

    let tree = TempTree::good();
    add_current_delta(&tree);
    assert_eq!(tree.validate_manifest().unwrap().artifact_count, 7);
    assert_eq!(tree.validate_release().unwrap().artifact_count, 7);
}

#[test]
fn rust_release_manifest_rejects_checkout_binding_drift() {
    let cases: Vec<(ManifestError, ManifestMutation)> = vec![
        (
            ManifestError::ProductMismatch,
            Box::new(|value| value["product"] = json!("other")),
        ),
        (
            ManifestError::VersionMismatch,
            Box::new(|value| value["version"] = json!("0.2.10")),
        ),
        (
            ManifestError::SourceCommitMismatch,
            Box::new(|value| {
                value["source_commit"] = json!("3333333333333333333333333333333333333333")
            }),
        ),
        (
            ManifestError::CargoLockMismatch,
            Box::new(|value| {
                value["cargo_lock_sha256"] =
                    json!("3333333333333333333333333333333333333333333333333333333333333333")
            }),
        ),
        (
            ManifestError::TargetKindMismatch,
            Box::new(|value| value["target"] = json!({"kind":"source"})),
        ),
        (
            ManifestError::TargetTripleMismatch,
            Box::new(|value| value["target"]["triple"] = json!("other-target")),
        ),
        (
            ManifestError::TargetProfileMismatch,
            Box::new(|value| value["target"]["profile"] = json!("debug")),
        ),
        (
            ManifestError::TargetFeaturesMismatch,
            Box::new(|value| value["target"]["features"] = json!([])),
        ),
        (
            ManifestError::CargoDenyVersionMismatch,
            Box::new(|value| value["dependency_policy"]["cargo_deny_version"] = json!("0.20.1")),
        ),
        (
            ManifestError::ActiveExceptionsMismatch,
            Box::new(|value| {
                value["active_exceptions"].as_array_mut().unwrap().pop();
            }),
        ),
    ];
    for (expected, mutate) in cases {
        assert_eq!(mutate_manifest(mutate), expected);
    }

    let tree = TempTree::good();
    let mut facts = tree.facts.clone();
    facts.source_dirty = true;
    assert_eq!(
        rust_release_manifest::validate_release_dir_with_facts(&tree.root.0, &facts),
        Err(ManifestError::SourceDirty)
    );

    let mut manifest = read_manifest(&tree.manifest_path());
    manifest.packaged_executable.bytes = 0;
    assert_eq!(
        rust_release_manifest::validate_semantic_binding(&manifest, &tree.facts),
        Err(ManifestError::PackagedExecutableInvalid)
    );
    let mut manifest = read_manifest(&tree.manifest_path());
    manifest.packaged_executable.sha256 = "A".repeat(64);
    assert_eq!(
        rust_release_manifest::validate_semantic_binding(&manifest, &tree.facts),
        Err(ManifestError::PackagedExecutableInvalid)
    );
}

#[test]
fn rust_release_manifest_rustc_evidence_is_byte_exact() {
    for value in [
        "release: 1.96.0\nhost: x86_64-pc-windows-msvc extra",
        "prefix release: 1.96.0\nhost: x86_64-pc-windows-msvc",
        "release: 1.96.0\nhost: x86_64-pc-windows-msvc suffix",
        "release: 1.96.0\nhost: x86_64-pc-windows-msvc\nrelease: 1.96.0",
        "host: x86_64-pc-windows-msvc\nrelease: 1.96.0",
        "release: 1.96.0\nhost: x86_64-pc-windows-msvc\n",
    ] {
        assert_eq!(
            mutate_manifest(|manifest| manifest["rust"]["rustc_verbose"] = json!(value)),
            ManifestError::RustcEvidenceMismatch
        );
    }
    assert_eq!(
        mutate_manifest(|manifest| manifest["rust"]["cargo_version"] = json!("1.95.0")),
        ManifestError::CargoVersionMismatch
    );
}

#[test]
fn rust_release_manifest_rejects_native_tool_shape_and_signing_drift() {
    assert_eq!(
        mutate_manifest(|manifest| manifest["native_tools"]["signing_mode"] = json!("signed")),
        ManifestError::SigningModeInvalid
    );
    for mutation in 0..3 {
        let expected = if mutation == 0 {
            ManifestError::SigningModeInvalid
        } else {
            ManifestError::NativeToolsMismatch
        };
        assert_eq!(
            mutate_manifest(|manifest| match mutation {
                0 => {
                    manifest["native_tools"]
                        .as_object_mut()
                        .unwrap()
                        .remove("signing_mode");
                }
                1 => manifest["native_tools"]["unknown"] = json!("1"),
                _ => manifest["native_tools"]["dotnet"] = json!("8.0.421"),
            }),
            expected
        );
    }
    assert_eq!(
        mutate_manifest(|manifest| {
            manifest["native_tools"]["signing_mode"] = json!("signed-verified");
        }),
        ManifestError::NativeToolsMismatch
    );
}

#[test]
fn rust_release_manifest_diagnostics_do_not_echo_rejected_values() {
    let rejected = [
        "acc", "ount-", "sec", "ret-", "pri", "vate-", "ho", "st-", "pa", "th-value",
    ]
    .concat();
    let error =
        mutate_manifest(|manifest| manifest["native_tools"]["dotnet"] = json!(rejected.clone()));
    assert_eq!(error, ManifestError::NativeToolsMismatch);
    let diagnostic = error.to_string();
    assert!(!diagnostic.contains(&rejected));
    assert_eq!(
        ManifestError::EvidenceInvalid {
            field: "source_commit"
        }
        .to_string(),
        "release evidence is invalid: field `source_commit`"
    );
    assert_eq!(
        ManifestError::EvidenceNotCanonical {
            field: "active_exceptions"
        }
        .to_string(),
        "release evidence is not canonical: field `active_exceptions`"
    );
}

#[test]
fn rust_release_manifest_rejects_artifact_bytes_hash_and_post_render_mutation() {
    assert_eq!(
        mutate_manifest(|manifest| manifest["artifacts"][0]["bytes"] = json!(75)),
        ManifestError::ArtifactBytesMismatch
    );
    assert_eq!(
        mutate_manifest(|manifest| {
            manifest["artifacts"][0]["sha256"] =
                json!("3333333333333333333333333333333333333333333333333333333333333333")
        }),
        ManifestError::ArtifactSha256Mismatch
    );
    let tree = TempTree::good();
    let evidence = ReleaseEvidence::from(read_manifest(&tree.manifest_path()));
    rust_release_manifest::render_release_evidence(&evidence).unwrap();
    fs::write(
        tree.path("Solstone-0.2.11-full.nupkg"),
        b"changed after render\n",
    )
    .unwrap();
    assert!(matches!(
        tree.validate_release(),
        Err(ManifestError::ArtifactBytesMismatch | ManifestError::ArtifactSha256Mismatch)
    ));
}

#[test]
fn rust_release_manifest_rejects_incomplete_extra_and_misnamed_bundles() {
    let tree = TempTree::good();
    fs::remove_file(tree.path("Solstone-win-Portable.zip")).unwrap();
    assert_eq!(
        tree.validate_release(),
        Err(ManifestError::MissingBundleEntry)
    );

    let tree = TempTree::good();
    fs::write(tree.path("unknown.txt"), b"unknown\n").unwrap();
    assert_eq!(
        tree.validate_release(),
        Err(ManifestError::UnknownBundleEntry)
    );

    let tree = TempTree::good();
    fs::write(tree.path("unlisted.exe"), b"inert\n").unwrap();
    assert_eq!(
        tree.validate_release(),
        Err(ManifestError::UnmanifestedRustOutput)
    );

    for name in ["unlisted.dll", "unlisted.pdb"] {
        let tree = TempTree::good();
        fs::write(tree.path(name), b"inert\n").unwrap();
        assert_eq!(
            tree.validate_release(),
            Err(ManifestError::UnmanifestedRustOutput),
            "{name}"
        );
    }

    let tree = TempTree::good();
    fs::write(tree.path("Solstone-0.2.10-full.nupkg"), b"historical\n").unwrap();
    assert_eq!(
        tree.validate_release(),
        Err(ManifestError::HistoricalArtifact)
    );

    let tree = TempTree::good();
    fs::copy(
        tree.manifest_path(),
        tree.path("extra.rust-release-manifest.json"),
    )
    .unwrap();
    assert_eq!(tree.validate_release(), Err(ManifestError::ExtraManifest));

    let tree = TempTree::good();
    fs::rename(
        tree.manifest_path(),
        tree.path("wrong.rust-release-manifest.json"),
    )
    .unwrap();
    assert_eq!(
        tree.validate_release(),
        Err(ManifestError::WrongManifestName)
    );

    let tree = TempTree::good();
    fs::create_dir(tree.path("work")).unwrap();
    assert_eq!(
        tree.validate_release(),
        Err(ManifestError::DirectoryNotFlat)
    );
}

#[cfg(unix)]
#[test]
fn rust_release_manifest_modes_reject_links_and_special_files() {
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;

    let tree = TempTree::good();
    let full = tree.path("Solstone-0.2.11-full.nupkg");
    fs::remove_file(&full).unwrap();
    symlink("Solstone-win-Portable.zip", &full).unwrap();
    assert!(matches!(
        tree.validate_manifest(),
        Err(ManifestError::NonRegularFile {
            kind: "symlink",
            ..
        })
    ));
    assert!(matches!(
        tree.validate_release(),
        Err(ManifestError::NonRegularFile {
            kind: "symlink",
            ..
        })
    ));

    let tree = TempTree::good();
    let socket = tree.path("unexpected.sock");
    let _listener = UnixListener::bind(&socket).unwrap();
    assert!(matches!(
        tree.validate_release(),
        Err(ManifestError::NonRegularFile { .. })
    ));
}

#[test]
fn rust_release_manifest_rejects_real_case_collisions_when_the_filesystem_preserves_them() {
    let tree = TempTree::good();
    let entry_count_before = fs::read_dir(&tree.root.0).unwrap().count();
    fs::write(tree.path("ASSETS.WIN.JSON"), b"[]\n").unwrap();
    let entry_count_after = fs::read_dir(&tree.root.0).unwrap().count();
    if entry_count_after > entry_count_before {
        assert!(matches!(
            tree.validate_release(),
            Err(ManifestError::CaseCollision { .. })
        ));
    }
}

#[cfg(unix)]
#[test]
fn rust_release_manifest_modes_reject_symlinked_roots_and_traversal_roots() {
    use std::os::unix::fs::symlink;

    let tree = TempTree::good();
    let links = TempDir::new("containment-links");
    let linked = links.0.join("candidate");
    symlink(&tree.root.0, &linked).unwrap();
    assert!(matches!(
        rust_release_manifest::validate_release_dir_with_facts(&linked, &tree.facts),
        Err(ManifestError::UnsafeResolution { .. })
    ));
    assert!(matches!(
        rust_release_manifest::validate_manifest_with_facts(
            &linked.join(companion_basename()),
            &tree.facts
        ),
        Err(ManifestError::UnsafeResolution { .. })
    ));

    let ancestor = TempDir::new("containment-ancestor");
    let real_parent = ancestor.0.join("real-parent");
    let candidate = real_parent.join("candidate");
    fs::create_dir(&real_parent).unwrap();
    fs::create_dir(&candidate).unwrap();
    copy_tree(&tree.root.0, &candidate);
    let linked_parent = ancestor.0.join("linked-parent");
    symlink(&real_parent, &linked_parent).unwrap();
    let candidate_through_link = linked_parent.join("candidate");
    assert!(matches!(
        rust_release_manifest::validate_release_dir_with_facts(
            &candidate_through_link,
            &tree.facts
        ),
        Err(ManifestError::UnsafeResolution { .. })
    ));
    assert!(matches!(
        rust_release_manifest::validate_manifest_with_facts(
            &candidate_through_link.join(companion_basename()),
            &tree.facts
        ),
        Err(ManifestError::UnsafeResolution { .. })
    ));

    let nested = tree.path("nested");
    fs::create_dir(&nested).unwrap();
    let traversal_root = nested.join("..");
    assert!(matches!(
        rust_release_manifest::validate_release_dir_with_facts(&traversal_root, &tree.facts),
        Err(ManifestError::UnsafeResolution { .. })
    ));
    assert!(matches!(
        rust_release_manifest::validate_manifest_with_facts(
            &traversal_root.join(companion_basename()),
            &tree.facts
        ),
        Err(ManifestError::UnsafeResolution { .. })
    ));
}

#[test]
fn rust_release_manifest_validates_historical_ledgers_without_historical_bytes() {
    let tree = TempTree::good();
    let mut feed: Value =
        serde_json::from_slice(&fs::read(tree.path("releases.win.json")).unwrap()).unwrap();
    feed["Assets"].as_array_mut().unwrap().insert(
        0,
        json!({
            "PackageId":"Solstone","Version":"0.2.10","Type":"Full",
            "FileName":"Solstone-0.2.10-full.nupkg",
            "SHA1":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "SHA256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "Size":12,"NotesMarkdown":"","NotesHTML":""
        }),
    );
    feed["Assets"].as_array_mut().unwrap().insert(
        1,
        json!({
            "PackageId":"Solstone","Version":"0.2.11-alpha.1","Type":"Full",
            "FileName":"Solstone-0.2.11-alpha.1-full.nupkg",
            "SHA1":"cccccccccccccccccccccccccccccccccccccccc",
            "SHA256":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
            "Size":13,"NotesMarkdown":"","NotesHTML":""
        }),
    );
    tree.write_json_artifact("releases.win.json", &feed);
    let releases = b"\xef\xbb\xbfaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa Solstone-0.2.10-full.nupkg 12\ncccccccccccccccccccccccccccccccccccccccc Solstone-0.2.11-alpha.1-full.nupkg 13\n99f8ce8a6760286f3eff6970c8161da2722aad45 Solstone-0.2.11-full.nupkg 25\n";
    fs::write(tree.path("RELEASES"), releases).unwrap();
    tree.sync_artifact("RELEASES");
    tree.validate_release().unwrap();
    assert!(!tree.path("Solstone-0.2.10-full.nupkg").exists());
}

#[test]
fn rust_release_manifest_rejects_ledger_semver_duplicates_and_newer_versions() {
    for version in ["not-semver", "0.2.12"] {
        let tree = TempTree::good();
        let mut feed: Value =
            serde_json::from_slice(&fs::read(tree.path("releases.win.json")).unwrap()).unwrap();
        feed["Assets"].as_array_mut().unwrap().insert(
            0,
            json!({
                "PackageId":"Solstone","Version":version,"Type":"Full",
                "FileName":format!("Solstone-{version}-full.nupkg"),
                "SHA1":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "SHA256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "Size":12,"NotesMarkdown":"","NotesHTML":""
            }),
        );
        tree.write_json_artifact("releases.win.json", &feed);
        let expected = if version == "not-semver" {
            ManifestError::LedgerVersionMalformed
        } else {
            ManifestError::LedgerVersionNewerThanCandidate
        };
        assert_eq!(tree.validate_release(), Err(expected));
    }

    let tree = TempTree::good();
    let mut feed: Value =
        serde_json::from_slice(&fs::read(tree.path("releases.win.json")).unwrap()).unwrap();
    let duplicate = feed["Assets"][0].clone();
    feed["Assets"].as_array_mut().unwrap().push(duplicate);
    tree.write_json_artifact("releases.win.json", &feed);
    assert_eq!(tree.validate_release(), Err(ManifestError::LedgerDuplicate));

    let tree = TempTree::good();
    let mut feed: Value =
        serde_json::from_slice(&fs::read(tree.path("releases.win.json")).unwrap()).unwrap();
    let mut conflict = feed["Assets"][0].clone();
    conflict["Size"] = json!(26);
    feed["Assets"].as_array_mut().unwrap().push(conflict);
    tree.write_json_artifact("releases.win.json", &feed);
    assert_eq!(tree.validate_release(), Err(ManifestError::LedgerConflict));
}

#[test]
fn rust_release_manifest_rejects_closed_ledger_and_current_metadata_drift() {
    assert_eq!(
        ledger_mutation("releases.win.json", |value| value["Assets"][0]
            ["PackageId"] =
            json!("other")),
        ManifestError::LedgerPackageIdMismatch
    );
    assert_eq!(
        ledger_mutation("releases.win.json", |value| value["Assets"][0]["Unknown"] =
            json!(true)),
        ManifestError::LedgerJsonMalformed
    );
    assert_eq!(
        ledger_mutation("releases.win.json", |value| {
            let record = value["Assets"][0].as_object_mut().unwrap();
            let notes = record.remove("NotesHTML").unwrap();
            record.insert("NotesHtml".to_owned(), notes);
        }),
        ManifestError::LedgerJsonMalformed
    );
    assert_eq!(
        ledger_mutation("assets.win.json", |value| value[0]["Unknown"] = json!(true)),
        ManifestError::LedgerJsonMalformed
    );
    assert_eq!(
        ledger_mutation("releases.win.json", |value| value["Assets"][0]["SHA1"] =
            json!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")),
        ManifestError::LedgerCurrentMismatch
    );
    assert_eq!(
        ledger_mutation("assets.win.json", |value| {
            value
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|entry| entry["Type"] == "Installer")
                .unwrap()["RelativeFileName"] = json!("Solstone-win-Setup.exe");
        }),
        ManifestError::AssetsDefaultSetupForbidden
    );
    assert_eq!(
        ledger_mutation("assets.win.json", |value| value
            .as_array_mut()
            .unwrap()
            .push(
                json!({"RelativeFileName":"Solstone-0.2.11-delta.nupkg","Type":"Delta"})
            )),
        ManifestError::DeltaMismatch
    );

    let tree = TempTree::good();
    let mut releases = fs::read(tree.path("RELEASES")).unwrap();
    releases[0] = b'X';
    fs::write(tree.path("RELEASES"), releases).unwrap();
    tree.sync_artifact("RELEASES");
    assert_eq!(
        tree.validate_release(),
        Err(ManifestError::ReleasesBomMissing)
    );
}

fn ledger_mutation<F>(name: &str, mutate: F) -> ManifestError
where
    F: FnOnce(&mut Value),
{
    let tree = TempTree::good();
    let mut value: Value = serde_json::from_slice(&fs::read(tree.path(name)).unwrap()).unwrap();
    mutate(&mut value);
    tree.write_json_artifact(name, &value);
    tree.validate_release()
        .expect_err("ledger mutation must fail")
}

#[test]
fn rust_release_manifest_renderer_rejects_noncanonical_public_evidence() {
    fn evidence() -> ReleaseEvidence {
        ReleaseEvidence::from(read_manifest(
            &fixture_root()
                .join("release-dir")
                .join(companion_basename()),
        ))
    }
    let cases: Vec<(&str, ManifestError, EvidenceMutation)> = vec![
        (
            "schema",
            ManifestError::EvidenceInvalid {
                field: "schema_version",
            },
            Box::new(|e| e.schema_version = 2),
        ),
        (
            "product",
            ManifestError::EvidenceInvalid { field: "product" },
            Box::new(|e| e.product = "other".to_owned()),
        ),
        (
            "dirty",
            ManifestError::EvidenceInvalid {
                field: "source_dirty",
            },
            Box::new(|e| e.source_dirty = true),
        ),
        (
            "gate",
            ManifestError::EvidenceInvalid {
                field: "dependency_policy.deterministic_gate",
            },
            Box::new(|e| e.dependency_policy.deterministic_gate = "fail".to_owned()),
        ),
        (
            "date",
            ManifestError::EvidenceInvalid { field: "schema" },
            Box::new(|e| e.dependency_policy.advisory_checked_at = "invalid".to_owned()),
        ),
        (
            "commit",
            ManifestError::EvidenceInvalid {
                field: "source_commit",
            },
            Box::new(|e| e.source_commit = "A".repeat(40)),
        ),
        (
            "lock",
            ManifestError::EvidenceInvalid {
                field: "cargo_lock_sha256",
            },
            Box::new(|e| e.cargo_lock_sha256 = "A".repeat(64)),
        ),
        (
            "packaged executable bytes",
            ManifestError::EvidenceInvalid {
                field: "packaged_executable.bytes",
            },
            Box::new(|e| e.packaged_executable.bytes = 0),
        ),
        (
            "packaged executable hash",
            ManifestError::EvidenceInvalid {
                field: "packaged_executable.sha256",
            },
            Box::new(|e| e.packaged_executable.sha256 = "A".repeat(64)),
        ),
        (
            "wrong feature",
            ManifestError::EvidenceInvalid {
                field: "target.features",
            },
            Box::new(|e| {
                if let TargetEvidence::Compiled { features, .. } = &mut e.target {
                    *features = vec!["other".to_owned()];
                }
            }),
        ),
        (
            "extra feature",
            ManifestError::EvidenceInvalid {
                field: "target.features",
            },
            Box::new(|e| {
                if let TargetEvidence::Compiled { features, .. } = &mut e.target {
                    features.push("other".to_owned());
                }
            }),
        ),
        (
            "unsorted features",
            ManifestError::EvidenceInvalid {
                field: "target.features",
            },
            Box::new(|e| {
                if let TargetEvidence::Compiled { features, .. } = &mut e.target {
                    *features = vec!["z".to_owned(), "a".to_owned()];
                }
            }),
        ),
        (
            "triple",
            ManifestError::EvidenceInvalid {
                field: "target.triple",
            },
            Box::new(|e| {
                if let TargetEvidence::Compiled { triple, .. } = &mut e.target {
                    *triple = "other-target".to_owned();
                }
            }),
        ),
        (
            "profile",
            ManifestError::EvidenceInvalid {
                field: "target.profile",
            },
            Box::new(|e| {
                if let TargetEvidence::Compiled { profile, .. } = &mut e.target {
                    *profile = "debug".to_owned();
                }
            }),
        ),
        (
            "source target",
            ManifestError::EvidenceInvalid {
                field: "target.kind",
            },
            Box::new(|e| e.target = TargetEvidence::Source),
        ),
        (
            "exceptions",
            ManifestError::EvidenceNotCanonical {
                field: "active_exceptions",
            },
            Box::new(|e| e.active_exceptions.reverse()),
        ),
        (
            "path",
            ManifestError::EvidenceInvalid {
                field: "artifacts.path",
            },
            Box::new(|e| e.artifacts[0].path = "nested/file".to_owned()),
        ),
        (
            "bytes",
            ManifestError::EvidenceInvalid {
                field: "artifacts.bytes",
            },
            Box::new(|e| e.artifacts[0].bytes = 0),
        ),
        (
            "hash",
            ManifestError::EvidenceInvalid {
                field: "artifacts.sha256",
            },
            Box::new(|e| e.artifacts[0].sha256 = "A".repeat(64)),
        ),
        (
            "order",
            ManifestError::EvidenceNotCanonical { field: "artifacts" },
            Box::new(|e| e.artifacts.swap(0, 1)),
        ),
        (
            "signing",
            ManifestError::EvidenceInvalid {
                field: "native_tools.signing_mode",
            },
            Box::new(|e| {
                e.native_tools
                    .insert("signing_mode".to_owned(), "signed".to_owned())
                    .map(|_| ())
                    .unwrap()
            }),
        ),
        (
            "tools",
            ManifestError::EvidenceInvalid {
                field: "native_tools",
            },
            Box::new(|e| {
                e.native_tools.insert("extra".to_owned(), "1".to_owned());
            }),
        ),
    ];
    for (label, expected, mutate) in cases {
        let mut value = evidence();
        mutate(&mut value);
        assert_eq!(
            rust_release_manifest::render_release_evidence(&value),
            Err(expected),
            "{label}"
        );
    }

    let mut duplicate = evidence();
    duplicate.artifacts[1].path = duplicate.artifacts[0].path.clone();
    assert_eq!(
        rust_release_manifest::render_release_evidence(&duplicate),
        Err(ManifestError::EvidenceNotCanonical {
            field: "artifacts.path"
        })
    );

    let mut case_collision = evidence();
    case_collision.artifacts[0].path = "Foo.exe".to_owned();
    case_collision.artifacts[1].path = "foo.exe".to_owned();
    case_collision
        .artifacts
        .sort_by(|first, second| first.path.cmp(&second.path));
    assert_eq!(
        rust_release_manifest::render_release_evidence(&case_collision),
        Err(ManifestError::CaseCollision {
            first: "Foo.exe".to_owned(),
            second: "foo.exe".to_owned()
        })
    );

    let mut unsigned_with_signed_key = evidence();
    unsigned_with_signed_key
        .native_tools
        .insert("smctl".to_owned(), "1".to_owned());
    assert_eq!(
        rust_release_manifest::render_release_evidence(&unsigned_with_signed_key),
        Err(ManifestError::EvidenceInvalid {
            field: "native_tools"
        })
    );
    let mut incomplete_signed = evidence();
    incomplete_signed
        .native_tools
        .insert("signing_mode".to_owned(), "signed-verified".to_owned());
    assert_eq!(
        rust_release_manifest::render_release_evidence(&incomplete_signed),
        Err(ManifestError::EvidenceInvalid {
            field: "native_tools"
        })
    );
}

#[test]
fn rust_release_manifest_renderer_is_cross_root_deterministic() {
    let evidence = ReleaseEvidence::from(read_manifest(
        &fixture_root()
            .join("release-dir")
            .join(companion_basename()),
    ));
    let first_root = TempDir::new("render-one");
    let second_root = TempDir::new("render-two");
    let first = rust_release_manifest::render_release_evidence(&evidence).unwrap();
    let second = rust_release_manifest::render_release_evidence(&evidence).unwrap();
    fs::write(first_root.0.join("manifest.json"), &first).unwrap();
    fs::write(second_root.0.join("manifest.json"), &second).unwrap();
    assert_eq!(first, second);
    let round_trip = rust_release_manifest::validate_manifest_bytes(&first).unwrap();
    assert_eq!(ReleaseEvidence::from(round_trip), evidence);
    assert!(first.ends_with(b"\n"));
    assert!(!first.ends_with(b"\n\n"));
}

#[test]
fn rust_release_manifest_self_check_binds_exceptions_to_deny_toml() {
    let root = TempDir::new("self-check");
    fs::create_dir_all(root.0.join("schemas/rust-release-manifest")).unwrap();
    fs::create_dir_all(root.0.join("packaging")).unwrap();
    fs::create_dir_all(root.0.join("xtask/tests/fixtures/rust-release-manifest")).unwrap();
    fs::copy(
        repo_root().join("schemas/rust-release-manifest/v1.json"),
        root.0.join("schemas/rust-release-manifest/v1.json"),
    )
    .unwrap();
    fs::copy(
        repo_root().join("packaging/release-toolchain.json"),
        root.0.join("packaging/release-toolchain.json"),
    )
    .unwrap();
    fs::copy(repo_root().join("deny.toml"), root.0.join("deny.toml")).unwrap();
    copy_tree(
        &fixture_root(),
        &root.0.join("xtask/tests/fixtures/rust-release-manifest"),
    );
    rust_release_manifest::run_self_check(&root.0).unwrap();

    let manifest_path = root
        .0
        .join("xtask/tests/fixtures/rust-release-manifest/release-dir")
        .join(companion_basename());
    let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["active_exceptions"].as_array_mut().unwrap().pop();
    fs::write(manifest_path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    assert_eq!(
        rust_release_manifest::run_self_check(&root.0),
        Err(ManifestError::ActiveExceptionsMismatch)
    );
}
