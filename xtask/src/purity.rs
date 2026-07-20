// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Exhaustive workspace purity classification.
//!
//! Cargo metadata is the authoritative member list. Every member is inspected
//! exactly once; only the seven Windows-capable roots are maintained by hand.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceMember {
    pub package_name: String,
    pub manifest_path: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PurityWitness {
    pub member_count: usize,
    pub inspected_edge_count: usize,
    pub strict_count: usize,
    pub exception_count: usize,
}

pub const WINDOWS_ALLOWED_MEMBERS: &[&str] = &[
    "capture-engine",
    "capture-screen-encode",
    "capture-wasapi",
    "capture-wgc",
    "pl-transport-win",
    "platform-win",
    "solstone-windows-app",
];

pub fn configured_cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

pub fn parse_workspace_members(metadata_json: &str) -> Result<Vec<WorkspaceMember>, String> {
    let metadata: Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("invalid cargo metadata JSON: {error}"))?;
    let workspace_member_ids = metadata
        .get("workspace_members")
        .and_then(Value::as_array)
        .ok_or_else(|| "cargo metadata omitted workspace_members".to_string())?;
    if workspace_member_ids.is_empty() {
        return Err("cargo metadata contained zero workspace members".to_string());
    }
    let packages = metadata
        .get("packages")
        .and_then(Value::as_array)
        .ok_or_else(|| "cargo metadata omitted packages".to_string())?;

    let mut seen_ids = BTreeSet::new();
    let mut members = Vec::with_capacity(workspace_member_ids.len());
    for id_value in workspace_member_ids {
        let id = id_value
            .as_str()
            .ok_or_else(|| "cargo metadata carried a non-string workspace member ID".to_string())?;
        if !seen_ids.insert(id.to_string()) {
            return Err(format!("cargo metadata repeated workspace member ID {id}"));
        }

        let matching_packages: Vec<_> = packages
            .iter()
            .filter(|package| package.get("id").and_then(Value::as_str) == Some(id))
            .collect();
        if matching_packages.len() != 1 {
            return Err(format!(
                "workspace member ID {id} matched {} package records",
                matching_packages.len()
            ));
        }
        let package = matching_packages[0];
        let package_name = package
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| format!("workspace member ID {id} omitted its package name"))?;
        let manifest_path = package
            .get("manifest_path")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
            .ok_or_else(|| format!("workspace member {package_name} omitted its manifest path"))?;
        members.push(WorkspaceMember {
            package_name: package_name.to_string(),
            manifest_path: PathBuf::from(manifest_path),
        });
    }

    let duplicate_diagnostics = duplicate_member_diagnostics(&members);
    if !duplicate_diagnostics.is_empty() {
        return Err(duplicate_diagnostics.join("\n"));
    }
    members.sort_by(|left, right| {
        left.manifest_path
            .cmp(&right.manifest_path)
            .then_with(|| left.package_name.cmp(&right.package_name))
    });
    Ok(members)
}

pub fn windows_leaks(tree_stdout: &str) -> Vec<String> {
    dependency_lines(tree_stdout)
        .into_iter()
        .filter(|line| {
            line.split_whitespace()
                .next()
                .is_some_and(|token| token.starts_with("windows"))
        })
        .map(str::to_string)
        .collect()
}

pub fn classify_members(
    members: &[WorkspaceMember],
    exceptions: &[&str],
    tree_outputs: &BTreeMap<String, String>,
) -> Result<PurityWitness, Vec<String>> {
    let mut diagnostics = duplicate_member_diagnostics(members);
    if members.is_empty() {
        diagnostics.push("workspace (<no workspace manifest>): member count is zero".to_string());
    }

    let members_by_name: BTreeMap<_, _> = members
        .iter()
        .map(|member| (member.package_name.as_str(), member))
        .collect();
    let mut exception_counts = BTreeMap::new();
    for exception in exceptions {
        *exception_counts.entry(*exception).or_insert(0usize) += 1;
    }
    for (exception, count) in &exception_counts {
        let location = members_by_name
            .get(exception)
            .map(|member| member.manifest_path.display().to_string())
            .unwrap_or_else(|| "<no workspace manifest>".to_string());
        if *count > 1 {
            diagnostics.push(format!(
                "duplicate exception {exception} ({location}) appears {count} times"
            ));
        }
        if !members_by_name.contains_key(exception) {
            diagnostics.push(format!(
                "unknown exception {exception} (<no workspace manifest>)"
            ));
        }
    }

    for output_name in tree_outputs.keys() {
        if !members_by_name.contains_key(output_name.as_str()) {
            diagnostics.push(format!(
                "tree output for unknown member {output_name} (<no workspace manifest>)"
            ));
        }
    }

    let exception_names: BTreeSet<_> = exception_counts.keys().copied().collect();
    let mut inspected_edge_count = 0usize;
    let mut strict_count = 0usize;
    let mut exception_count = 0usize;
    let mut sorted_members: Vec<_> = members.iter().collect();
    sorted_members.sort_by(|left, right| {
        left.manifest_path
            .cmp(&right.manifest_path)
            .then_with(|| left.package_name.cmp(&right.package_name))
    });

    for member in sorted_members {
        let display = member.manifest_path.display();
        let Some(tree_stdout) = tree_outputs.get(&member.package_name) else {
            diagnostics.push(format!(
                "missing tree output for {} ({display})",
                member.package_name
            ));
            continue;
        };
        inspected_edge_count += dependency_lines(tree_stdout).len();
        let leaks = windows_leaks(tree_stdout);
        if exception_names.contains(member.package_name.as_str()) {
            exception_count += 1;
            if leaks.is_empty() {
                diagnostics.push(format!(
                    "stale exception {} ({display}) reaches no windows-family dependency",
                    member.package_name
                ));
            }
        } else {
            strict_count += 1;
            for leak in leaks {
                diagnostics.push(format!(
                    "strict member {} ({display}) reaches {leak}",
                    member.package_name
                ));
            }
        }
    }

    if inspected_edge_count == 0 {
        diagnostics.push(
            "workspace (<no workspace manifest>): inspected dependency edge count is zero"
                .to_string(),
        );
    }
    if diagnostics.is_empty() {
        Ok(PurityWitness {
            member_count: members.len(),
            inspected_edge_count,
            strict_count,
            exception_count,
        })
    } else {
        Err(diagnostics)
    }
}

