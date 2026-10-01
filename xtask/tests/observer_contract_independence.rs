// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has workspace parent")
        .to_path_buf()
}

fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned())
}

#[test]
fn observer_contract_production_graph_excludes_xtask() {
    let root = repo_root();
    let output = Command::new(cargo())
        .current_dir(&root)
        .args(["metadata", "--locked", "--offline", "--format-version", "1"])
        .output()
        .expect("run cargo metadata");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Value = serde_json::from_slice(&output.stdout).expect("parse cargo metadata");
    let packages = metadata["packages"].as_array().unwrap();
    let xtask_id = packages
        .iter()
        .find(|package| package["name"] == "xtask")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let workspace: BTreeSet<&str> = metadata["workspace_members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap())
        .collect();
    let nodes: BTreeMap<&str, &Value> = metadata["resolve"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| (node["id"].as_str().unwrap(), node))
        .collect();

    // Dev-dependencies intentionally power conformance tests, but Cargo never
    // propagates dev edges into a dependent's normal/build (runtime) graph.
    for member in workspace.iter().copied().filter(|id| *id != xtask_id) {
        let mut queue = VecDeque::from([member]);
        let mut visited = BTreeSet::new();
        while let Some(package_id) = queue.pop_front() {
            if !visited.insert(package_id) {
                continue;
            }
            assert_ne!(package_id, xtask_id, "xtask reached from {member}");
            let node = nodes.get(package_id).expect("metadata node");
            for dependency in node["deps"].as_array().unwrap() {
                let production_edge =
                    dependency["dep_kinds"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|kind| {
                            kind["kind"].is_null()
                                || matches!(kind["kind"].as_str(), Some("normal" | "build"))
                        });
                if production_edge {
                    queue.push_back(dependency["pkg"].as_str().unwrap());
                }
            }
        }
    }

    for edge_kind in ["normal", "build"] {
        let output = Command::new(cargo())
            .current_dir(&root)
            .args([
                "tree",
                "--locked",
                "--offline",
                "-p",
                "solstone-windows-app",
                "-e",
                edge_kind,
            ])
            .output()
            .expect("run cargo tree");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !String::from_utf8_lossy(&output.stdout)
                .to_ascii_lowercase()
                .contains("xtask"),
            "xtask appeared in app {edge_kind} graph"
        );
    }
}

#[test]
fn observer_contract_bundle_is_absent_from_product_and_package_inputs() {
    let root = repo_root();
    for directory in ["src-tauri", "packaging", "scripts", "ui"] {
        scan_for_contract_reference(&root.join(directory), Path::new(directory));
    }
    let tauri: Value = serde_json::from_slice(
        &fs::read(root.join("src-tauri/tauri.conf.json")).expect("read Tauri config"),
    )
    .expect("parse Tauri config");
    assert_eq!(tauri["build"]["frontendDist"], "../ui/dist");
    assert!(tauri["bundle"].get("resources").is_none());
}

fn scan_for_contract_reference(path: &Path, logical_path: &Path) {
    let mut entries: Vec<_> = fs::read_dir(path)
        .unwrap_or_else(|error| panic!("read {}: {error}", logical_path.display()))
        .map(|entry| {
            entry.unwrap_or_else(|error| {
                panic!("read entry under {}: {error}", logical_path.display())
            })
        })
        .collect();
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let name = entry.file_name();
        let entry_logical_path = logical_path.join(&name);
        let metadata = fs::symlink_metadata(entry.path()).unwrap_or_else(|error| {
            panic!("metadata for {}: {error}", entry_logical_path.display())
        });
        if metadata.is_dir()
            && matches!(
                name.to_str(),
                Some("node_modules" | "dist" | "target" | ".git")
            )
        {
            continue;
        }
        assert!(
            !metadata.file_type().is_symlink(),
            "refusing to follow build-input symlink {}",
            entry_logical_path.display()
        );
        if metadata.is_dir() {
            scan_for_contract_reference(&entry.path(), &entry_logical_path);
        } else if metadata.is_file() {
            let bytes = fs::read(entry.path())
                .unwrap_or_else(|error| panic!("read {}: {error}", entry_logical_path.display()));
            let text = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
            for forbidden in [
                "contracts/observer-client",
                "observer-client/bundle",
                "adoption.json",
                "../contracts",
            ] {
                assert!(
                    !text.contains(forbidden),
                    "{} references product-excluded observer contract input {forbidden}",
                    entry_logical_path.display()
                );
            }
        } else {
            panic!(
                "unsupported build-input file type at {}",
                entry_logical_path.display()
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn observer_contract_command_boundary_is_offline_locked_and_propagates_failure() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = tempfile::tempdir().unwrap();
    let fake_cargo = scratch.path().join("cargo-fixture");
    fs::write(
        &fake_cargo,
        r#"#!/bin/sh
set -eu
printf '%s' "${CARGO_NET_OFFLINE-unset}" >> "$CALLS_PATH"
for argument in "$@"; do printf '\t%s' "$argument" >> "$CALLS_PATH"; done
printf '\n' >> "$CALLS_PATH"
count=$(wc -l < "$CALLS_PATH")
if [ "$count" -eq "$FAIL_AT" ]; then exit 17; fi
"#,
    )
    .unwrap();
    fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o755)).unwrap();
    for fail_at in 0..=4 {
        let calls_path = scratch.path().join(format!("calls-{fail_at}"));
        let output = Command::new("make")
            .current_dir(repo_root())
            .args([
                "--no-print-directory",
                "-o",
                "preflight-toolchain",
                "check-observer-contract",
            ])
            .arg(format!("CARGO={}", fake_cargo.display()))
            .env("CALLS_PATH", &calls_path)
            .env("FAIL_AT", fail_at.to_string())
            .output()
            .expect("run the actual make command boundary");
        assert_eq!(
            output.status.success(),
            fail_at == 0,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let calls = fs::read_to_string(calls_path).unwrap();
        let rows: Vec<Vec<&str>> = calls
            .lines()
            .map(|line| line.split('\t').collect())
            .collect();
        assert_eq!(
            rows.len(),
            if fail_at == 0 { 4 } else { fail_at },
            "failure must stop every later required command"
        );
        for (row, package) in rows
            .iter()
            .zip(["xtask", "xtask", "observer-pl", "pl-transport-win"])
        {
            assert_eq!(row[0], "true", "contract command must resolve offline");
            assert!(
                row.contains(&"--locked"),
                "contract command must preserve its lock"
            );
            let selected = row.windows(2).find(|pair| pair[0] == "-p").unwrap();
            assert_eq!(selected[1], package);
            if package == "pl-transport-win" {
                assert!(
                    row.contains(&"--lib"),
                    "routine contract behavior uses library tests"
                );
                assert!(
                    !row.iter().any(|value| value.contains("transport-tests")),
                    "routine contract gate cannot activate live fixtures"
                );
            }
        }
    }
}
