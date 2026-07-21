// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use xtask::rust_release_manifest::{
    self, CheckoutFacts, ClassificationMode, Manifest, PRODUCT, TARGET_FEATURES, TARGET_PROFILE,
    TARGET_TRIPLE,
};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct WorkingDirectory {
    original: PathBuf,
}

impl WorkingDirectory {
    fn enter(path: &Path) -> Self {
        let original = std::env::current_dir().expect("read working directory");
        std::env::set_current_dir(path).expect("enter isolated working directory");
        Self { original }
    }
}

impl Drop for WorkingDirectory {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.original).expect("restore working directory");
    }
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "solstone-rust-release-manifest-bare-{}-{}",
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

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a workspace parent")
        .to_path_buf()
}

fn copy_tree(source: &Path, destination: &Path) {
    for entry in fs::read_dir(source).expect("read fixture tree") {
        let entry = entry.expect("read fixture entry");
        let target = destination.join(entry.file_name());
        if entry.file_type().expect("read fixture type").is_dir() {
            fs::create_dir(&target).expect("create fixture directory");
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy fixture file");
        }
    }
}

#[test]
fn rust_release_manifest_mode_accepts_a_bare_filename_candidate() {
    let root = repo_root();
    let temp = TempDir::new();
    copy_tree(
        &root.join("xtask/tests/fixtures/rust-release-manifest/manifest-mode"),
        &temp.0,
    );
    let manifest: Manifest = rust_release_manifest::validate_manifest_bytes(
        &fs::read(temp.0.join("manifest.json")).expect("read fixture manifest"),
    )
    .expect("parse fixture manifest");
    let projection =
        rust_release_manifest::project_release_toolchain(&root).expect("project release toolchain");
    let facts = CheckoutFacts {
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
        active_exceptions: rust_release_manifest::read_active_exceptions(&root)
            .expect("read advisory exceptions"),
        unsigned_native_tools: projection.unsigned_native_tools,
        signed_native_tools: projection.signed_native_tools,
    };
    let _working_directory = WorkingDirectory::enter(&temp.0);
    let report =
        rust_release_manifest::validate_manifest_with_facts(Path::new("manifest.json"), &facts)
            .expect("bare manifest path must validate");
    assert_eq!(report.mode, ClassificationMode::SiblingBytesOnly);
}