pub fn run_purity_check(repo_root: &Path, cargo: &OsStr) -> Result<PurityWitness, String> {
    let metadata_output = Command::new(cargo)
        .args(["metadata", "--locked", "--format-version", "1", "--no-deps"])
        .current_dir(repo_root)
        .output()
        .map_err(|error| {
            failure_message(
                0,
                0,
                &[format!(
                    "workspace (<no workspace manifest>): failed to run cargo metadata: {error}"
                )],
            )
        })?;
    if !metadata_output.status.success() {
        return Err(failure_message(
            0,
            0,
            &[format!(
                "workspace (<no workspace manifest>): cargo metadata failed: {}",
                String::from_utf8_lossy(&metadata_output.stderr).trim()
            )],
        ));
    }
    let members = parse_workspace_members(&String::from_utf8_lossy(&metadata_output.stdout))
        .map_err(|error| {
            failure_message(
                0,
                0,
                &[format!("workspace (<no workspace manifest>): {error}")],
            )
        })?;

    let mut tree_outputs = BTreeMap::new();
    for member in &members {
        let package_name = member.package_name.as_str();
        let output = Command::new(cargo)
            .args([
                "tree",
                "--locked",
                "-p",
                package_name,
                "--target",
                "all",
                "--all-features",
                "-e",
                "normal,build,dev",
                "--prefix",
                "none",
            ])
            .current_dir(repo_root)
            .output()
            .map_err(|error| {
                failure_message(
                    tree_outputs.len(),
                    inspected_edge_count(&tree_outputs),
                    &[format!(
                        "{} ({}): failed to run cargo tree: {error}",
                        member.package_name,
                        member.manifest_path.display()
                    )],
                )
            })?;
        if !output.status.success() {
            return Err(failure_message(
                tree_outputs.len(),
                inspected_edge_count(&tree_outputs),
                &[format!(
                    "{} ({}): cargo tree failed: {}",
                    member.package_name,
                    member.manifest_path.display(),
                    String::from_utf8_lossy(&output.stderr).trim()
                )],
            ));
        }
        tree_outputs.insert(
            member.package_name.clone(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        );
    }

    classify_members(&members, WINDOWS_ALLOWED_MEMBERS, &tree_outputs).map_err(|diagnostics| {
        failure_message(
            members.len(),
            inspected_edge_count(&tree_outputs),
            &diagnostics,
        )
    })
}

fn dependency_lines(tree_stdout: &str) -> Vec<&str> {
    let mut root_seen = false;
    let mut dependencies = Vec::new();
    for line in tree_stdout.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('[') {
            continue;
        }
        if !root_seen {
            root_seen = true;
            continue;
        }
        dependencies.push(line);
    }
    dependencies
}

fn inspected_edge_count(tree_outputs: &BTreeMap<String, String>) -> usize {
    tree_outputs
        .values()
        .map(|tree_stdout| dependency_lines(tree_stdout).len())
        .sum()
}

fn duplicate_member_diagnostics(members: &[WorkspaceMember]) -> Vec<String> {
    let mut paths_by_name: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for member in members {
        paths_by_name
            .entry(&member.package_name)
            .or_default()
            .push(member.manifest_path.display().to_string());
    }

    let mut diagnostics = Vec::new();
    for (package_name, mut paths) in paths_by_name {
        if paths.len() < 2 {
            continue;
        }
        paths.sort();
        let members = paths
            .iter()
            .map(|path| format!("{package_name} ({path})"))
            .collect::<Vec<_>>()
            .join(", ");
        diagnostics.push(format!(
            "duplicate workspace package name {package_name}: {members}"
        ));
    }
    diagnostics
}

fn failure_message(member_count: usize, edge_count: usize, diagnostics: &[String]) -> String {
    let mut message = format!(
        "purity-check: workspace classification failed after inspecting {member_count} members and {edge_count} dependency edges:"
    );
    for diagnostic in diagnostics {
        message.push_str("\n  ");
        message.push_str(diagnostic);
    }
    message
}
