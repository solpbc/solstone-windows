// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::json;
use xtask::purity::{
    classify_members, parse_workspace_members, windows_leaks, WorkspaceMember,
    WINDOWS_ALLOWED_MEMBERS,
};

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
    let root_only_trees = trees([("safe-member", "safe-member v0.1.0\n")]);
    let diagnostics = classify_members(&members, &[], &root_only_trees).unwrap_err();
    assert!(diagnostics
        .iter()
        .any(|diagnostic| diagnostic.contains("edge count is zero")));

    let trees = trees([("safe-member", "safe-member v0.1.0\nsafe-helper v1.0.0\n")]);
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
    let trees = trees([("windows-root", "windows-root v0.1.0\nwindows-sys v0.52.0\n")]);

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
    let trees = trees([("safe-member", "safe-member v0.1.0\nsafe-helper v1.0.0\n")]);

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
    let trees = trees([("stale-root", "stale-root v0.1.0\nsafe-helper v1.0.0\n")]);

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
        ("windows-root", "windows-root v0.1.0\nwindows v0.58.0\n"),
        (
            "windows-sys-root",
            "windows-sys-root v0.1.0\nwindows-sys v0.52.0\n",
        ),
        (
            "windows-targets-root",
            "windows-targets-root v0.1.0\nwindows-targets v0.52.6\n",
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
fn strict_dev_windows_dependency_reports_full_path() {
    let path = "/workspace/crates/strict-dev/Cargo.toml";
    let members = vec![member("strict-dev", path)];
    let trees = trees([(
        "strict-dev",
        "strict-dev v0.1.0\n[dev-dependencies]\nwindows-sys v0.52.0\n",
    )]);

    let diagnostics = classify_members(&members, &[], &trees).unwrap_err();
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.contains("strict member strict-dev")
            && diagnostic.contains(path)
            && diagnostic.contains("windows-sys v0.52.0")
    }));
}

#[test]
fn strict_optional_windows_dependency_reports_full_path() {
    let path = "/workspace/crates/strict-optional/Cargo.toml";
    let members = vec![member("strict-optional", path)];
    let trees = trees([(
        "strict-optional",
        "strict-optional v0.1.0\nwindows v0.58.0\n",
    )]);

    let diagnostics = classify_members(&members, &[], &trees).unwrap_err();
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.contains("strict member strict-optional")
            && diagnostic.contains(path)
            && diagnostic.contains("windows v0.58.0")
    }));
}

#[test]
fn safe_dev_dependency_is_a_negative_control() {
    let members = vec![member("safe-dev", "/workspace/crates/safe-dev/Cargo.toml")];
    let trees = trees([(
        "safe-dev",
        "safe-dev v0.1.0\n[dev-dependencies]\nserde_json v1.0.0\n",
    )]);

    assert!(classify_members(&members, &[], &trees).is_ok());
}

#[test]
fn safe_optional_dependency_is_a_negative_control() {
    let members = vec![member(
        "safe-optional",
        "/workspace/crates/safe-optional/Cargo.toml",
    )];
    let trees = trees([(
        "safe-optional",
        "safe-optional v0.1.0\noptional-safe-helper v1.0.0\n",
    )]);

    assert!(classify_members(&members, &[], &trees).is_ok());
}

#[test]
fn windows_leaks_matches_first_tokens_only() {
    let leaks = windows_leaks(
        "windows-root v0.1.0\n\
         [dev-dependencies]\n\
         windows-sys v0.52.0\n\
         my-windows-helper v1.0.0\n\
         windowsill v2.0.0\n\
         safe-helper v3.0.0\n",
    );

    assert_eq!(
        leaks,
        vec![
            "windows-sys v0.52.0".to_string(),
            "windowsill v2.0.0".to_string(),
        ]
    );
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
            "strict-a v0.1.0\nsafe-one v1.0.0\nsafe-two v2.0.0\n",
        ),
        ("strict-b", "strict-b v0.1.0\nsafe-one v1.0.0\n"),
        ("windows-root", "windows-root v0.1.0\nwindows v0.58.0\n"),
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
        ("safe-member", "safe-member v0.1.0\nsafe-helper v1.0.0\n"),
        ("unknown-member", "unknown-member v0.1.0\nwindows v0.58.0\n"),
    ]);

    let diagnostics = classify_members(&members, &[], &trees).unwrap_err();
    assert!(diagnostics.iter().any(|diagnostic| diagnostic
        == "tree output for unknown member unknown-member (<no workspace manifest>)"));
}

fn member(package_name: &str, manifest_path: &str) -> WorkspaceMember {
    WorkspaceMember {
        package_name: package_name.to_string(),
        manifest_path: PathBuf::from(manifest_path),
    }
}

fn trees<const N: usize>(entries: [(&str, &str); N]) -> BTreeMap<String, String> {
    entries
        .into_iter()
        .map(|(name, output)| (name.to_string(), output.to_string()))
        .collect()
}
