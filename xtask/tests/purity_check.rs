// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use serde_json::json;
use xtask::purity::{
    classify_members, configured_cargo, is_windows_family, parse_member_tree,
    parse_workspace_members, run_purity_check, DependencyTree, PurityWitness, WorkspaceMember,
    WINDOWS_ALLOWED_MEMBERS,
};

static TEMP_WORKSPACE_COUNTER: AtomicUsize = AtomicUsize::new(0);

#[test]
fn metadata_maps_workspace_ids_and_ignores_nonmembers() {
    let metadata = json!({
        "workspace_members": ["workspace-a", "workspace-b"],
        "packages": [
            {
                "id": "workspace-a",
                "name": "member-a",
                "manifest_path": "/workspace/crates/member-a/Cargo.toml"
            },
            {
                "id": "external",
                "name": "external",
                "manifest_path": "/registry/external/Cargo.toml"
            },
            {
                "id": "workspace-b",
                "name": "member-b",
                "manifest_path": "/workspace/crates/member-b/Cargo.toml"
            }
        ]
    });

    let members = parse_workspace_members(&metadata.to_string()).unwrap();
    assert_eq!(
        members,
        vec![
            member("member-a", "/workspace/crates/member-a/Cargo.toml"),
            member("member-b", "/workspace/crates/member-b/Cargo.toml"),
        ]
    );
}

#[test]
fn empty_workspace_and_zero_edge_witness_fail() {
    let empty = json!({
        "workspace_members": [],
        "packages": []
    });
    assert!(parse_workspace_members(&empty.to_string())
        .unwrap_err()
        .contains("zero workspace members"));

    let members = vec![member(
        "safe-member",
        "/workspace/crates/safe-member/Cargo.toml",
    )];
    let root_only_trees = trees([("safe-member", "0safe-member v0.1.0\n")]);
    let diagnostics = classify_members(&members, &[], &root_only_trees).unwrap_err();
    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.contains("edge count is zero")));

    let trees = trees([("safe-member", "0safe-member v0.1.0\n1safe-helper v1.0.0\n")]);
    let witness = classify_members(&members, &[], &trees).unwrap();
    assert_eq!(witness.member_count, 1);
    assert_eq!(witness.inspected_edge_count, 1);
}

#[test]
fn duplicate_package_name_reports_every_manifest_path() {
    let metadata = json!({
        "workspace_members": ["duplicate-a", "duplicate-b"],
        "packages": [
            {
                "id": "duplicate-a",
                "name": "duplicate",
                "manifest_path": "/workspace/crates/duplicate-a/Cargo.toml"
            },
            {
                "id": "duplicate-b",
                "name": "duplicate",
                "manifest_path": "/workspace/crates/duplicate-b/Cargo.toml"
            }
        ]
    });

    let error = parse_workspace_members(&metadata.to_string()).unwrap_err();
    assert!(error.contains("duplicate workspace package name duplicate"));
    assert!(error.contains("/workspace/crates/duplicate-a/Cargo.toml"));
    assert!(error.contains("/workspace/crates/duplicate-b/Cargo.toml"));
}

#[test]
fn duplicate_exception_reports_full_path() {
    let members = vec![member(
        "windows-root",
        "/workspace/crates/windows-root/Cargo.toml",
    )];
    let trees = trees([(
        "windows-root",
        "0windows-root v0.1.0\n1windows-sys v0.52.0\n",
    )]);

    let diagnostics =
        classify_members(&members, &["windows-root", "windows-root"], &trees).unwrap_err();
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.contains("duplicate exception windows-root")
            && diagnostic.contains("/workspace/crates/windows-root/Cargo.toml")
    }));
}

#[test]
fn unknown_exception_reports_no_manifest() {
    let members = vec![member(
        "safe-member",
        "/workspace/crates/safe-member/Cargo.toml",
    )];
    let trees = trees([("safe-member", "0safe-member v0.1.0\n1safe-helper v1.0.0\n")]);

    let diagnostics = classify_members(&members, &["missing-root"], &trees).unwrap_err();
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic
                == "unknown exception missing-root (<no workspace manifest>)")
    );
}

#[test]
fn stale_exception_reports_full_path() {
    let members = vec![member(
        "stale-root",
        "/workspace/crates/stale-root/Cargo.toml",
    )];
    let trees = trees([("stale-root", "0stale-root v0.1.0\n1safe-helper v1.0.0\n")]);

    let diagnostics = classify_members(&members, &["stale-root"], &trees).unwrap_err();
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.contains("stale exception stale-root")
            && diagnostic.contains("/workspace/crates/stale-root/Cargo.toml")
    }));
}

