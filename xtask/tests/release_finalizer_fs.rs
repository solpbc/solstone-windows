// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use xtask::release_finalizer_fs::{
    create_candidate_temp, DeletionPlan, ReleaseCleanupCatalog, ReleaseFinalizerFsError,
};
use xtask::rust_release_manifest::companion_basename;

const VERSION: &str = "0.2.11";
const OLDER_FULL: &str = "Solstone-0.2.10-full.nupkg";
const CANDIDATE_TEMP: &str =
    "target/release-candidate/.0.2.11.finalize-0123456789abcdef0123456789abcdef.tmp";

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TempTree {
    root: PathBuf,
}

impl TempTree {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "solstone-release-finalizer-fs-{label}-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("create isolated test checkout");
        Self { root }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    fn mkdir(&self, relative: &str) {
        fs::create_dir_all(self.path(relative)).expect("create test directory");
    }

    fn write(&self, relative: &str, bytes: &[u8]) {
        let path = self.path(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create test file parent");
        }
        fs::write(path, bytes).expect("write test file");
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove isolated test tree");
    }
}

fn current_release_files() -> Vec<String> {
    vec![
        "Releases/RELEASES".to_owned(),
        "Releases/Solstone-0.2.11-delta.nupkg".to_owned(),
        "Releases/Solstone-0.2.11-full.nupkg".to_owned(),
        "Releases/Solstone-win-Portable.zip".to_owned(),
        "Releases/Solstone-win-Setup.exe".to_owned(),
        "Releases/assets.win.json".to_owned(),
        "Releases/releases.win.json".to_owned(),
        "Releases/solstone-setup-0.2.11.exe".to_owned(),
        format!("Releases/{}", companion_basename()),
    ]
}

fn catalog_file_targets() -> Vec<String> {
    let mut paths = current_release_files();
    paths.extend([
        "target/release-evidence/0.2.11/.rust-release-finalization.json.tmp".to_owned(),
        format!("target/release-manifest/0.2.11/{}", companion_basename()),
        "target/release-notes-0.2.11.md".to_owned(),
    ]);
    paths.sort();
    paths
}

fn catalog_directory_targets() -> Vec<String> {
    vec![
        CANDIDATE_TEMP.to_owned(),
        "target/release-candidate/0.2.11".to_owned(),
        "target/release-finalizer/0.2.11".to_owned(),
        "target/vpk-stage".to_owned(),
    ]
}

fn catalog_confinement_leaves() -> Vec<String> {
    let mut paths = catalog_file_targets();
    paths.extend([
        "target/release-evidence/0.2.11/rust-release-finalization.json".to_owned(),
        "target/release-evidence/0.2.11/windows-native-proof.json".to_owned(),
    ]);
    paths
}

fn catalog_intermediate_ancestors() -> Vec<String> {
    vec![
        "Releases".to_owned(),
        "target".to_owned(),
        "target/release-candidate".to_owned(),
        "target/release-evidence".to_owned(),
        "target/release-evidence/0.2.11".to_owned(),
        "target/release-finalizer".to_owned(),
        "target/release-manifest".to_owned(),
        "target/release-manifest/0.2.11".to_owned(),
    ]
}

fn populate_catalog(tree: &TempTree) {
    for path in current_release_files() {
        tree.write(&path, format!("bytes for {path}").as_bytes());
    }
    tree.write("target/release-notes-0.2.11.md", b"release notes");
    tree.write(
        "target/release-evidence/0.2.11/.rust-release-finalization.json.tmp",
        b"incomplete receipt",
    );
    tree.write(
        &format!("target/release-manifest/0.2.11/{}", companion_basename()),
        b"legacy manifest",
    );
    for path in catalog_directory_targets() {
        tree.write(&format!("{path}/marker.bin"), path.as_bytes());
    }
    tree.write(
        "target/release-evidence/0.2.11/rust-release-finalization.json",
        b"prior final receipt stays until engine promotion",
    );
}

