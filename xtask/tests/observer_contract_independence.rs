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
        scan_for_contract_reference(&root.join(directory));
    }
    let tauri: Value = serde_json::from_slice(
        &fs::read(root.join("src-tauri/tauri.conf.json")).expect("read Tauri config"),
    )
    .expect("parse Tauri config");
    assert_eq!(tauri["build"]["frontendDist"], "../ui/dist");
    assert!(tauri["bundle"].get("resources").is_none());
}

fn scan_for_contract_reference(path: &Path) {
    for entry in
        fs::read_dir(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
    {
        let entry = entry.expect("read directory entry");
        let metadata = fs::symlink_metadata(entry.path()).expect("entry metadata");
        if metadata.is_dir() {
            scan_for_contract_reference(&entry.path());
        } else if metadata.is_file() {
            let Ok(bytes) = fs::read(entry.path()) else {
                continue;
            };
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
                    entry.path().display()
                );
            }
        }
    }
}

#[test]
fn observer_contract_make_target_is_locked_offline_and_ordered() {
    let makefile = fs::read_to_string(repo_root().join("Makefile")).expect("read Makefile");
    let start = makefile
        .find("check-observer-contract: preflight-toolchain")
        .expect("observer contract target");
    let tail = &makefile[start..];
    let end = tail[1..]
        .find("\n\n")
        .map(|index| index + 1)
        .unwrap_or(tail.len());
    let target = &tail[..end];
    for line in target.lines().filter(|line| line.contains("$(CARGO)")) {
        assert!(line.contains("CARGO_NET_OFFLINE=true"), "{line}");
        assert!(line.contains("--locked"), "{line}");
    }
    for required in [
        "observer-contract check",
        "-p xtask",
        "-p observer-pl",
        "-p pl-transport-win",
        "--test transport_round_trip",
    ] {
        assert!(target.contains(required), "target lacks {required}");
    }
    assert!(makefile.contains(".PHONY:") && makefile.contains("check-observer-contract"));
    let help = makefile.split("# Local dev-tooling").next().unwrap();
    assert!(help.contains("check-observer-contract"));
    let purity = makefile
        .find("$(CARGO) run --locked -q -p xtask -- purity-check")
        .unwrap();
    let observer = makefile[purity..]
        .find("$(MAKE) check-observer-contract")
        .map(|index| index + purity)
        .unwrap();
    let workspace_tests = makefile[observer..]
        .find("$(CARGO) test --locked --workspace $(REMOTE_CRATES)")
        .map(|index| index + observer)
        .unwrap();
    assert!(purity < observer && observer < workspace_tests);
    let lower = target.to_ascii_lowercase();
    assert!(lower.contains("local offline") && lower.contains("structural/behavioral"));
    for unsupported_claim in [
        "live-journal",
        "package proof",
        "smoke proof",
        "release proof",
    ] {
        assert!(!lower.contains(unsupported_claim));
    }
}