#[test]
fn valid_exceptions_accept_windows_family_names() {
    let members = vec![
        member("windows-root", "/workspace/crates/windows-root/Cargo.toml"),
        member(
            "windows-sys-root",
            "/workspace/crates/windows-sys-root/Cargo.toml",
        ),
        member(
            "windows-targets-root",
            "/workspace/crates/windows-targets-root/Cargo.toml",
        ),
    ];
    let trees = trees([
        ("windows-root", "0windows-root v0.1.0\n1windows v0.58.0\n"),
        (
            "windows-sys-root",
            "0windows-sys-root v0.1.0\n1windows-sys v0.52.0\n",
        ),
        (
            "windows-targets-root",
            "0windows-targets-root v0.1.0\n1windows-targets v0.52.6\n",
        ),
    ]);

    let witness = classify_members(
        &members,
        &["windows-root", "windows-sys-root", "windows-targets-root"],
        &trees,
    )
    .unwrap();
    assert_eq!(witness.member_count, 3);
    assert_eq!(witness.exception_count, 3);
    assert_eq!(witness.inspected_edge_count, 3);
}

#[test]
fn windows_family_matches_first_tokens_only() {
    assert!(is_windows_family("windows-sys v0.52.0"));
    assert!(is_windows_family("windowsill v2.0.0"));
    assert!(!is_windows_family("my-windows-helper v1.0.0"));
    assert!(!is_windows_family("safe-helper v3.0.0"));
}

#[test]
fn parse_member_tree_rejects_missing_leading_depth() {
    assert_parse_error("strict", "strict v0.1.0\n", "no leading depth digit");
}

#[test]
fn parse_member_tree_rejects_nonzero_first_depth() {
    assert_parse_error(
        "foo",
        "1foo v0.1.0\n",
        "first dependency line has depth 1, expected 0",
    );
}

#[test]
fn parse_member_tree_rejects_depth_jump() {
    assert_parse_error("root", "0root v0.1.0\n2leaf v0.1.0\n", "depth jump to 2");
}

#[test]
fn parse_member_tree_rejects_second_root() {
    assert_parse_error(
        "root",
        "0root v0.1.0\n0other v0.1.0\n",
        "unexpected second root other v0.1.0",
    );
}

#[test]
fn parse_member_tree_rejects_empty_identity() {
    assert_parse_error("root", "0root v0.1.0\n1\n", "empty package identity");
}

#[test]
fn parse_member_tree_rejects_empty_output() {
    assert_parse_error("empty", "", "produced no dependency tree output");
}

#[test]
fn parse_member_tree_rejects_mismatched_root() {
    assert_parse_error(
        "expected",
        "0other v0.1.0\n",
        "root package other does not match requested member expected",
    );
}

#[test]
fn parse_member_tree_builds_parent_links_and_chain() {
    let tree = parse_member_tree(
        "root",
        "0root v0.1.0 (/workspace/root)\n\
         1mid v0.1.0 (/x)\n\
         2windows-sys v0.1.0\n",
    )
    .unwrap();

    let shape: Vec<_> = tree
        .nodes
        .iter()
        .map(|node| (node.depth, node.parent))
        .collect();
    assert_eq!(shape, vec![(0, None), (1, Some(0)), (2, Some(1))]);
    assert_eq!(tree.edge_count(), 2);
    assert_eq!(
        tree.ancestry_chain(2),
        "root v0.1.0 -> mid v0.1.0 -> windows-sys v0.1.0"
    );
}

#[test]
fn strict_member_reports_full_windows_ancestry() {
    let path = "/workspace/crates/strict/Cargo.toml";
    let members = vec![member("strict", path)];
    let trees = trees([(
        "strict",
        "0strict v0.1.0\n1bridge v0.1.0\n2windows-sys v0.52.0\n",
    )]);

    let diagnostics = classify_members(&members, &[], &trees).unwrap_err();
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.contains("strict member strict")
            && diagnostic.contains(path)
            && diagnostic.contains("strict v0.1.0 -> bridge v0.1.0 -> windows-sys v0.52.0")
    }));
}

#[test]
fn distinct_windows_paths_are_reported_deterministically() {
    let path = "/workspace/crates/strict/Cargo.toml";
    let members = vec![member("strict", path)];
    let trees = trees([(
        "strict",
        "0strict v0.1.0\n\
         1bridge-a v0.1.0\n\
         2windows-sys v0.52.0\n\
         1bridge-b v0.1.0\n\
         2windows-sys v0.52.0\n",
    )]);

    let diagnostics = classify_members(&members, &[], &trees).unwrap_err();
    assert_eq!(
        diagnostics,
        vec![
            format!(
                "strict member strict ({path}) reaches windows-family dependency via strict v0.1.0 -> bridge-a v0.1.0 -> windows-sys v0.52.0"
            ),
            format!(
                "strict member strict ({path}) reaches windows-family dependency via strict v0.1.0 -> bridge-b v0.1.0 -> windows-sys v0.52.0"
            ),
        ]
    );
}