#[test]
fn catalog_materializes_only_the_enumerated_version_namespaces() {
    let tree = TempTree::new("catalog");
    populate_catalog(&tree);
    tree.mkdir("target/release-candidate/.0.2.12.finalize-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.tmp");
    tree.mkdir("target/release-candidate/.0.2.11.finalize-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA.tmp");
    tree.write(
        &format!("target/release-manifest/0.2.12/{}", companion_basename()),
        b"different version",
    );

    let catalog = ReleaseCleanupCatalog::for_version(&tree.root, VERSION).expect("build catalog");
    let mut expected = catalog_file_targets();
    expected.extend(catalog_directory_targets());
    expected.sort();

    assert_eq!(catalog.deletable_paths(), expected);
    assert_eq!(
        catalog.finalization_receipt_path(),
        "target/release-evidence/0.2.11/rust-release-finalization.json"
    );
    assert_eq!(
        catalog.native_proof_path(),
        "target/release-evidence/0.2.11/windows-native-proof.json"
    );
    assert!(catalog
        .deletable_paths()
        .iter()
        .all(|path| !path.contains("0.2.12") && !path.contains("AAAAAAAA")));
}

#[test]
fn delta_base_allowlist_retains_one_explicit_older_full() {
    let tree = TempTree::new("delta-retain");
    tree.write(&format!("Releases/{OLDER_FULL}"), b"retained full");
    tree.write(
        "Releases/Solstone-0.2.9-full.nupkg",
        b"unselected historical full",
    );
    tree.write("Releases/Solstone-0.2.9-delta.nupkg", b"historical delta");

    let catalog = ReleaseCleanupCatalog::for_version(&tree.root, VERSION).expect("build catalog");
    let plan = DeletionPlan::materialize(&catalog, &[OLDER_FULL.to_owned()])
        .expect("accept explicit older full");

    assert_eq!(plan.retained_delta_bases(), &[OLDER_FULL.to_owned()]);
    assert!(!plan.paths().contains(&format!("Releases/{OLDER_FULL}")));
    assert!(plan
        .paths()
        .contains(&"Releases/Solstone-0.2.9-full.nupkg".to_owned()));
    assert!(plan
        .paths()
        .contains(&"Releases/Solstone-0.2.9-delta.nupkg".to_owned()));
    plan.execute().expect("execute explicit cleanup plan");
    assert_eq!(
        fs::read(tree.path(&format!("Releases/{OLDER_FULL}"))).expect("read retained full"),
        b"retained full"
    );
    assert!(!tree.path("Releases/Solstone-0.2.9-full.nupkg").exists());
    assert!(!tree.path("Releases/Solstone-0.2.9-delta.nupkg").exists());
}

#[test]
fn cleanup_removes_only_planned_members_and_preserves_the_final_receipt() {
    let tree = TempTree::new("execute");
    populate_catalog(&tree);
    let catalog = ReleaseCleanupCatalog::for_version(&tree.root, VERSION).expect("build catalog");
    let protected_receipt = catalog.finalization_receipt_path().to_owned();
    let expected_removed = catalog.deletable_paths();
    let plan = DeletionPlan::materialize(&catalog, &[]).expect("materialize cleanup");

    plan.execute().expect("execute cleanup");

    for relative in expected_removed {
        assert!(!tree.path(&relative).exists(), "{relative} was not removed");
    }
    assert_eq!(
        fs::read(tree.path(&protected_receipt)).expect("read protected prior receipt"),
        b"prior final receipt stays until engine promotion"
    );
}

