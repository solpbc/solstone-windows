// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::fs;
use std::path::{Path, PathBuf};

const AUDITED_UNSAFE_CRATES: &[&str] = &[
    "capture-screen-encode",
    "capture-wasapi",
    "capture-wgc",
    "platform-win",
    "pl-transport-win",
];

#[test]
fn workspace_members_inherit_the_unsafe_policy() {
    let root = repo_root();
    let manifests = member_manifests(&root);
    let mut violations = Vec::new();

    for manifest in manifests {
        let text = fs::read_to_string(&manifest)
            .unwrap_or_else(|error| panic!("read {}: {error}", manifest.display()));
        if !inherits_workspace_lints(&text) {
            violations.push(format!(
                "{} must contain [lints] with workspace = true",
                manifest.strip_prefix(&root).unwrap_or(&manifest).display()
            ));
        }

        let crate_name = manifest
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .expect("member manifest has a UTF-8 parent name");
        let source_root = manifest
            .parent()
            .expect("member manifest has a parent")
            .join("src");
        inspect_sources(
            &root,
            &source_root,
            AUDITED_UNSAFE_CRATES.contains(&crate_name),
            &mut violations,
        );
    }

    assert!(
        violations.is_empty(),
        "unsafe policy violations:\n{}",
        violations.join("\n")
    );
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn member_manifests(root: &Path) -> Vec<PathBuf> {
    let mut manifests = Vec::new();
    let crates = root.join("crates");
    for entry in
        fs::read_dir(&crates).unwrap_or_else(|error| panic!("read {}: {error}", crates.display()))
    {
        let path = entry.expect("read crates entry").path().join("Cargo.toml");
        if path.is_file() {
            manifests.push(path);
        }
    }
    manifests.push(root.join("src-tauri/Cargo.toml"));
    manifests.push(root.join("xtask/Cargo.toml"));
    manifests.sort();
    manifests
}

fn inherits_workspace_lints(text: &str) -> bool {
    let mut in_lints = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == "[lints]" {
            in_lints = true;
            continue;
        }
        if in_lints && trimmed.starts_with('[') {
            return false;
        }
        if in_lints && trimmed == "workspace = true" {
            return true;
        }
    }
    false
}

fn inspect_sources(root: &Path, dir: &Path, audited: bool, violations: &mut Vec<String>) {
    if !dir.is_dir() {
        return;
    }
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
        .map(|entry| entry.expect("read source entry").path())
        .collect();
    entries.sort();

    for path in entries {
        if path.is_dir() {
            inspect_sources(root, &path, audited, violations);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            inspect_rust_file(root, &path, audited, violations);
        }
    }
}

fn inspect_rust_file(root: &Path, path: &Path, audited: bool, violations: &mut Vec<String>) {
    let text =
        fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    for (index, line) in text.lines().enumerate() {
        let normalized: String = line
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();
        let display = path.strip_prefix(root).unwrap_or(path).display();
        if normalized == "#![allow(unsafe_code)]" {
            violations.push(format!(
                "{display}:{} crate-wide allow(unsafe_code) is forbidden",
                index + 1
            ));
        } else if normalized == "#[allow(unsafe_code)]" && !audited {
            violations.push(format!(
                "{display}:{} allow(unsafe_code) is outside the audited unsafe crates",
                index + 1
            ));
        }
    }
}