#[test]
fn legitimate_exception_reaching_windows_via_chain_is_accepted() {
    let members = vec![member(
        "windows-root",
        "/workspace/crates/windows-root/Cargo.toml",
    )];
    let trees = trees([("windows-root", "0windows-root v0.1.0\n1windows v0.58.0\n")]);

    assert!(classify_members(&members, &["windows-root"], &trees).is_ok());
}

#[test]
fn real_cargo_target_gated_dev_dependency_reports_full_path() {
    let workspace = TempWorkspace::new();
    workspace.write_workspace("strict-dev", &["gated-bridge", "windows-sys"]);
    workspace.write_crate(
        "strict-dev",
        "strict-dev",
        "[target.'cfg(windows)'.dev-dependencies]\n\
         gated-bridge = { path = \"../gated-bridge\" }\n",
    );
    workspace.write_crate(
        "gated-bridge",
        "gated-bridge",
        "[dependencies]\nwindows-sys = { path = \"../windows-sys\" }\n",
    );
    workspace.write_crate("windows-sys", "windows-sys", "");

    let error = run_fixture(&workspace).unwrap_err();
    let manifest = workspace.root.join("strict-dev/Cargo.toml");
    assert!(error.contains("strict member strict-dev"));
    assert!(error.contains(&manifest.display().to_string()));
    assert!(error.contains("strict-dev v0.1.0 -> gated-bridge v0.1.0 -> windows-sys v0.1.0"));
}

#[test]
fn real_cargo_optional_dependency_reports_full_path() {
    let workspace = TempWorkspace::new();
    workspace.write_workspace("strict-optional", &["gated-optional", "windows"]);
    workspace.write_crate(
        "strict-optional",
        "strict-optional",
        "[dependencies]\n\
         gated-optional = { path = \"../gated-optional\", optional = true }\n\
         \n\
         [features]\n\
         default = []\n",
    );
    workspace.write_crate(
        "gated-optional",
        "gated-optional",
        "[dependencies]\nwindows = { path = \"../windows\" }\n",
    );
    workspace.write_crate("windows", "windows", "");

    let error = run_fixture(&workspace).unwrap_err();
    let manifest = workspace.root.join("strict-optional/Cargo.toml");
    assert!(error.contains("strict member strict-optional"));
    assert!(error.contains(&manifest.display().to_string()));
    assert!(error.contains("strict-optional v0.1.0 -> gated-optional v0.1.0 -> windows v0.1.0"));
}

#[test]
fn real_cargo_safe_dev_dependency_is_a_negative_control() {
    let workspace = TempWorkspace::new();
    workspace.write_workspace("safe-dev", &["benign-dev"]);
    workspace.write_crate(
        "safe-dev",
        "safe-dev",
        "[dev-dependencies]\nbenign-dev = { path = \"../benign-dev\" }\n",
    );
    workspace.write_crate("benign-dev", "benign-dev", "");

    assert!(run_fixture(&workspace).is_ok());
}

#[test]
fn real_cargo_safe_optional_dependency_is_a_negative_control() {
    let workspace = TempWorkspace::new();
    workspace.write_workspace("safe-optional", &["benign-optional"]);
    workspace.write_crate(
        "safe-optional",
        "safe-optional",
        "[dependencies]\n\
         benign-optional = { path = \"../benign-optional\", optional = true }\n\
         \n\
         [features]\n\
         default = []\n",
    );
    workspace.write_crate("benign-optional", "benign-optional", "");

    assert!(run_fixture(&workspace).is_ok());
}

#[test]
fn missing_tree_output_names_member() {
    let path = "/workspace/crates/missing-tree/Cargo.toml";
    let members = vec![member("missing-tree", path)];

    let diagnostics = classify_members(&members, &[], &BTreeMap::new()).unwrap_err();
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.contains("missing tree output for missing-tree") && diagnostic.contains(path)
    }));
}

#[test]
fn classification_counts_each_member_and_edge_once() {
    let members = vec![
        member("strict-a", "/workspace/crates/strict-a/Cargo.toml"),
        member("strict-b", "/workspace/crates/strict-b/Cargo.toml"),
        member("windows-root", "/workspace/crates/windows-root/Cargo.toml"),
    ];
    let trees = trees([
        (
            "strict-a",
            "0strict-a v0.1.0\n1safe-one v1.0.0\n1safe-two v2.0.0\n",
        ),
        ("strict-b", "0strict-b v0.1.0\n1safe-one v1.0.0\n"),
        ("windows-root", "0windows-root v0.1.0\n1windows v0.58.0\n"),
    ]);

    let witness = classify_members(&members, &["windows-root"], &trees).unwrap();
    assert_eq!(witness.member_count, 3);
    assert_eq!(witness.inspected_edge_count, 4);
    assert_eq!(witness.strict_count, 2);
    assert_eq!(witness.exception_count, 1);
}