#[test]
fn delta_base_allowlist_rejects_duplicates_nonolder_noncanonical_missing_and_nonregular() {
    let tree = TempTree::new("delta-refusals");
    tree.write(&format!("Releases/{OLDER_FULL}"), b"retained full");
    let catalog = ReleaseCleanupCatalog::for_version(&tree.root, VERSION).expect("build catalog");

    assert_eq!(
        DeletionPlan::materialize(&catalog, &[OLDER_FULL.to_owned(), OLDER_FULL.to_owned()])
            .expect_err("duplicate must fail"),
        ReleaseFinalizerFsError::DuplicateDeltaBase
    );
    assert_eq!(
        DeletionPlan::materialize(&catalog, &["Solstone-0.2.11-full.nupkg".to_owned()])
            .expect_err("current version must fail"),
        ReleaseFinalizerFsError::DeltaBaseNotOlder
    );
    assert_eq!(
        DeletionPlan::materialize(&catalog, &["Solstone-00.2.10-full.nupkg".to_owned()])
            .expect_err("noncanonical name must fail"),
        ReleaseFinalizerFsError::InvalidDeltaBase
    );
    assert_eq!(
        DeletionPlan::materialize(&catalog, &["Solstone-0.2.8-full.nupkg".to_owned()])
            .expect_err("missing base must fail"),
        ReleaseFinalizerFsError::DeltaBaseMissing
    );

    let nonregular = TempTree::new("delta-nonregular");
    nonregular.mkdir(&format!("Releases/{OLDER_FULL}"));
    let catalog = ReleaseCleanupCatalog::for_version(&nonregular.root, VERSION)
        .expect("build nonregular catalog");
    assert_eq!(
        DeletionPlan::materialize(&catalog, &[OLDER_FULL.to_owned()])
            .expect_err("directory base must fail"),
        ReleaseFinalizerFsError::DeltaBaseNotRegular
    );
}

#[test]
fn unknown_releases_entry_refuses_the_whole_cleanup() {
    let tree = TempTree::new("unknown-release");
    tree.write("Releases/operator-notes.txt", b"unknown");
    tree.write("target/vpk-stage/must-stay.bin", b"must stay");

    let error = ReleaseCleanupCatalog::for_version(&tree.root, VERSION)
        .expect_err("unknown entry must fail");

    assert_eq!(error, ReleaseFinalizerFsError::UnknownReleasesEntry);
    assert_eq!(
        fs::read(tree.path("target/vpk-stage/must-stay.bin")).expect("read untouched marker"),
        b"must stay"
    );
}

#[cfg(unix)]
#[test]
fn symlink_at_every_catalog_leaf_refuses_before_deletion() {
    for relative in catalog_confinement_leaves() {
        assert_symlink_refusal(&relative, false);
    }
}

#[cfg(unix)]
#[test]
fn symlink_at_every_catalog_root_refuses_before_deletion() {
    for relative in catalog_directory_targets() {
        assert_symlink_refusal(&relative, true);
    }
}

#[cfg(unix)]
#[test]
fn symlink_at_every_intermediate_ancestor_refuses_before_deletion() {
    for relative in catalog_intermediate_ancestors() {
        assert_symlink_refusal(&relative, true);
    }
}

#[cfg(unix)]
fn assert_symlink_refusal(relative: &str, directory: bool) {
    use std::os::unix::fs::symlink;

    let label = relative.replace('/', "-");
    let tree = TempTree::new(&format!("symlink-{label}"));
    let outside = TempTree::new(&format!("outside-{label}"));
    populate_catalog(&tree);
    outside.write("sentinel.bin", b"external sentinel bytes");
    outside.mkdir("linked-directory");
    outside.write("linked-directory/sentinel.bin", b"external sentinel bytes");

    tree.mkdir("test-parked");
    if tree.path(relative).exists() {
        fs::rename(tree.path(relative), tree.path("test-parked/original"))
            .expect("park original catalog member");
    }
    let external_target = if directory {
        outside.path("linked-directory")
    } else {
        outside.path("sentinel.bin")
    };
    symlink(external_target, tree.path(relative)).expect("plant outside symlink");
    let before = snapshot(&tree.root);

    let result = ReleaseCleanupCatalog::for_version(&tree.root, VERSION).and_then(|catalog| {
        DeletionPlan::materialize(&catalog, &[]).and_then(DeletionPlan::execute)
    });

    assert!(result.is_err(), "symlink at {relative} must refuse cleanup");
    assert_eq!(snapshot(&tree.root), before, "tree changed for {relative}");
    assert_eq!(
        fs::read(outside.path("sentinel.bin")).expect("read outside sentinel"),
        b"external sentinel bytes",
        "outside sentinel changed for {relative}"
    );
    assert_eq!(
        fs::read(outside.path("linked-directory/sentinel.bin"))
            .expect("read outside directory sentinel"),
        b"external sentinel bytes",
        "outside directory changed for {relative}"
    );
}

