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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DependencyNode {
    pub depth: usize,
    pub identity: String,
    pub parent: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DependencyTree {
    pub nodes: Vec<DependencyNode>,
}

impl DependencyTree {
    pub fn edge_count(&self) -> usize {
        self.nodes
            .iter()
            .filter(|node| node.parent.is_some())
            .count()
    }

    pub fn ancestry_chain(&self, index: usize) -> String {
        let mut identities = Vec::new();
        let mut current = Some(index);
        while let Some(node_index) = current {
            let node = &self.nodes[node_index];
            identities.push(short_identity(&node.identity));
            current = node.parent;
        }
        identities.reverse();
        identities.join(" -> ")
    }
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

pub fn is_windows_family(identity: &str) -> bool {
    package_name(identity).starts_with("windows")
}

fn package_name(identity: &str) -> &str {
    identity.split_whitespace().next().unwrap_or_default()
}

fn short_identity(identity: &str) -> &str {
    identity
        .find(" (")
        .map_or(identity, |suffix_start| &identity[..suffix_start])
}

pub fn parse_member_tree(member_name: &str, tree_stdout: &str) -> Result<DependencyTree, String> {
    let mut nodes = Vec::new();
    let mut stack = Vec::new();

    for line in tree_stdout.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            continue;
        }

        let depth_end = line
            .find(|character: char| !character.is_ascii_digit())
            .unwrap_or(line.len());
        if depth_end == 0 {
            return Err(format!(
                "dependency line has no leading depth digit: {line}"
            ));
        }
        let (depth_str, identity) = line.split_at(depth_end);
        if identity.is_empty() {
            return Err("dependency line has empty package identity".to_string());
        }
        let depth = depth_str
            .parse::<usize>()
            .map_err(|error| format!("malformed depth {depth_str}: {error}"))?;
        let index = nodes.len();

        if index == 0 && depth != 0 {
            return Err(format!(
                "first dependency line has depth {depth}, expected 0"
            ));
        }
        if index > 0 && depth == 0 {
            return Err(format!("unexpected second root {identity}"));
        }
        if index > 0 && depth > stack.len() {
            return Err(format!("depth jump to {depth}"));
        }

        let parent = if depth == 0 {
            None
        } else {
            Some(stack[depth - 1])
        };
        nodes.push(DependencyNode {
            depth,
            identity: identity.to_string(),
            parent,
        });
        stack.truncate(depth);
        stack.push(index);
    }

    if nodes.is_empty() {
        return Err("cargo tree produced no dependency tree output".to_string());
    }

    let root_package = package_name(&nodes[0].identity);
    if root_package != member_name {
        return Err(format!(
            "root package {root_package} does not match requested member {member_name}"
        ));
    }

    Ok(DependencyTree { nodes })
}

pub fn classify_members(
    members: &[WorkspaceMember],
    exceptions: &[&str],
    trees: &BTreeMap<String, DependencyTree>,
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

    for output_name in trees.keys() {
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
        let Some(tree) = trees.get(&member.package_name) else {
            diagnostics.push(format!(
                "missing tree output for {} ({display})",
                member.package_name
            ));
            continue;
        };
        inspected_edge_count += tree.edge_count();
        let windows_nodes = tree
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| is_windows_family(&node.identity).then_some(index));
        if exception_names.contains(member.package_name.as_str()) {
            exception_count += 1;
            if windows_nodes.count() == 0 {
                diagnostics.push(format!(
                    "stale exception {} ({display}) reaches no windows-family dependency",
                    member.package_name
                ));
            }
        } else {
            strict_count += 1;
            let chains: BTreeSet<_> = windows_nodes
                .map(|index| tree.ancestry_chain(index))
                .collect();
            for chain in chains {
                diagnostics.push(format!(
                    "strict member {} ({display}) reaches windows-family dependency via {chain}",
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

    let mut trees = BTreeMap::new();
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
                "depth",
                "--no-dedupe",
            ])
            .current_dir(repo_root)
            .output()
            .map_err(|error| {
                failure_message(
                    trees.len(),
                    inspected_edge_count(&trees),
                    &[format!(
                        "{} ({}): failed to run cargo tree: {error}",
                        member.package_name,
                        member.manifest_path.display()
                    )],
                )
            })?;
        if !output.status.success() {
            return Err(failure_message(
                trees.len(),
                inspected_edge_count(&trees),
                &[format!(
                    "{} ({}): cargo tree failed: {}",
                    member.package_name,
                    member.manifest_path.display(),
                    String::from_utf8_lossy(&output.stderr).trim()
                )],
            ));
        }
        let tree_stdout = String::from_utf8_lossy(&output.stdout);
        let tree = parse_member_tree(&member.package_name, &tree_stdout).map_err(|reason| {
            failure_message(
                trees.len(),
                inspected_edge_count(&trees),
                &[format!(
                    "{} ({}): {reason}",
                    member.package_name,
                    member.manifest_path.display()
                )],
            )
        })?;
        trees.insert(member.package_name.clone(), tree);
    }

    classify_members(&members, WINDOWS_ALLOWED_MEMBERS, &trees).map_err(|diagnostics| {
        failure_message(members.len(), inspected_edge_count(&trees), &diagnostics)
    })
}

fn inspected_edge_count(trees: &BTreeMap<String, DependencyTree>) -> usize {
    trees.values().map(DependencyTree::edge_count).sum()
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
