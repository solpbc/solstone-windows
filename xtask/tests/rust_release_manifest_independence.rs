// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a workspace parent")
        .to_path_buf()
}

#[test]
fn rust_release_manifest_dependencies_do_not_enter_the_shipped_graph_through_xtask() {
    let output = Command::new(env!("CARGO"))
        .args([
            "tree",
            "--locked",
            "-p",
            "solstone-windows-app",
            "-e",
            "normal,build",
        ])
        .current_dir(repo_root())
        .env("CARGO_NET_OFFLINE", "true")
        .output()
        .expect("run offline cargo tree");
    assert!(output.status.success());
    let tree = String::from_utf8(output.stdout).unwrap();
    assert!(!tree.contains("xtask v"));
    assert!(!tree.contains("jsonschema v0.48.2"));

    let manifest = fs::read_to_string(repo_root().join("xtask/Cargo.toml")).unwrap();
    assert!(manifest.contains("jsonschema = { version = \"=0.48.2\", default-features = false }"));
    assert!(manifest.contains("toml = \"=0.8.2\""));
    assert!(manifest.contains("sha1 = \"=0.10.6\""));
    assert!(manifest.contains("semver = \"=1.0.28\""));
}

#[test]
fn rust_release_manifest_command_wiring_is_offline_and_precedes_native_ci() {
    let makefile = fs::read_to_string(repo_root().join("Makefile")).unwrap();
    let target = makefile
        .find("check-rust-release-manifest: preflight-toolchain")
        .expect("standalone manifest target");
    let target_body = &makefile[target
        ..makefile[target..]
            .find("\n# Gate, build")
            .map(|offset| target + offset)
            .unwrap_or(makefile.len())];
    assert!(target_body.contains("CARGO_NET_OFFLINE=true"));
    assert!(target_body.contains("rust-release-manifest check"));
    assert!(target_body.contains("cargo test") || target_body.contains("$(CARGO) test"));
    assert!(!target_body.contains("make package"));

    let ci_call = makefile
        .find("MANIFEST= RELEASE_DIR= $(MAKE) check-rust-release-manifest")
        .expect("self-check in ci");
    let native_call = makefile
        .find("\t$(MAKE) win-host-ci")
        .expect("native ci leg");
    assert!(ci_call < native_call);

    let main = fs::read_to_string(repo_root().join("xtask/src/main.rs")).unwrap();
    assert!(main.contains("Some(\"rust-release-manifest\")"));
    assert!(main.contains("MANIFEST"));
    assert!(main.contains("RELEASE_DIR"));
}

#[test]
fn rust_release_manifest_both_modes_is_a_usage_error() {
    let status = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["rust-release-manifest", "check"])
        .env("MANIFEST", "one")
        .env("RELEASE_DIR", "two")
        .status()
        .expect("run xtask command");
    assert_eq!(status.code(), Some(2));
}

#[test]
fn rust_release_manifest_public_surface_has_no_private_data_canaries() {
    let forbidden = [
        ["sensitive-", "ho", "st-value"].concat(),
        ["sensitive-", "pa", "th-value"].concat(),
        ["sec", "ret-account-value"].concat(),
        ["pri", "vate-workflow-marker"].concat(),
        ["off", "ice-name-marker"].concat(),
        ["lo", "de-id-marker"].concat(),
        ["hop", "per"].concat(),
        ["lo", "de"].concat(),
        ["mi", "ll"].concat(),
        ["/ho", "me/private-marker"].concat(),
        ["c:\\us", "ers\\private-marker"].concat(),
        ["acc", "ount-secret-private-host-path-value"].concat(),
    ];
    let files = [
        ".gitignore",
        "AGENTS.md",
        "Cargo.toml",
        "Cargo.lock",
        "Makefile",
        "deny.toml",
        "docs/release-runbook.md",
        "schemas/rust-release-manifest/v1.json",
        "xtask/Cargo.toml",
    ];
    for relative in files {
        let source = fs::read_to_string(repo_root().join(relative))
            .unwrap()
            .to_ascii_lowercase();
        for token in &forbidden {
            assert!(!source.contains(token), "public-data canary in {relative}");
        }
    }
    scan_tree(&repo_root().join("xtask/src"), &forbidden);
    scan_tree(&repo_root().join("xtask/tests"), &forbidden);
}

#[test]
fn rust_release_manifest_package_fixtures_exist_in_the_source_tree() {
    for relative in [
        "xtask/tests/fixtures/rust-release-manifest/manifest-mode/Solstone-0.2.11-full.nupkg",
        "xtask/tests/fixtures/rust-release-manifest/release-dir/Solstone-0.2.11-full.nupkg",
    ] {
        assert!(repo_root().join(relative).is_file(), "missing {relative}");
    }
}

fn scan_tree(root: &Path, forbidden: &[String]) {
    for entry in fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            scan_tree(&entry.path(), forbidden);
        } else {
            let bytes = fs::read(entry.path()).unwrap();
            let source = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
            for token in forbidden {
                assert!(!source.contains(token), "fixture public-data canary");
            }
        }
    }
}