#[cfg(unix)]
#[test]
fn changed_member_is_rejected_by_the_immediate_premutation_recheck() {
    use std::os::unix::fs::symlink;

    let tree = TempTree::new("recheck-swap");
    let outside = TempTree::new("recheck-outside");
    populate_catalog(&tree);
    outside.write("sentinel.bin", b"external sentinel bytes");
    let catalog = ReleaseCleanupCatalog::for_version(&tree.root, VERSION).expect("build catalog");
    let plan = DeletionPlan::materialize(&catalog, &[]).expect("materialize safe plan");

    fs::rename(
        tree.path("Releases/assets.win.json"),
        tree.path("test-parked-assets.win.json"),
    )
    .expect("park planned file");
    symlink(
        outside.path("sentinel.bin"),
        tree.path("Releases/assets.win.json"),
    )
    .expect("swap planned file for symlink");
    let before = snapshot(&tree.root);

    assert!(plan.execute().is_err(), "changed plan must be refused");
    assert_eq!(snapshot(&tree.root), before);
    assert_eq!(
        fs::read(outside.path("sentinel.bin")).expect("read outside sentinel"),
        b"external sentinel bytes"
    );
}

#[cfg(not(windows))]
#[test]
fn case_fold_collision_in_a_catalog_tree_refuses_cleanup() {
    let tree = TempTree::new("case-collision");
    tree.write("target/vpk-stage/Artifact.bin", b"first");
    tree.write("target/vpk-stage/artifact.bin", b"second");
    let catalog = ReleaseCleanupCatalog::for_version(&tree.root, VERSION).expect("build catalog");

    assert!(DeletionPlan::materialize(&catalog, &[]).is_err());
    assert_eq!(
        fs::read(tree.path("target/vpk-stage/Artifact.bin")).expect("read first"),
        b"first"
    );
    assert_eq!(
        fs::read(tree.path("target/vpk-stage/artifact.bin")).expect("read second"),
        b"second"
    );
}

#[cfg(unix)]
#[test]
fn special_file_in_a_catalog_tree_refuses_cleanup() {
    use std::os::unix::net::UnixListener;

    let tree = TempTree::new("special-file");
    tree.mkdir("target/vpk-stage");
    let socket = UnixListener::bind(tree.path("target/vpk-stage/control.sock"))
        .expect("create special-file witness");
    let catalog = ReleaseCleanupCatalog::for_version(&tree.root, VERSION).expect("build catalog");

    assert!(DeletionPlan::materialize(&catalog, &[]).is_err());
    assert!(tree.path("target/vpk-stage/control.sock").exists());
    drop(socket);
}

#[test]
fn native_proof_refuses_same_version_refinalization() {
    let tree = TempTree::new("native-proof");
    tree.write(
        "target/release-evidence/0.2.11/windows-native-proof.json",
        b"proof",
    );

    assert_eq!(
        ReleaseCleanupCatalog::for_version(&tree.root, VERSION)
            .expect_err("proof must refuse refinalization"),
        ReleaseFinalizerFsError::NativeProofExists
    );
    assert_eq!(
        fs::read(tree.path("target/release-evidence/0.2.11/windows-native-proof.json"))
            .expect("proof remains"),
        b"proof"
    );
}