#[test]
fn production_exception_set_is_exact() {
    assert_eq!(
        WINDOWS_ALLOWED_MEMBERS,
        [
            "capture-engine",
            "capture-screen-encode",
            "capture-wasapi",
            "capture-wgc",
            "pl-transport-win",
            "platform-win",
            "solstone-windows-app",
        ]
    );
}

#[test]
fn extra_tree_output_is_rejected() {
    let members = vec![member(
        "safe-member",
        "/workspace/crates/safe-member/Cargo.toml",
    )];
    let trees = trees([
        ("safe-member", "0safe-member v0.1.0\n1safe-helper v1.0.0\n"),
        (
            "unknown-member",
            "0unknown-member v0.1.0\n1windows v0.58.0\n",
        ),
    ]);

    let diagnostics = classify_members(&members, &[], &trees).unwrap_err();
    assert!(diagnostics.iter().any(|diagnostic| diagnostic
        == "tree output for unknown member unknown-member (<no workspace manifest>)"));
}

fn assert_parse_error(member_name: &str, stdout: &str, reason: &str) {
    let error = parse_member_tree(member_name, stdout).unwrap_err();
    assert!(error.contains(reason), "{error}");
}

fn member(package_name: &str, manifest_path: &str) -> WorkspaceMember {
    WorkspaceMember {
        package_name: package_name.to_string(),
        manifest_path: PathBuf::from(manifest_path),
    }
}

fn trees<const N: usize>(entries: [(&str, &str); N]) -> BTreeMap<String, DependencyTree> {
    entries
        .into_iter()
        .map(|(name, output)| {
            (
                name.to_string(),
                parse_member_tree(name, output).expect("valid depth-prefixed cargo tree fixture"),
            )
        })
        .collect()
}

struct TempWorkspace {
    root: PathBuf,
}

impl TempWorkspace {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "solstone-purity-{}-{}",
            std::process::id(),
            TEMP_WORKSPACE_COUNTER.fetch_add(1, Relaxed)
        ));
        fs::create_dir(&root)
            .unwrap_or_else(|error| panic!("create temp workspace {}: {error}", root.display()));
        Self { root }
    }

    fn write_workspace(&self, strict_member: &str, excluded: &[&str]) {
        let mut members = vec![strict_member];
        members.extend(WINDOWS_ALLOWED_MEMBERS.iter().copied());
        let members = members
            .iter()
            .map(|member| format!("\"{member}\""))
            .collect::<Vec<_>>()
            .join(", ");

        let mut excluded = excluded.to_vec();
        excluded.push("windows-fixture-support");
        let excluded = excluded
            .iter()
            .map(|member| format!("\"{member}\""))
            .collect::<Vec<_>>()
            .join(", ");

        self.write(
            "Cargo.toml",
            &format!(
                "[workspace]\nmembers = [{members}]\nexclude = [{excluded}]\nresolver = \"2\"\n"
            ),
        );
        self.write_crate("windows-fixture-support", "windows-fixture-support", "");
        for exception in WINDOWS_ALLOWED_MEMBERS {
            self.write_crate(
                exception,
                exception,
                "[dependencies]\n\
                 windows-fixture-support = { path = \"../windows-fixture-support\" }\n",
            );
        }
    }

    fn write_crate(&self, directory: &str, package_name: &str, manifest_tail: &str) {
        self.write(
            &format!("{directory}/Cargo.toml"),
            &format!(
                "[package]\n\
                 name = \"{package_name}\"\n\
                 version = \"0.1.0\"\n\
                 edition = \"2021\"\n\
                 \n\
                 {manifest_tail}"
            ),
        );
        self.write(&format!("{directory}/src/lib.rs"), "");
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().expect("fixture file has parent"))
            .unwrap_or_else(|error| panic!("create fixture directory {}: {error}", path.display()));
        fs::write(&path, contents)
            .unwrap_or_else(|error| panic!("write fixture file {}: {error}", path.display()));
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn run_fixture(workspace: &TempWorkspace) -> Result<PurityWitness, String> {
    let cargo = configured_cargo();
    let output = Command::new(&cargo)
        .args(["generate-lockfile"])
        .current_dir(&workspace.root)
        .env("CARGO_NET_OFFLINE", "1")
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .unwrap_or_else(|error| panic!("run cargo generate-lockfile: {error}"));
    assert!(
        output.status.success(),
        "cargo generate-lockfile failed for {}:\nstdout:\n{}\nstderr:\n{}",
        workspace.root.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    run_purity_check(&workspace.root, OsStr::new(&cargo))
}
