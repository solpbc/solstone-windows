// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::fs;
use std::path::PathBuf;

use xtask::transparency_stage::{
    render_staging_manifest_v1, verify_staging_manifest_v1, TransparencyStageError,
};

const MANIFEST_FIXTURE: &[u8] = include_bytes!("fixtures/transparency/stage-manifest-v1.txt");
const MANIFEST_SHA256: &str = "b0439a7a92f344bbcce83e2f2b2533be5684f02f400c13e4a17029fc28f468e7";

#[test]
fn transparency_staging_manifest_v1_matches_the_shared_byte_fixture() {
    let root = temporary_root("manifest");
    fs::create_dir_all(root.join("z-last")).expect("create nested archive directory");
    fs::write(root.join("z-last/file.bin"), b"omega\n").expect("write later file first");
    fs::write(root.join("a-first.txt"), b"alpha\n").expect("write earlier file second");

    let manifest = render_staging_manifest_v1(&root).expect("render staging manifest");
    assert_eq!(manifest.bytes, MANIFEST_FIXTURE);
    assert_eq!(manifest.sha256, MANIFEST_SHA256);
    assert_eq!(manifest.bytes.len(), 198);
    assert_eq!(
        verify_staging_manifest_v1(&root, MANIFEST_FIXTURE),
        Ok(manifest)
    );
}

#[cfg(unix)]
#[test]
fn transparency_staging_manifest_rejects_symbolic_links() {
    use std::os::unix::fs::symlink;

    let root = temporary_root("symlink");
    fs::write(root.join("target"), b"bytes").expect("write symlink target");
    symlink(root.join("target"), root.join("alias")).expect("create symbolic link");
    assert_eq!(
        render_staging_manifest_v1(&root),
        Err(TransparencyStageError::LinkRejected)
    );
}

#[test]
fn transparency_staging_manifest_rejects_non_ascii_paths() {
    let root = temporary_root("non-ascii");
    fs::write(root.join("café"), b"bytes").expect("write non-ASCII path");
    assert_eq!(
        render_staging_manifest_v1(&root),
        Err(TransparencyStageError::InvalidRelativePath)
    );
}

#[cfg(unix)]
#[test]
fn transparency_staging_manifest_rejects_control_character_paths() {
    let root = temporary_root("control");
    fs::write(root.join("line\nbreak"), b"bytes").expect("write control-character path");
    assert_eq!(
        render_staging_manifest_v1(&root),
        Err(TransparencyStageError::InvalidRelativePath)
    );
}

#[test]
fn transparency_staged_retry_requires_the_persisted_bytes_to_match() {
    let root = temporary_root("retry-mismatch");
    fs::write(root.join("file"), b"bytes").expect("write archive file");
    assert_eq!(
        verify_staging_manifest_v1(&root, b"different\n"),
        Err(TransparencyStageError::RetryRecordMismatch)
    );
}

fn temporary_root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "solstone-transparency-stage-{label}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("create archive root");
    root
}