#[test]
fn candidate_temp_is_new_empty_and_promotes_atomically() {
    let tree = TempTree::new("promote");

    let temp = create_candidate_temp(&tree.root, VERSION).expect("create candidate temp");
    assert!(temp.path().is_dir());
    assert!(fs::read_dir(temp.path())
        .expect("read candidate temp")
        .next()
        .is_none());
    let basename = Path::new(temp.relative_path())
        .file_name()
        .and_then(|name| name.to_str())
        .expect("portable temp basename");
    assert!(basename.starts_with(".0.2.11.finalize-"));
    assert!(basename.ends_with(".tmp"));
    let nonce = basename
        .strip_prefix(".0.2.11.finalize-")
        .and_then(|rest| rest.strip_suffix(".tmp"))
        .expect("extract nonce");
    assert_eq!(nonce.len(), 32);
    assert!(nonce
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));

    fs::write(temp.path().join("assembled.bin"), b"assembled").expect("assemble candidate member");
    let final_path = temp.promote().expect("atomically promote candidate");

    assert_eq!(final_path, tree.path("target/release-candidate/0.2.11"));
    assert_eq!(
        fs::read(final_path.join("assembled.bin")).expect("read promoted member"),
        b"assembled"
    );
}

#[test]
fn candidate_promotion_refuses_any_existing_final_target() {
    let tree = TempTree::new("promotion-existing");
    let temp = create_candidate_temp(&tree.root, VERSION).expect("create candidate temp");
    tree.mkdir("target/release-candidate/0.2.11");

    assert_eq!(
        temp.promote().expect_err("existing final must fail"),
        ReleaseFinalizerFsError::PromotionTargetExists
    );
}

#[test]
fn refusal_diagnostics_do_not_echo_absolute_or_unknown_private_data() {
    let tree = TempTree::new("private-machine-account-canary");
    let private_name = "operator-private-canary.txt";
    tree.write(&format!("Releases/{private_name}"), b"private");
    let diagnostic = ReleaseCleanupCatalog::for_version(&tree.root, VERSION)
        .expect_err("unknown file must fail")
        .to_string();

    assert!(!diagnostic.contains(&tree.root.display().to_string()));
    assert!(!diagnostic.contains(private_name));
    assert!(diagnostic.contains("Releases"));
    assert!(diagnostic.contains("move"));
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SnapshotEntry {
    Directory,
    File(Vec<u8>),
    Symlink(PathBuf),
    Special,
}

fn snapshot(root: &Path) -> BTreeMap<String, SnapshotEntry> {
    let mut entries = BTreeMap::new();
    snapshot_directory(root, "", &mut entries);
    entries
}

fn snapshot_directory(root: &Path, relative: &str, entries: &mut BTreeMap<String, SnapshotEntry>) {
    let directory = if relative.is_empty() {
        root.to_path_buf()
    } else {
        root.join(relative)
    };
    let mut children: Vec<_> = fs::read_dir(directory)
        .expect("read snapshot directory")
        .map(|entry| entry.expect("read snapshot entry"))
        .collect();
    children.sort_by_key(|entry| entry.file_name());
    for child in children {
        let name = child.file_name().into_string().expect("portable test name");
        let child_relative = if relative.is_empty() {
            name
        } else {
            format!("{relative}/{name}")
        };
        let metadata = fs::symlink_metadata(child.path()).expect("read snapshot metadata");
        if metadata.file_type().is_dir() {
            entries.insert(child_relative.clone(), SnapshotEntry::Directory);
            snapshot_directory(root, &child_relative, entries);
        } else if metadata.file_type().is_file() {
            entries.insert(
                child_relative,
                SnapshotEntry::File(fs::read(child.path()).expect("read snapshot file")),
            );
        } else if metadata.file_type().is_symlink() {
            entries.insert(
                child_relative,
                SnapshotEntry::Symlink(fs::read_link(child.path()).expect("read snapshot symlink")),
            );
        } else {
            entries.insert(child_relative, SnapshotEntry::Special);
        }
    }
}

// A live Windows junction/reparse mutation is exercised post-ship on the box.
// artifact_fs's synthetic FILE_ATTRIBUTE_REPARSE_POINT test keeps that unit seam host-testable.
