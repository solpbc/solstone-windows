// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use xtask::artifact_fs::{self, ArtifactFsError, ContainedRoot, UnixModePolicy, UnsafePathReason};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "solstone-artifact-fs-{}-{}",
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

#[test]
fn artifact_fs_accepts_release_filename_charset() {
    for path in [
        "assets.win.json",
        "RELEASES",
        "releases.win.json",
        "Solstone-0.2.11-full.nupkg",
        "Solstone-0.2.11-delta.nupkg",
        "solstone-setup-0.2.11.exe",
        "Solstone-win-Portable.zip",
        "solstone-windows-x86_64-pc-windows-msvc.rust-release-manifest.json",
    ] {
        artifact_fs::validate_relative_path(path).expect("release filename is portable");
    }
}

#[test]
fn artifact_fs_rejects_every_path_safety_class() {
    assert!(matches!(
        artifact_fs::validate_relative_path(""),
        Err(ArtifactFsError::UnsafePath {
            reason: UnsafePathReason::Empty,
            ..
        })
    ));
    assert!(matches!(
        artifact_fs::validate_relative_path("/absolute"),
        Err(ArtifactFsError::UnsafePath {
            reason: UnsafePathReason::Absolute,
            ..
        })
    ));
    assert!(matches!(
        artifact_fs::validate_relative_path("../escape"),
        Err(ArtifactFsError::Traversal { .. })
    ));
    assert!(matches!(
        artifact_fs::validate_relative_path("bad\\name"),
        Err(ArtifactFsError::Backslash { .. })
    ));
    assert!(matches!(
        artifact_fs::validate_relative_path("bad\nname"),
        Err(ArtifactFsError::ControlChar { .. })
    ));
}

#[test]
fn artifact_fs_rejects_declared_case_collisions() {
    let mut folded = BTreeMap::new();
    artifact_fs::check_case_collision(&mut folded, "asset.json").unwrap();
    assert!(matches!(
        artifact_fs::check_case_collision(&mut folded, "ASSET.JSON"),
        Err(ArtifactFsError::CaseCollision { .. })
    ));
}

#[test]
fn artifact_fs_walk_rejects_case_collisions_when_the_filesystem_preserves_them() {
    let root = TempDir::new();
    fs::write(root.0.join("asset.json"), b"one").unwrap();
    fs::write(root.0.join("ASSET.JSON"), b"two").unwrap();
    let entry_count = fs::read_dir(&root.0)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .len();
    if entry_count < 2 {
        return;
    }
    assert!(matches!(
        artifact_fs::walk_directory(&root.0, "root", UnixModePolicy::AllowExecute),
        Err(ArtifactFsError::CaseCollision { .. })
    ));
}

#[cfg(unix)]
#[test]
fn artifact_fs_parameterizes_only_the_execute_policy() {
    use std::os::unix::fs::PermissionsExt;

    let root = TempDir::new();
    let file = root.0.join("setup.exe");
    fs::write(&file, b"inert").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        artifact_fs::verify_regular_file(&file, "setup.exe", UnixModePolicy::StrictNoExecute),
        Err(ArtifactFsError::InvalidFileMode { .. })
    ));
    artifact_fs::verify_regular_file(&file, "setup.exe", UnixModePolicy::AllowExecute)
        .expect("release artifacts may carry execute bits");
}

#[cfg(unix)]
#[test]
fn artifact_fs_containment_rejects_symlinked_parent_and_leaf() {
    use std::os::unix::fs::symlink;

    let outer = TempDir::new();
    let real = outer.0.join("real");
    fs::create_dir(&real).unwrap();
    fs::write(real.join("artifact.bin"), b"inert").unwrap();
    let linked = outer.0.join("linked");
    symlink(&real, &linked).unwrap();
    assert!(matches!(
        ContainedRoot::new(&linked, "candidate", UnixModePolicy::AllowExecute),
        Err(ArtifactFsError::UnsafeResolution { .. })
    ));

    let root = ContainedRoot::new(&real, "candidate", UnixModePolicy::AllowExecute).unwrap();
    fs::write(outer.0.join("outside.bin"), b"outside").unwrap();
    symlink(outer.0.join("outside.bin"), real.join("linked.bin")).unwrap();
    assert!(matches!(
        root.read("linked.bin", "linked.bin"),
        Err(ArtifactFsError::NonRegularFile {
            kind: "symlink",
            ..
        })
    ));
}

#[test]
fn artifact_fs_containment_rejects_traversal_escape() {
    let outer = TempDir::new();
    let root_path = outer.0.join("root");
    fs::create_dir(&root_path).unwrap();
    fs::write(outer.0.join("outside.bin"), b"outside").unwrap();
    let root = ContainedRoot::new(&root_path, "candidate", UnixModePolicy::AllowExecute).unwrap();
    assert!(matches!(
        root.read("../outside.bin", "artifact"),
        Err(ArtifactFsError::UnsafeResolution { .. })
    ));
}

#[test]
fn artifact_fs_accepts_a_byte_stable_pre_read_replacement() {
    let root = TempDir::new();
    let file = root.0.join("artifact.bin");
    fs::write(&file, b"first").unwrap();
    let resolver = ContainedRoot::new(&root.0, "candidate", UnixModePolicy::AllowExecute).unwrap();
    let resolved = resolver.resolve("artifact.bin", "artifact.bin").unwrap();
    fs::rename(&file, root.0.join("original.bin")).unwrap();
    fs::write(&file, b"other").unwrap();
    assert_eq!(resolved.read().unwrap(), b"other");
}
