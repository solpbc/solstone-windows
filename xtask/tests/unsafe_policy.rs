// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use proc_macro2::{Delimiter, Ident, Spacing, TokenStream, TokenTree};
use serde_json::Value;
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{
    Attribute, Expr, ForeignItemFn, ForeignItemStatic, ImplItemFn, ItemFn, ItemForeignMod,
    ItemImpl, ItemMod, ItemStatic, ItemTrait, Lit, LitStr, Macro, Meta, Token, TraitItemFn, Type,
};

const METADATA_ARGV: [&str; 6] = [
    "metadata",
    "--locked",
    "--offline",
    "--format-version",
    "1",
    "--no-deps",
];
const UNSAFE_ATTRIBUTE_NAMES: [&str; 4] = ["export_name", "link_section", "naked", "no_mangle"];

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum ApprovedKind {
    Mod,
    ModDecl,
    Fn,
    ImplMethod,
}

impl ApprovedKind {
    fn label(self) -> &'static str {
        match self {
            Self::Mod => "Mod",
            Self::ModDecl => "ModDecl",
            Self::Fn => "Fn",
            Self::ImplMethod => "ImplMethod",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ApprovedNode {
    path: &'static str,
    kind: ApprovedKind,
    name: &'static str,
    owner: Option<&'static str>,
}

const APPROVED_NODES: &[ApprovedNode] = &[
    ApprovedNode {
        path: "crates/capture-screen-encode/src/lib.rs",
        kind: ApprovedKind::Mod,
        name: "imp",
        owner: None,
    },
    ApprovedNode {
        path: "crates/capture-wasapi/src/lib.rs",
        kind: ApprovedKind::Mod,
        name: "imp",
        owner: None,
    },
    ApprovedNode {
        path: "crates/capture-wgc/src/lib.rs",
        kind: ApprovedKind::Mod,
        name: "imp",
        owner: None,
    },
    ApprovedNode {
        path: "crates/platform-win/src/lib.rs",
        kind: ApprovedKind::ModDecl,
        name: "local_offset",
        owner: None,
    },
    ApprovedNode {
        path: "crates/platform-win/src/lib.rs",
        kind: ApprovedKind::Fn,
        name: "acquire_single_instance",
        owner: None,
    },
    ApprovedNode {
        path: "crates/platform-win/src/lib.rs",
        kind: ApprovedKind::Mod,
        name: "notification_pump",
        owner: None,
    },
    ApprovedNode {
        path: "crates/pl-transport-win/src/credential.rs",
        kind: ApprovedKind::ImplMethod,
        name: "protect",
        owner: Some("DpapiProtector"),
    },
    ApprovedNode {
        path: "crates/pl-transport-win/src/credential.rs",
        kind: ApprovedKind::ImplMethod,
        name: "unprotect",
        owner: Some("DpapiProtector"),
    },
];

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct NodeIdentity {
    path: String,
    kind: ApprovedKind,
    name: String,
    owner: Option<String>,
}

impl NodeIdentity {
    fn from_approved(node: &ApprovedNode) -> Self {
        Self {
            path: node.path.to_string(),
            kind: node.kind,
            name: node.name.to_string(),
            owner: node.owner.map(str::to_string),
        }
    }

    fn label(&self) -> String {
        match &self.owner {
            Some(owner) => format!("{} {owner}::{}", self.kind.label(), self.name),
            None => format!("{} {}", self.kind.label(), self.name),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct UnsafePolicyWitness {
    member_count: usize,
    inventory_file_count: usize,
    visited_source_count: usize,
    approved_boundary_count: usize,
    unsafe_form_count: usize,
}

#[derive(Clone, Debug)]
struct TargetSource {
    path: PathBuf,
}

#[derive(Clone, Debug)]
struct WorkspaceMember {
    package_name: String,
    manifest_path: PathBuf,
    root: PathBuf,
    targets: Vec<TargetSource>,
}

#[derive(Debug)]
struct PolicyState {
    approved: BTreeSet<NodeIdentity>,
    approved_counts: BTreeMap<NodeIdentity, usize>,
    observed_nodes: BTreeMap<NodeIdentity, usize>,
    violations: Vec<String>,
    unsafe_form_count: usize,
}

impl PolicyState {
    fn new(approved: &[ApprovedNode]) -> Result<Self, String> {
        let mut identities = BTreeSet::new();
        for node in approved {
            validate_approved_node(node)?;
            let identity = NodeIdentity::from_approved(node);
            if !identities.insert(identity.clone()) {
                return Err(format!(
                    "approved-node table repeats {} in {}",
                    identity.label(),
                    identity.path
                ));
            }
        }
        Ok(Self {
            approved: identities,
            approved_counts: BTreeMap::new(),
            observed_nodes: BTreeMap::new(),
            violations: Vec::new(),
            unsafe_form_count: 0,
        })
    }

    fn observe_node(&mut self, identity: &NodeIdentity, line: usize) {
        if !self.approved.contains(identity) {
            return;
        }
        self.observed_nodes.entry(identity.clone()).or_insert(line);
    }

    fn inspect_level_attribute(
        &mut self,
        attribute: &Attribute,
        owner: Option<&NodeIdentity>,
        source_path: &str,
        line: usize,
    ) -> bool {
        let Some(level) = attribute_level(attribute) else {
            return false;
        };
        if level == "cfg_attr" {
            for nested_level in cfg_attr_unsafe_levels(attribute) {
                self.violations.push(format!(
                    "{source_path}:{line}: conditional {nested_level}(unsafe_code) cannot approve an unsafe boundary"
                ));
            }
            return false;
        }
        if !attribute_mentions_unsafe_code(attribute) {
            return false;
        }
        if level == "warn" || level == "expect" {
            self.violations.push(format!(
                "{source_path}:{line}: {level}(unsafe_code) cannot approve an unsafe boundary"
            ));
            return false;
        }
        if level != "allow" {
            return false;
        }
        if matches!(attribute.style, syn::AttrStyle::Inner(_)) {
            self.violations.push(format!(
                "{source_path}:{line}: inner allow(unsafe_code) cannot approve an unsafe boundary"
            ));
            return false;
        }
        if !is_canonical_allow(attribute) {
            let suffix = owner
                .map(NodeIdentity::label)
                .unwrap_or_else(|| "non-approved syntax".to_string());
            self.violations.push(format!(
                "{source_path}:{line}: noncanonical allow(unsafe_code) on {suffix}"
            ));
            return false;
        }
        let Some(identity) = owner else {
            self.violations.push(format!(
                "{source_path}:{line}: unapproved allow(unsafe_code) on non-approved syntax"
            ));
            return false;
        };
        if !self.approved.contains(identity) {
            self.violations.push(format!(
                "{source_path}:{line}: unapproved allow(unsafe_code) on {}",
                identity.label()
            ));
            return false;
        }
        let count = self.approved_counts.entry(identity.clone()).or_default();
        *count += 1;
        if *count == 2 {
            self.violations.push(format!(
                "{source_path}:{line}: approved node {} has duplicate allow(unsafe_code)",
                identity.label()
            ));
        }
        true
    }

    fn inspect_unsafe_attribute(
        &mut self,
        source_path: &str,
        line: usize,
        scope: Option<&NodeIdentity>,
    ) {
        self.record_unsafe("unsafe attribute", source_path, line, scope);
    }

    fn record_unsafe(
        &mut self,
        form: &str,
        source_path: &str,
        line: usize,
        scope: Option<&NodeIdentity>,
    ) {
        self.unsafe_form_count += 1;
        if scope.is_none() {
            self.violations.push(format!(
                "{source_path}:{line}: {form} is outside an approved unsafe boundary"
            ));
        }
    }

    fn finish_approved_set(&mut self) {
        for identity in &self.approved {
            if self.approved_counts.get(identity).copied().unwrap_or(0) == 0 {
                let line = self.observed_nodes.get(identity).copied().unwrap_or(1);
                self.violations.push(format!(
                    "{}:{line}: approved node {} is missing its canonical allow(unsafe_code)",
                    identity.path,
                    identity.label()
                ));
            }
        }
    }
}

#[derive(Clone, Debug)]
enum PendingEdge {
    Module {
        source_path: String,
        line: usize,
        module_dir: PathBuf,
        name: String,
        literal_path: Option<String>,
        scope: Option<NodeIdentity>,
    },
    Include {
        source_path: String,
        line: usize,
        physical_dir: PathBuf,
        module_dir: PathBuf,
        literal_path: String,
        scope: Option<NodeIdentity>,
    },
}

#[derive(Debug)]
struct LineLocator {
    lines_by_ident: BTreeMap<String, Vec<usize>>,
    cursors: BTreeMap<String, usize>,
}

impl LineLocator {
    fn new(source: &str) -> Self {
        Self {
            lines_by_ident: identifier_lines(source),
            cursors: BTreeMap::new(),
        }
    }

    fn next(&mut self, ident: &str) -> usize {
        let cursor = self.cursors.entry(ident.to_string()).or_default();
        let line = self
            .lines_by_ident
            .get(ident)
            .and_then(|lines| lines.get(*cursor).copied())
            .or_else(|| {
                self.lines_by_ident
                    .get(ident)
                    .and_then(|lines| lines.last().copied())
            })
            .unwrap_or(1);
        *cursor += 1;
        line
    }
}

struct FileVisitor<'a> {
    policy: &'a mut PolicyState,
    source_path: &'a str,
    physical_dir: PathBuf,
    module_dir: PathBuf,
    locator: LineLocator,
    scope: Option<NodeIdentity>,
    impl_owner: Option<String>,
    pending: Vec<PendingEdge>,
}

impl<'a> FileVisitor<'a> {
    fn new(
        policy: &'a mut PolicyState,
        source_path: &'a str,
        physical_dir: &'a Path,
        module_dir: &'a Path,
        scope: Option<NodeIdentity>,
        source: &str,
    ) -> Self {
        Self {
            policy,
            source_path,
            physical_dir: physical_dir.to_path_buf(),
            module_dir: module_dir.to_path_buf(),
            locator: LineLocator::new(source),
            scope,
            impl_owner: None,
            pending: Vec::new(),
        }
    }

    fn inspect_candidate(
        &mut self,
        identity: NodeIdentity,
        attributes: &[Attribute],
    ) -> Option<NodeIdentity> {
        // Resolve unsafe-attribute form lines first, so an attribute whose name equals the
        // item name attributes to the attribute's own line rather than the item's line.
        let mut unsafe_attribute_lines = Vec::new();
        for attribute in attributes {
            unsafe_attribute_lines.extend(self.unsafe_attribute_lines(&attribute.meta));
        }
        let node_line = self.locator.next(&identity.name);
        self.policy.observe_node(&identity, node_line);
        let mut approved = false;
        for attribute in attributes {
            let line = if attribute_mentions_unsafe_code(attribute) {
                self.locator.next("unsafe_code")
            } else {
                node_line
            };
            approved |= self.policy.inspect_level_attribute(
                attribute,
                Some(&identity),
                self.source_path,
                line,
            );
        }
        let effective_scope = if approved {
            Some(identity)
        } else {
            self.scope.clone()
        };
        for line in unsafe_attribute_lines {
            self.policy
                .inspect_unsafe_attribute(self.source_path, line, effective_scope.as_ref());
        }
        effective_scope
    }

    fn inspect_ordinary_attribute(&mut self, attribute: &Attribute) {
        let line = if attribute_mentions_unsafe_code(attribute) {
            self.locator.next("unsafe_code")
        } else {
            1
        };
        self.policy
            .inspect_level_attribute(attribute, None, self.source_path, line);
        for line in self.unsafe_attribute_lines(&attribute.meta) {
            self.policy
                .inspect_unsafe_attribute(self.source_path, line, self.scope.as_ref());
        }
    }

    fn unsafe_attribute_lines(&mut self, meta: &Meta) -> Vec<usize> {
        unsafe_attribute_form_events(meta)
            .into_iter()
            .filter_map(|event| match event {
                MacroUnsafeFinding::UnsafeAttribute(locator) => Some(self.locator.next(locator)),
                MacroUnsafeFinding::IgnoredLocator(locator) => {
                    self.locator.next(locator);
                    None
                }
                MacroUnsafeFinding::UnsafeKeyword => {
                    self.locator.next("unsafe");
                    None
                }
            })
            .collect()
    }

    fn record_unsafe(&mut self, form: &str, ident: &str) {
        let line = self.locator.next(ident);
        self.policy
            .record_unsafe(form, self.source_path, line, self.scope.as_ref());
    }

    fn literal_path_attribute(&mut self, node: &ItemMod) -> Result<Option<String>, ()> {
        let path_attributes: Vec<_> = node
            .attrs
            .iter()
            .filter(|attribute| attribute.path().is_ident("path"))
            .collect();
        if path_attributes.is_empty() {
            return Ok(None);
        }
        let line = self.locator.next("path");
        if path_attributes.len() != 1 {
            self.policy.violations.push(format!(
                "{}:{line}: multiple #[path] attributes are unsupported",
                self.source_path
            ));
            return Err(());
        }
        let Meta::NameValue(name_value) = &path_attributes[0].meta else {
            self.policy.violations.push(format!(
                "{}:{line}: dynamic #[path] is unsupported",
                self.source_path
            ));
            return Err(());
        };
        let Expr::Lit(expr_lit) = &name_value.value else {
            self.policy.violations.push(format!(
                "{}:{line}: dynamic #[path] is unsupported",
                self.source_path
            ));
            return Err(());
        };
        let Lit::Str(literal) = &expr_lit.lit else {
            self.policy.violations.push(format!(
                "{}:{line}: dynamic #[path] is unsupported",
                self.source_path
            ));
            return Err(());
        };
        Ok(Some(literal.value()))
    }
}

impl<'ast> Visit<'ast> for FileVisitor<'_> {
    fn visit_attribute(&mut self, attribute: &'ast Attribute) {
        self.inspect_ordinary_attribute(attribute);
    }

    fn visit_item_mod(&mut self, node: &'ast ItemMod) {
        let kind = if node.content.is_some() {
            ApprovedKind::Mod
        } else {
            ApprovedKind::ModDecl
        };
        let identity = NodeIdentity {
            path: self.source_path.to_string(),
            kind,
            name: node.ident.to_string(),
            owner: None,
        };
        let effective_scope = self.inspect_candidate(identity, &node.attrs);
        if let Some((_, items)) = &node.content {
            let previous_scope = self.scope.clone();
            let previous_module_dir = self.module_dir.clone();
            let nested_module_dir = self.module_dir.join(node.ident.to_string());
            self.scope = effective_scope;
            self.module_dir = nested_module_dir;
            for item in items {
                self.visit_item(item);
            }
            self.scope = previous_scope;
            self.module_dir = previous_module_dir;
            return;
        }
        let literal_path = match self.literal_path_attribute(node) {
            Ok(path) => path,
            Err(()) => return,
        };
        self.pending.push(PendingEdge::Module {
            source_path: self.source_path.to_string(),
            line: self.locator.next(&node.ident.to_string()),
            module_dir: self.module_dir.clone(),
            name: node.ident.to_string(),
            literal_path,
            scope: effective_scope,
        });
    }

    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        let identity = NodeIdentity {
            path: self.source_path.to_string(),
            kind: ApprovedKind::Fn,
            name: node.sig.ident.to_string(),
            owner: None,
        };
        let previous_scope = self.scope.clone();
        self.scope = self.inspect_candidate(identity, &node.attrs);
        if node.sig.unsafety.is_some() {
            self.record_unsafe("unsafe function", "unsafe");
        }
        visit::visit_signature(self, &node.sig);
        self.visit_block(&node.block);
        self.scope = previous_scope;
    }

    fn visit_item_impl(&mut self, node: &'ast ItemImpl) {
        if node.unsafety.is_some() {
            self.record_unsafe("unsafe impl", "unsafe");
        }
        let previous_owner = self.impl_owner.clone();
        self.impl_owner = self_type_ident(node.self_ty.as_ref());
        visit::visit_item_impl(self, node);
        self.impl_owner = previous_owner;
    }

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        let identity = NodeIdentity {
            path: self.source_path.to_string(),
            kind: ApprovedKind::ImplMethod,
            name: node.sig.ident.to_string(),
            owner: self.impl_owner.clone(),
        };
        let previous_scope = self.scope.clone();
        self.scope = self.inspect_candidate(identity, &node.attrs);
        if node.sig.unsafety.is_some() {
            self.record_unsafe("unsafe impl method", "unsafe");
        }
        visit::visit_signature(self, &node.sig);
        self.visit_block(&node.block);
        self.scope = previous_scope;
    }

    fn visit_trait_item_fn(&mut self, node: &'ast TraitItemFn) {
        if node.sig.unsafety.is_some() {
            self.record_unsafe("unsafe trait method", "unsafe");
        }
        visit::visit_trait_item_fn(self, node);
    }

    fn visit_foreign_item_fn(&mut self, node: &'ast ForeignItemFn) {
        if node.sig.unsafety.is_some() {
            self.record_unsafe("unsafe foreign function", "unsafe");
        }
        visit::visit_foreign_item_fn(self, node);
    }

    fn visit_item_trait(&mut self, node: &'ast ItemTrait) {
        if node.unsafety.is_some() {
            self.record_unsafe("unsafe trait", "unsafe");
        }
        visit::visit_item_trait(self, node);
    }

    fn visit_item_foreign_mod(&mut self, node: &'ast ItemForeignMod) {
        self.record_unsafe(
            "extern block",
            if node.unsafety.is_some() {
                "unsafe"
            } else {
                "extern"
            },
        );
        visit::visit_item_foreign_mod(self, node);
    }

    fn visit_item_static(&mut self, node: &'ast ItemStatic) {
        if matches!(node.mutability, syn::StaticMutability::Mut(_)) {
            self.record_unsafe("mutable static", "mut");
        }
        visit::visit_item_static(self, node);
    }

    fn visit_foreign_item_static(&mut self, node: &'ast ForeignItemStatic) {
        if matches!(node.mutability, syn::StaticMutability::Mut(_)) {
            self.record_unsafe("mutable foreign static", "mut");
        }
        visit::visit_foreign_item_static(self, node);
    }

    fn visit_expr_unsafe(&mut self, node: &'ast syn::ExprUnsafe) {
        self.record_unsafe("unsafe block", "unsafe");
        visit::visit_expr_unsafe(self, node);
    }

    fn visit_macro(&mut self, node: &'ast Macro) {
        let macro_name = node
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string());
        if macro_name.as_deref() == Some("include") {
            let line = self.locator.next("include");
            match syn::parse2::<LitStr>(node.tokens.clone()) {
                Ok(literal) => self.pending.push(PendingEdge::Include {
                    source_path: self.source_path.to_string(),
                    line,
                    physical_dir: self.physical_dir.clone(),
                    module_dir: self.module_dir.clone(),
                    literal_path: literal.value(),
                    scope: self.scope.clone(),
                }),
                Err(_) => self.policy.violations.push(format!(
                    "{}:{line}: dynamic include! is unsupported",
                    self.source_path
                )),
            }
            return;
        }
        if macro_name.as_deref() == Some("asm") {
            self.record_unsafe("asm macro", "asm");
        } else if macro_name.as_deref() == Some("global_asm") {
            self.record_unsafe("global_asm macro", "global_asm");
        }
        let mut findings = Vec::new();
        scan_macro_tokens(&node.tokens, &mut findings);
        for finding in findings {
            match finding {
                MacroUnsafeFinding::UnsafeKeyword => {
                    self.record_unsafe("macro token unsafe", "unsafe");
                }
                MacroUnsafeFinding::UnsafeAttribute(locator) => {
                    self.record_unsafe("macro token unsafe attribute", locator);
                }
                MacroUnsafeFinding::IgnoredLocator(locator) => {
                    self.locator.next(locator);
                }
            }
        }
        visit::visit_macro(self, node);
    }
}

#[derive(Clone, Debug)]
struct VisitRecord {
    member_index: usize,
    scope: Option<NodeIdentity>,
}

struct WorkspaceScanner {
    workspace_root: PathBuf,
    target_directory: PathBuf,
    members: Vec<WorkspaceMember>,
    inventories: Vec<BTreeSet<PathBuf>>,
    visited: BTreeMap<Vec<String>, VisitRecord>,
    visited_rs: Vec<BTreeSet<PathBuf>>,
    active_sources: Vec<Vec<String>>,
    policy: PolicyState,
}

impl WorkspaceScanner {
    fn new(
        workspace_root: PathBuf,
        target_directory: PathBuf,
        members: Vec<WorkspaceMember>,
        approved: &[ApprovedNode],
    ) -> Result<Self, String> {
        let member_count = members.len();
        Ok(Self {
            workspace_root,
            target_directory,
            members,
            inventories: vec![BTreeSet::new(); member_count],
            visited: BTreeMap::new(),
            visited_rs: vec![BTreeSet::new(); member_count],
            active_sources: Vec::new(),
            policy: PolicyState::new(approved)?,
        })
    }

    fn build_inventory(&mut self) -> Result<(), String> {
        for index in 0..self.members.len() {
            let root = self.members[index].root.clone();
            let mut symlink_violations = Vec::new();
            collect_rust_inventory(
                &root,
                &self.target_directory,
                &mut self.inventories[index],
                &mut symlink_violations,
            )
            .map_err(|error| {
                format!(
                    "{}: source inventory failed: {error}",
                    display_path(&self.workspace_root, &root)
                )
            })?;
            for finding in symlink_violations {
                match finding {
                    SymlinkFinding::Forbidden { kind, path } => {
                        self.policy.violations.push(format!(
                            "{}: {kind} symlink is forbidden in member source tree: {}",
                            display_path(&self.workspace_root, &self.members[index].manifest_path),
                            display_path(&self.workspace_root, &path)
                        ));
                    }
                    SymlinkFinding::ClassifyFailed { path, error } => {
                        self.policy.violations.push(format!(
                            "{}: non-Rust symlink target classification failed: {}: {error}",
                            display_path(&self.workspace_root, &self.members[index].manifest_path),
                            display_path(&self.workspace_root, &path)
                        ));
                    }
                }
            }
            if self.inventories[index].is_empty() {
                self.policy.violations.push(format!(
                    "{}: member has zero Rust source files",
                    display_path(&self.workspace_root, &self.members[index].manifest_path)
                ));
            }
        }
        Ok(())
    }

    fn scan_targets(&mut self) {
        let mut roots = Vec::new();
        for (member_index, member) in self.members.iter().enumerate() {
            for target in &member.targets {
                roots.push((member_index, target.path.clone()));
            }
        }
        roots.sort();
        for (member_index, path) in roots {
            let Some(parent) = path.parent() else {
                self.policy.violations.push(format!(
                    "{}: target source has no parent directory",
                    display_path(&self.workspace_root, &path)
                ));
                continue;
            };
            self.scan_source(
                member_index,
                path.clone(),
                parent.to_path_buf(),
                None,
                false,
                display_path(&self.workspace_root, &path),
                1,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn scan_source(
        &mut self,
        member_index: usize,
        path: PathBuf,
        module_dir: PathBuf,
        scope: Option<NodeIdentity>,
        is_include: bool,
        referring_source: String,
        referring_line: usize,
    ) {
        let identity = path_key(&path);
        if self.active_sources.contains(&identity) {
            let cycle = if is_include {
                "recursive include! cycle"
            } else {
                "recursive source cycle"
            };
            self.policy.violations.push(format!(
                "{referring_source}:{referring_line}: {cycle}: {}",
                display_path(&self.workspace_root, &path)
            ));
            return;
        }
        if let Some(previous) = self.visited.get(&identity) {
            if previous.member_index != member_index {
                self.policy.violations.push(format!(
                    "{referring_source}:{referring_line}: source is owned by multiple workspace members: {}",
                    display_path(&self.workspace_root, &path)
                ));
            } else if previous.scope != scope {
                self.policy.violations.push(format!(
                    "{referring_source}:{referring_line}: source reached under conflicting approved scopes: {}",
                    display_path(&self.workspace_root, &path)
                ));
            }
            return;
        }
        self.visited.insert(
            identity.clone(),
            VisitRecord {
                member_index,
                scope: scope.clone(),
            },
        );
        if path.extension().and_then(OsStr::to_str) == Some("rs") {
            self.visited_rs[member_index].insert(path.clone());
        }
        self.active_sources.push(identity);

        let source_path = display_path(&self.workspace_root, &path);
        let source = match fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                self.policy
                    .violations
                    .push(format!("{source_path}: source read/UTF-8 failed: {error}"));
                self.active_sources.pop();
                return;
            }
        };
        let file = match syn::parse_file(&source) {
            Ok(file) => file,
            Err(error) => {
                self.policy
                    .violations
                    .push(format!("{source_path}: Rust parse failed: {error}"));
                self.active_sources.pop();
                return;
            }
        };
        let Some(physical_dir) = path.parent() else {
            self.policy
                .violations
                .push(format!("{source_path}: source has no parent directory"));
            self.active_sources.pop();
            return;
        };
        let pending = {
            let mut visitor = FileVisitor::new(
                &mut self.policy,
                &source_path,
                physical_dir,
                &module_dir,
                scope,
                &source,
            );
            visitor.visit_file(&file);
            visitor.pending
        };
        for edge in pending {
            self.follow_edge(member_index, edge);
        }
        self.active_sources.pop();
    }

    fn follow_edge(&mut self, member_index: usize, edge: PendingEdge) {
        match edge {
            PendingEdge::Module {
                source_path,
                line,
                module_dir,
                name,
                literal_path,
                scope,
            } => {
                let candidate = if let Some(literal) = literal_path {
                    normalize_lexical(&module_dir.join(literal))
                } else {
                    let flat = module_dir.join(format!("{name}.rs"));
                    let nested = module_dir.join(&name).join("mod.rs");
                    let flat_exists = fs::symlink_metadata(&flat).is_ok();
                    let nested_exists = fs::symlink_metadata(&nested).is_ok();
                    match (flat_exists, nested_exists) {
                        (true, false) => flat,
                        (false, true) => nested,
                        (false, false) => {
                            self.policy.violations.push(format!(
                                "{source_path}:{line}: module {name} did not resolve (tried {} and {})",
                                display_path(&self.workspace_root, &flat),
                                display_path(&self.workspace_root, &nested)
                            ));
                            return;
                        }
                        (true, true) => {
                            self.policy.violations.push(format!(
                                "{source_path}:{line}: module {name} is ambiguous between {} and {}",
                                display_path(&self.workspace_root, &flat),
                                display_path(&self.workspace_root, &nested)
                            ));
                            return;
                        }
                    }
                };
                let Some(path) =
                    self.validate_resolved_source(member_index, &source_path, line, &candidate)
                else {
                    return;
                };
                let child_module_dir = if path.file_name().and_then(OsStr::to_str) == Some("mod.rs")
                {
                    path.parent().unwrap_or(&path).to_path_buf()
                } else {
                    let stem = path.file_stem().unwrap_or_default();
                    path.parent().unwrap_or(&path).join(stem)
                };
                self.scan_source(
                    member_index,
                    path,
                    child_module_dir,
                    scope,
                    false,
                    source_path,
                    line,
                );
            }
            PendingEdge::Include {
                source_path,
                line,
                physical_dir,
                module_dir,
                literal_path,
                scope,
            } => {
                let candidate = normalize_lexical(&physical_dir.join(literal_path));
                let Some(path) =
                    self.validate_resolved_source(member_index, &source_path, line, &candidate)
                else {
                    return;
                };
                self.scan_source(
                    member_index,
                    path,
                    module_dir,
                    scope,
                    true,
                    source_path,
                    line,
                );
            }
        }
    }

    fn validate_resolved_source(
        &mut self,
        member_index: usize,
        source_path: &str,
        line: usize,
        candidate: &Path,
    ) -> Option<PathBuf> {
        let member_root = &self.members[member_index].root;
        let lexical_member_root = normalize_lexical(member_root);
        let lexical_candidate = normalize_lexical(candidate);
        if !is_within(&lexical_candidate, &lexical_member_root) {
            self.policy.violations.push(format!(
                "{source_path}:{line}: resolved source escapes member root: {}",
                display_path(&self.workspace_root, &lexical_candidate)
            ));
            return None;
        }
        let components: Vec<_> = lexical_candidate
            .components()
            .skip(path_key(&lexical_member_root).len())
            .collect();
        let mut current = lexical_member_root;
        for (index, component) in components.iter().enumerate() {
            current.push(component.as_os_str());
            let metadata = match fs::symlink_metadata(&current) {
                Ok(metadata) => metadata,
                Err(error) => {
                    self.policy.violations.push(format!(
                        "{source_path}:{line}: resolved source is unavailable: {}: {error}",
                        display_path(&self.workspace_root, &current)
                    ));
                    return None;
                }
            };
            if metadata.file_type().is_symlink() {
                let kind = if index + 1 == components.len() {
                    "source-file"
                } else {
                    "source-directory"
                };
                self.policy.violations.push(format!(
                    "{source_path}:{line}: resolved {kind} symlink is forbidden: {}",
                    display_path(&self.workspace_root, &current)
                ));
                return None;
            }
        }
        let canonical = match fs::canonicalize(&lexical_candidate) {
            Ok(path) => path,
            Err(error) => {
                self.policy.violations.push(format!(
                    "{source_path}:{line}: resolved source canonicalization failed: {}: {error}",
                    display_path(&self.workspace_root, &lexical_candidate)
                ));
                return None;
            }
        };
        if !is_within(&canonical, member_root) {
            self.policy.violations.push(format!(
                "{source_path}:{line}: resolved source escapes member root: {}",
                display_path(&self.workspace_root, &canonical)
            ));
            return None;
        }
        if !canonical.is_file() {
            self.policy.violations.push(format!(
                "{source_path}:{line}: resolved source is not a regular file: {}",
                display_path(&self.workspace_root, &canonical)
            ));
            return None;
        }
        Some(canonical)
    }

    fn reconcile(&mut self) {
        for index in 0..self.members.len() {
            let inventory_by_key: BTreeMap<_, _> = self.inventories[index]
                .iter()
                .map(|path| (path_key(path), path))
                .collect();
            let visited_by_key: BTreeMap<_, _> = self.visited_rs[index]
                .iter()
                .map(|path| (path_key(path), path))
                .collect();
            for (key, orphan) in &inventory_by_key {
                if !visited_by_key.contains_key(key) {
                    self.policy.violations.push(format!(
                        "{}: orphan .rs unreachable from any Cargo target",
                        display_path(&self.workspace_root, orphan)
                    ));
                }
            }
            for (key, unexpected) in &visited_by_key {
                if !inventory_by_key.contains_key(key) {
                    self.policy.violations.push(format!(
                        "{}: visited Rust source is absent from member inventory",
                        display_path(&self.workspace_root, unexpected)
                    ));
                }
            }
        }
    }
}

#[test]
fn workspace_members_inherit_the_unsafe_policy() {
    let root = repo_root();
    let witness = run_unsafe_policy(&root).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        witness.approved_boundary_count,
        APPROVED_NODES.len(),
        "the witness must prove exactly the reviewed boundaries"
    );
    assert!(witness.member_count > 0, "the witness must inspect members");
    assert!(
        witness.inventory_file_count > 0,
        "the witness must inventory Rust sources"
    );
    assert!(
        witness.visited_source_count > 0,
        "the witness must visit Rust sources"
    );
    assert_eq!(
        witness.inventory_file_count, witness.visited_source_count,
        "every inventoried Rust source must be visited exactly once"
    );
    assert!(
        witness.unsafe_form_count > 0,
        "the witness must find and quarantine real unsafe forms"
    );
}

#[test]
fn metadata_argv_is_locked_offline_and_stable() {
    assert_eq!(
        METADATA_ARGV,
        [
            "metadata",
            "--locked",
            "--offline",
            "--format-version",
            "1",
            "--no-deps",
        ]
    );
}

#[test]
fn unsafe_attribute_vocabulary_is_pinned() {
    assert_eq!(
        UNSAFE_ATTRIBUTE_NAMES,
        ["export_name", "link_section", "naked", "no_mangle"]
    );
}

fn run_unsafe_policy(repo_root: &Path) -> Result<UnsafePolicyWitness, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    scan_unsafe_policy(repo_root, &cargo, APPROVED_NODES)
}

fn scan_unsafe_policy(
    workspace_root: &Path,
    cargo: &OsStr,
    approved: &[ApprovedNode],
) -> Result<UnsafePolicyWitness, String> {
    let requested_root = fs::canonicalize(workspace_root).map_err(|error| {
        format!(
            "unsafe-policy: workspace root {} is unavailable: {error}",
            normalize_path_display(workspace_root)
        )
    })?;
    let output = Command::new(cargo)
        .args(METADATA_ARGV)
        .current_dir(&requested_root)
        .output()
        .map_err(|error| {
            format!(
                "unsafe-policy: failed to run cargo metadata in {}: {error}",
                normalize_path_display(&requested_root)
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "unsafe-policy: cargo metadata failed in {}: {}",
            normalize_path_display(&requested_root),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let metadata: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("unsafe-policy: invalid cargo metadata JSON: {error}"))?;
    let metadata_root = required_path(&metadata, "workspace_root")?;
    let metadata_root = fs::canonicalize(&metadata_root).map_err(|error| {
        format!(
            "unsafe-policy: metadata workspace_root {} is unavailable: {error}",
            normalize_path_display(&metadata_root)
        )
    })?;
    if path_key(&metadata_root) != path_key(&requested_root) {
        return Err(format!(
            "unsafe-policy: requested root {} differs from metadata workspace_root {}",
            normalize_path_display(&requested_root),
            normalize_path_display(&metadata_root)
        ));
    }
    let target_directory =
        canonicalize_allow_missing(&required_path(&metadata, "target_directory")?).map_err(
            |error| format!("unsafe-policy: target_directory normalization failed: {error}"),
        )?;
    let members = parse_workspace_members(&metadata, &metadata_root, &target_directory)?;
    let mut scanner =
        WorkspaceScanner::new(metadata_root.clone(), target_directory, members, approved)
            .map_err(|error| format!("unsafe-policy: invalid approved-node table: {error}"))?;

    for member in &scanner.members {
        let text = fs::read_to_string(&member.manifest_path).map_err(|error| {
            format!(
                "unsafe-policy: read {}: {error}",
                display_path(&metadata_root, &member.manifest_path)
            )
        })?;
        if !inherits_workspace_lints(&text) {
            scanner.policy.violations.push(format!(
                "{}: member manifest must contain [lints] with workspace = true",
                display_path(&metadata_root, &member.manifest_path)
            ));
        }
    }
    scanner.build_inventory()?;
    scanner.scan_targets();
    scanner.reconcile();
    scanner.policy.finish_approved_set();

    let member_count = scanner.members.len();
    let inventory_file_count = scanner.inventories.iter().map(BTreeSet::len).sum();
    let visited_source_count = scanner.visited.len();
    let approved_boundary_count = scanner.policy.approved.len();
    let unsafe_form_count = scanner.policy.unsafe_form_count;
    if member_count == 0 || inventory_file_count == 0 || visited_source_count == 0 {
        scanner
            .policy
            .violations
            .push("workspace scan produced an empty witness".to_string());
    }
    if scanner.policy.violations.is_empty() {
        return Ok(UnsafePolicyWitness {
            member_count,
            inventory_file_count,
            visited_source_count,
            approved_boundary_count,
            unsafe_form_count,
        });
    }
    scanner.policy.violations.sort();
    Err(format!(
        "unsafe-policy: workspace scan failed after inspecting {member_count} members, {inventory_file_count} inventory files, and {visited_source_count} visited sources:\n  {}",
        scanner.policy.violations.join("\n  ")
    ))
}

fn parse_workspace_members(
    metadata: &Value,
    workspace_root: &Path,
    target_directory: &Path,
) -> Result<Vec<WorkspaceMember>, String> {
    let ids = metadata
        .get("workspace_members")
        .and_then(Value::as_array)
        .ok_or_else(|| "unsafe-policy: cargo metadata omitted workspace_members".to_string())?;
    if ids.is_empty() {
        return Err("unsafe-policy: cargo metadata contained zero workspace members".to_string());
    }
    let packages = metadata
        .get("packages")
        .and_then(Value::as_array)
        .ok_or_else(|| "unsafe-policy: cargo metadata omitted packages".to_string())?;
    let mut seen_ids = BTreeSet::new();
    let mut members = Vec::new();
    for id in ids {
        let id = id
            .as_str()
            .ok_or_else(|| "unsafe-policy: workspace member ID was not a string".to_string())?;
        if !seen_ids.insert(id.to_string()) {
            return Err(format!(
                "unsafe-policy: cargo metadata repeated workspace member ID {id}"
            ));
        }
        let matches: Vec<_> = packages
            .iter()
            .filter(|package| package.get("id").and_then(Value::as_str) == Some(id))
            .collect();
        if matches.len() != 1 {
            return Err(format!(
                "unsafe-policy: workspace member ID {id} matched {} package records",
                matches.len()
            ));
        }
        let package = matches[0];
        let package_name = package
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| format!("unsafe-policy: workspace member {id} omitted its name"))?
            .to_string();
        let manifest_path = package
            .get("manifest_path")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
            .ok_or_else(|| {
                format!("unsafe-policy: workspace member {package_name} omitted manifest_path")
            })?;
        let lexical_manifest_path = normalize_lexical(Path::new(manifest_path));
        let lexical_root = lexical_manifest_path
            .parent()
            .ok_or_else(|| {
                format!(
                    "unsafe-policy: manifest {} has no parent",
                    normalize_path_display(&lexical_manifest_path)
                )
            })?
            .to_path_buf();
        let manifest_path = fs::canonicalize(&lexical_manifest_path).map_err(|error| {
            format!(
                "unsafe-policy: manifest_path {} is unavailable: {error}",
                normalize_path_display(&lexical_manifest_path)
            )
        })?;
        let root = manifest_path
            .parent()
            .ok_or_else(|| {
                format!(
                    "unsafe-policy: manifest {} has no parent",
                    display_path(workspace_root, &manifest_path)
                )
            })?
            .to_path_buf();
        if !is_within(&root, workspace_root) {
            return Err(format!(
                "unsafe-policy: member root {} escapes workspace root {}",
                display_path(workspace_root, &root),
                normalize_path_display(workspace_root)
            ));
        }
        if is_within(&root, target_directory) {
            return Err(format!(
                "unsafe-policy: target_directory {} equals or contains member root {}",
                display_path(workspace_root, target_directory),
                display_path(workspace_root, &root)
            ));
        }
        let targets = package
            .get("targets")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                format!("unsafe-policy: workspace member {package_name} omitted targets")
            })?;
        if targets.is_empty() {
            return Err(format!(
                "unsafe-policy: workspace member {package_name} has zero targets"
            ));
        }
        let mut target_sources = Vec::new();
        for target in targets {
            let kinds = target
                .get("kind")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    format!(
                        "unsafe-policy: target in {} omitted kind",
                        display_path(workspace_root, &manifest_path)
                    )
                })?;
            if kinds.is_empty() || kinds.iter().any(|kind| kind.as_str().is_none()) {
                return Err(format!(
                    "unsafe-policy: target in {} carried an invalid kind",
                    display_path(workspace_root, &manifest_path)
                ));
            }
            let src_path = target
                .get("src_path")
                .and_then(Value::as_str)
                .filter(|path| !path.is_empty())
                .ok_or_else(|| {
                    format!(
                        "unsafe-policy: target in {} omitted src_path",
                        display_path(workspace_root, &manifest_path)
                    )
                })?;
            let lexical = normalize_lexical(Path::new(src_path));
            if !is_within(&lexical, &lexical_root) {
                return Err(format!(
                    "unsafe-policy: {}: declared target source escapes member root: {}",
                    display_path(workspace_root, &manifest_path),
                    display_path(workspace_root, &lexical)
                ));
            }
            ensure_no_symlink_components(&lexical_root, &lexical).map_err(|(kind, path)| {
                format!(
                    "unsafe-policy: {}: declared target {kind} symlink is forbidden: {}",
                    display_path(workspace_root, &manifest_path),
                    display_path(workspace_root, &path)
                )
            })?;
            let src_path = fs::canonicalize(&lexical).map_err(|error| {
                format!(
                    "unsafe-policy: target source {} is unavailable: {error}",
                    display_path(workspace_root, &lexical)
                )
            })?;
            if !is_within(&src_path, &root) || !src_path.is_file() {
                return Err(format!(
                    "unsafe-policy: target source {} is not a regular in-member file",
                    display_path(workspace_root, &src_path)
                ));
            }
            target_sources.push(TargetSource { path: src_path });
        }
        target_sources.sort_by_cached_key(|target| path_key(&target.path));
        target_sources.dedup_by(|left, right| path_key(&left.path) == path_key(&right.path));
        members.push(WorkspaceMember {
            package_name,
            manifest_path,
            root,
            targets: target_sources,
        });
    }
    members.sort_by(|left, right| {
        left.manifest_path
            .cmp(&right.manifest_path)
            .then_with(|| left.package_name.cmp(&right.package_name))
    });
    for (index, member) in members.iter().enumerate() {
        for other in members.iter().skip(index + 1) {
            if is_within(&member.root, &other.root) || is_within(&other.root, &member.root) {
                return Err(format!(
                    "unsafe-policy: overlapping member roots {} and {}",
                    display_path(workspace_root, &member.root),
                    display_path(workspace_root, &other.root)
                ));
            }
        }
    }
    Ok(members)
}

fn validate_approved_node(node: &ApprovedNode) -> Result<(), String> {
    let path = Path::new(node.path);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        || node.path.contains('\\')
    {
        return Err(format!("approved path is not normalized: {}", node.path));
    }
    if node.name.is_empty() {
        return Err(format!("approved node in {} has an empty name", node.path));
    }
    match (node.kind, node.owner) {
        (ApprovedKind::ImplMethod, Some(owner)) if !owner.is_empty() => Ok(()),
        (ApprovedKind::ImplMethod, _) => Err(format!(
            "approved impl method {} in {} omitted its owner",
            node.name, node.path
        )),
        (_, None) => Ok(()),
        (_, Some(_)) => Err(format!(
            "approved non-method {} in {} unexpectedly has an owner",
            node.name, node.path
        )),
    }
}

fn required_path(metadata: &Value, field: &str) -> Result<PathBuf, String> {
    metadata
        .get(field)
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| format!("unsafe-policy: cargo metadata omitted {field}"))
}

/// Component-wise path identity, with Windows prefix forms and ASCII case normalized.
fn path_key(path: &Path) -> Vec<String> {
    use std::path::Prefix;

    let fold = |value: &OsStr| {
        let value = value.to_string_lossy();
        if cfg!(windows) {
            value.to_ascii_lowercase()
        } else {
            value.into_owned()
        }
    };
    path.components()
        .map(|component| match component {
            Component::Prefix(prefix) => match prefix.kind() {
                Prefix::Disk(disk) | Prefix::VerbatimDisk(disk) => {
                    format!("{}:", (disk as char).to_ascii_lowercase())
                }
                Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
                    format!(r"\\{}\{}", fold(server), fold(share))
                }
                Prefix::DeviceNS(value) | Prefix::Verbatim(value) => fold(value),
            },
            Component::RootDir => "\u{0}root".to_string(),
            Component::CurDir => ".".to_string(),
            Component::ParentDir => "..".to_string(),
            Component::Normal(value) => fold(value),
        })
        .collect()
}

fn is_within(child: &Path, ancestor: &Path) -> bool {
    let child = path_key(child);
    let ancestor = path_key(ancestor);
    ancestor.len() <= child.len() && child[..ancestor.len()] == ancestor[..]
}

fn relative_display(root: &Path, path: &Path) -> String {
    let root_key = path_key(root);
    let path_identity = path_key(path);
    if root_key.len() <= path_identity.len() && path_identity[..root_key.len()] == root_key[..] {
        path.components()
            .skip(root_key.len())
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/")
    } else {
        normalize_path_display(path)
    }
}

#[test]
fn component_path_containment_is_segment_aware() {
    assert!(is_within(
        Path::new("/ws/crates/app/src/lib.rs"),
        Path::new("/ws/crates/app")
    ));
    assert!(is_within(
        Path::new("/ws/crates/app"),
        Path::new("/ws/crates/app")
    ));
    assert!(!is_within(
        Path::new("/other/crates/app/src/lib.rs"),
        Path::new("/ws/crates/app")
    ));
    assert!(!is_within(
        Path::new("/ws/crates/app-evil/x.rs"),
        Path::new("/ws/crates/app")
    ));
    assert_eq!(
        relative_display(Path::new("/ws"), Path::new("/ws/crates/app/src/lib.rs")),
        "crates/app/src/lib.rs"
    );
}

#[cfg(windows)]
#[test]
fn windows_path_identity_handles_prefixes_case_and_segments() {
    assert!(is_within(
        Path::new(r"\\?\C:\WS\crates\app\src\lib.rs"),
        Path::new(r"c:\ws\crates\app")
    ));
    assert_eq!(
        path_key(Path::new(r"\\?\C:\x")),
        path_key(Path::new(r"C:\x"))
    );
    assert!(!is_within(
        Path::new(r"C:\ws\app-evil"),
        Path::new(r"C:\ws\app")
    ));
    assert_eq!(
        relative_display(
            Path::new(r"c:\ws"),
            Path::new(r"\\?\C:\WS\crates\app\src\lib.rs")
        ),
        "crates/app/src/lib.rs"
    );
}

fn canonicalize_allow_missing(path: &Path) -> Result<PathBuf, String> {
    let mut current = path;
    let mut suffix = Vec::new();
    while !current.exists() {
        let name = current
            .file_name()
            .ok_or_else(|| format!("{} has no existing ancestor", normalize_path_display(path)))?;
        suffix.push(name.to_os_string());
        current = current
            .parent()
            .ok_or_else(|| format!("{} has no existing ancestor", normalize_path_display(path)))?;
    }
    let mut normalized = fs::canonicalize(current).map_err(|error| error.to_string())?;
    for component in suffix.iter().rev() {
        normalized.push(component);
    }
    Ok(normalized)
}

enum NonRustSymlinkClassification {
    SourceDirectory,
    Ignore,
    Failure(io::Error),
}

fn classify_non_rust_symlink_target(follow: io::Result<bool>) -> NonRustSymlinkClassification {
    match follow {
        Ok(true) => NonRustSymlinkClassification::SourceDirectory,
        Ok(false) => NonRustSymlinkClassification::Ignore,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            NonRustSymlinkClassification::Ignore
        }
        Err(error) => NonRustSymlinkClassification::Failure(error),
    }
}

#[test]
fn non_rust_symlink_classification_marks_directory() {
    assert!(matches!(
        classify_non_rust_symlink_target(Ok(true)),
        NonRustSymlinkClassification::SourceDirectory
    ));
}

#[test]
fn non_rust_symlink_classification_ignores_file() {
    assert!(matches!(
        classify_non_rust_symlink_target(Ok(false)),
        NonRustSymlinkClassification::Ignore
    ));
}

#[test]
fn non_rust_symlink_classification_ignores_not_found() {
    assert!(matches!(
        classify_non_rust_symlink_target(Err(io::Error::from(io::ErrorKind::NotFound))),
        NonRustSymlinkClassification::Ignore
    ));
}

#[test]
fn non_rust_symlink_classification_preserves_hard_failure() {
    assert!(matches!(
        classify_non_rust_symlink_target(Err(io::Error::from(
            io::ErrorKind::PermissionDenied
        ))),
        NonRustSymlinkClassification::Failure(error)
            if error.kind() == io::ErrorKind::PermissionDenied
    ));
}

enum SymlinkFinding {
    Forbidden { kind: &'static str, path: PathBuf },
    ClassifyFailed { path: PathBuf, error: io::Error },
}

fn collect_rust_inventory(
    directory: &Path,
    target_directory: &Path,
    output: &mut BTreeSet<PathBuf>,
    symlink_violations: &mut Vec<SymlinkFinding>,
) -> Result<(), String> {
    let mut entries = fs::read_dir(directory)
        .map_err(|error| format!("read {}: {error}", normalize_path_display(directory)))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("read {} entry: {error}", normalize_path_display(directory)))?;
    entries.sort();
    for path in entries {
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("inspect {}: {error}", normalize_path_display(&path)))?;
        if metadata.file_type().is_symlink() {
            if path.extension().and_then(OsStr::to_str) == Some("rs") {
                symlink_violations.push(SymlinkFinding::Forbidden {
                    kind: "source-file",
                    path,
                });
            } else {
                match classify_non_rust_symlink_target(
                    fs::metadata(&path).map(|target| target.is_dir()),
                ) {
                    NonRustSymlinkClassification::SourceDirectory => {
                        symlink_violations.push(SymlinkFinding::Forbidden {
                            kind: "source-directory",
                            path,
                        });
                    }
                    NonRustSymlinkClassification::Ignore => {}
                    NonRustSymlinkClassification::Failure(error) => {
                        symlink_violations.push(SymlinkFinding::ClassifyFailed { path, error });
                    }
                }
            }
            continue;
        }
        if metadata.is_dir() {
            let canonical = fs::canonicalize(&path).map_err(|error| {
                format!("canonicalize {}: {error}", normalize_path_display(&path))
            })?;
            if is_within(&canonical, target_directory) {
                continue;
            }
            collect_rust_inventory(&canonical, target_directory, output, symlink_violations)?;
        } else if metadata.is_file() && path.extension().and_then(OsStr::to_str) == Some("rs") {
            let canonical = fs::canonicalize(&path).map_err(|error| {
                format!("canonicalize {}: {error}", normalize_path_display(&path))
            })?;
            if !is_within(&canonical, target_directory) {
                output.insert(canonical);
            }
        }
    }
    Ok(())
}

fn ensure_no_symlink_components(
    root: &Path,
    candidate: &Path,
) -> Result<(), (&'static str, PathBuf)> {
    if !is_within(candidate, root) {
        return Ok(());
    }
    let components: Vec<_> = candidate.components().skip(path_key(root).len()).collect();
    let mut current = root.to_path_buf();
    for (index, component) in components.iter().enumerate() {
        current.push(component.as_os_str());
        let Ok(metadata) = fs::symlink_metadata(&current) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            return Err((
                if index + 1 == components.len() {
                    "source-file"
                } else {
                    "source-directory"
                },
                current,
            ));
        }
    }
    Ok(())
}

fn inherits_workspace_lints(manifest: &str) -> bool {
    let mut in_lints = false;
    for raw_line in manifest.lines() {
        let line = strip_toml_comment(raw_line).trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_lints = line == "[lints]";
            continue;
        }
        if !in_lints {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == "workspace" && value.trim() == "true" {
            return true;
        }
    }
    false
}

fn strip_toml_comment(line: &str) -> &str {
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if quoted && character == '\\' {
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
        } else if character == '#' && !quoted {
            return &line[..index];
        }
    }
    line
}

fn attribute_level(attribute: &Attribute) -> Option<&str> {
    let path = attribute.path();
    if path.is_ident("allow") {
        Some("allow")
    } else if path.is_ident("warn") {
        Some("warn")
    } else if path.is_ident("expect") {
        Some("expect")
    } else if path.is_ident("cfg_attr") {
        Some("cfg_attr")
    } else {
        None
    }
}

fn is_canonical_allow(attribute: &Attribute) -> bool {
    if !attribute.path().is_ident("allow") {
        return false;
    }
    let Meta::List(list) = &attribute.meta else {
        return false;
    };
    let parser = Punctuated::<Meta, Token![,]>::parse_terminated;
    let Ok(arguments) = parser.parse2(list.tokens.clone()) else {
        return false;
    };
    arguments.len() == 1
        && matches!(
            arguments.first(),
            Some(Meta::Path(path)) if path.is_ident("unsafe_code")
        )
}

fn attribute_mentions_unsafe_code(attribute: &Attribute) -> bool {
    match &attribute.meta {
        Meta::Path(path) => path.is_ident("unsafe_code"),
        Meta::NameValue(_) => false,
        Meta::List(list) => token_stream_contains_ident(&list.tokens, "unsafe_code"),
    }
}

fn unsafe_attribute_name(path: &syn::Path) -> Option<&'static str> {
    UNSAFE_ATTRIBUTE_NAMES
        .iter()
        .copied()
        .find(|name| path.is_ident(name))
}

fn unsafe_attribute_ident(ident: &Ident) -> Option<&'static str> {
    UNSAFE_ATTRIBUTE_NAMES
        .iter()
        .copied()
        .find(|name| ident == name)
}

fn direct_unsafe_attribute_ident(ident: &Ident) -> Option<&'static str> {
    unsafe_attribute_ident(ident).filter(|name| *name != "naked")
}

fn unsafe_attribute_form_events(meta: &Meta) -> Vec<MacroUnsafeFinding> {
    match meta {
        Meta::Path(path) => unsafe_attribute_name(path)
            .map(MacroUnsafeFinding::UnsafeAttribute)
            .into_iter()
            .collect(),
        Meta::NameValue(name_value) => unsafe_attribute_name(&name_value.path)
            .map(MacroUnsafeFinding::UnsafeAttribute)
            .into_iter()
            .collect(),
        Meta::List(list) if list.path.is_ident("unsafe") => {
            vec![MacroUnsafeFinding::UnsafeAttribute("unsafe")]
        }
        Meta::List(list) if list.path.is_ident("cfg_attr") => {
            let parser = Punctuated::<Meta, Token![,]>::parse_terminated;
            match parser.parse2(list.tokens.clone()) {
                Ok(arguments) => arguments
                    .iter()
                    .skip(1)
                    .flat_map(unsafe_attribute_form_events)
                    .collect(),
                Err(_) => {
                    let mut events = Vec::new();
                    collect_cfg_attr_token_events(&list.tokens, &mut events);
                    events
                }
            }
        }
        Meta::List(_) => Vec::new(),
    }
}

fn cfg_attr_unsafe_levels(attribute: &Attribute) -> Vec<&'static str> {
    let Meta::List(list) = &attribute.meta else {
        return Vec::new();
    };
    let parser = Punctuated::<Meta, Token![,]>::parse_terminated;
    let Ok(arguments) = parser.parse2(list.tokens.clone()) else {
        if token_stream_contains_ident(&list.tokens, "unsafe_code")
            && token_stream_contains_ident(&list.tokens, "allow")
        {
            return vec!["allow"];
        }
        return Vec::new();
    };
    let mut levels = Vec::new();
    for meta in arguments.iter().skip(1) {
        collect_meta_unsafe_levels(meta, &mut levels);
    }
    levels
}

fn collect_meta_unsafe_levels(meta: &Meta, levels: &mut Vec<&'static str>) {
    let Meta::List(list) = meta else {
        return;
    };
    let level = if list.path.is_ident("allow") {
        Some("allow")
    } else if list.path.is_ident("warn") {
        Some("warn")
    } else if list.path.is_ident("expect") {
        Some("expect")
    } else {
        None
    };
    if let Some(level) = level {
        if token_stream_contains_ident(&list.tokens, "unsafe_code") {
            levels.push(level);
        }
    } else if list.path.is_ident("cfg_attr") {
        let parser = Punctuated::<Meta, Token![,]>::parse_terminated;
        if let Ok(arguments) = parser.parse2(list.tokens.clone()) {
            for nested in arguments.iter().skip(1) {
                collect_meta_unsafe_levels(nested, levels);
            }
        } else if token_stream_contains_ident(&list.tokens, "unsafe_code")
            && token_stream_contains_ident(&list.tokens, "allow")
        {
            levels.push("allow");
        }
    }
}

fn token_stream_contains_ident(tokens: &TokenStream, expected: &str) -> bool {
    tokens.clone().into_iter().any(|token| match token {
        TokenTree::Ident(ident) => ident == expected,
        TokenTree::Group(group) => token_stream_contains_ident(&group.stream(), expected),
        TokenTree::Punct(_) | TokenTree::Literal(_) => false,
    })
}

enum MacroUnsafeFinding {
    UnsafeKeyword,
    UnsafeAttribute(&'static str),
    IgnoredLocator(&'static str),
}

fn macro_attribute_events(tokens: &TokenStream) -> Vec<MacroUnsafeFinding> {
    let tokens: Vec<_> = tokens.clone().into_iter().collect();
    let mut events = Vec::new();
    collect_attribute_token_events(&tokens, &mut events);
    events
}

fn leading_unqualified_ident(tokens: &[TokenTree]) -> Option<&Ident> {
    let Some(TokenTree::Ident(ident)) = tokens.first() else {
        return None;
    };
    if matches!(tokens.get(1), Some(TokenTree::Punct(punct)) if punct.as_char() == ':') {
        return None;
    }
    Some(ident)
}

fn contains_recognizable_macro_metavariable(tokens: &[TokenTree]) -> bool {
    for (index, token) in tokens.iter().enumerate() {
        if matches!(token, TokenTree::Punct(punct) if punct.as_char() == '$')
            && matches!(tokens.get(index + 1), Some(TokenTree::Ident(ident)) if ident != "crate")
        {
            return true;
        }
        if let TokenTree::Group(group) = token {
            let nested: Vec<_> = group.stream().into_iter().collect();
            if contains_recognizable_macro_metavariable(&nested) {
                return true;
            }
        }
    }
    false
}

fn joint_punctuation_matches(tokens: &[TokenTree], expected: &str) -> bool {
    let expected: Vec<_> = expected.chars().collect();
    expected.iter().enumerate().all(|(index, character)| {
        matches!(
            tokens.get(index),
            Some(TokenTree::Punct(punct))
                if punct.as_char() == *character
                    && (index + 1 == expected.len() || punct.spacing() == Spacing::Joint)
        )
    })
}

fn repetition_operator(tokens: &[TokenTree]) -> Option<char> {
    let Some(TokenTree::Punct(punct)) = tokens.first() else {
        return None;
    };
    let character = punct.as_char();
    if (character == '*' && joint_punctuation_matches(tokens, "*="))
        || (character == '+' && joint_punctuation_matches(tokens, "+="))
    {
        return None;
    }
    matches!(character, '*' | '+' | '?').then_some(character)
}

fn consume_lifetime_separator(tokens: &[TokenTree]) -> Option<usize> {
    matches!(
        (tokens.first(), tokens.get(1)),
        (Some(TokenTree::Punct(apostrophe)), Some(TokenTree::Ident(_)))
            if apostrophe.as_char() == '\'' && apostrophe.spacing() == Spacing::Joint
    )
    .then_some(2)
}

fn consume_repetition_separator(tokens: &[TokenTree]) -> Option<usize> {
    const LEGAL_PUNCTUATION: &[&str] = &[
        "...", "..=", "<<=", ">>=", "!=", "%=", "&&", "&=", "*=", "+=", "-=", "->", "..", "/=",
        "::", "<-", "<<", "<=", "==", "=>", ">=", ">>", "^=", "|=", "||", "!", "#", "%", "&", ",",
        "-", ".", "/", ":", ";", "<", "=", ">", "@", "^", "|", "~",
    ];
    if let Some(lifetime) = consume_lifetime_separator(tokens) {
        return Some(lifetime);
    }
    match tokens.first()? {
        TokenTree::Ident(_) | TokenTree::Literal(_) => Some(1),
        TokenTree::Group(_) => None,
        TokenTree::Punct(_) => LEGAL_PUNCTUATION
            .iter()
            .find(|punctuation| joint_punctuation_matches(tokens, punctuation))
            .map(|punctuation| punctuation.len()),
    }
}

fn consume_one_macro_substitution(tokens: &[TokenTree]) -> Option<usize> {
    if !matches!(tokens.first(), Some(TokenTree::Punct(punct)) if punct.as_char() == '$') {
        return None;
    }
    match tokens.get(1)? {
        TokenTree::Ident(ident) if ident != "crate" => Some(2),
        TokenTree::Group(group) if group.delimiter() == Delimiter::Parenthesis => {
            let repeated: Vec<_> = group.stream().into_iter().collect();
            if !contains_recognizable_macro_metavariable(&repeated) {
                return None;
            }
            if repetition_operator(&tokens[2..]).is_some() {
                return Some(3);
            }
            let separator = consume_repetition_separator(&tokens[2..])?;
            matches!(
                repetition_operator(&tokens[2 + separator..]),
                Some('*' | '+')
            )
            .then_some(3 + separator)
        }
        TokenTree::Ident(_) | TokenTree::Group(_) | TokenTree::Punct(_) | TokenTree::Literal(_) => {
            None
        }
    }
}

fn consume_leading_macro_substitutions(tokens: &[TokenTree]) -> usize {
    let mut consumed = 0;
    while let Some(prefix) = consume_one_macro_substitution(&tokens[consumed..]) {
        consumed += prefix;
    }
    consumed
}

fn collect_attribute_token_events(tokens: &[TokenTree], events: &mut Vec<MacroUnsafeFinding>) {
    if let Some(ident) = leading_unqualified_ident(tokens) {
        if let Some(name) = direct_unsafe_attribute_ident(ident) {
            events.push(MacroUnsafeFinding::UnsafeAttribute(name));
            collect_ignored_locator_events(&tokens[1..], events);
            return;
        }
        if let Some(TokenTree::Group(arguments)) = tokens.get(1) {
            if arguments.delimiter() == Delimiter::Parenthesis {
                if ident == "unsafe" {
                    let arguments: Vec<_> = arguments.stream().into_iter().collect();
                    events.push(MacroUnsafeFinding::UnsafeAttribute("unsafe"));
                    collect_ignored_locator_events(&arguments, events);
                    collect_ignored_locator_events(&tokens[2..], events);
                    return;
                }
                if ident == "cfg_attr" {
                    collect_cfg_attr_token_events(&arguments.stream(), events);
                    collect_ignored_locator_events(&tokens[2..], events);
                    return;
                }
            }
        }
    }

    let prefix = consume_leading_macro_substitutions(tokens);
    if prefix > 0 {
        let remaining = &tokens[prefix..];
        if let Some(name) =
            leading_unqualified_ident(remaining).and_then(direct_unsafe_attribute_ident)
        {
            collect_ignored_locator_events(&tokens[..prefix], events);
            events.push(MacroUnsafeFinding::UnsafeAttribute(name));
            collect_ignored_locator_events(&remaining[1..], events);
            return;
        }
    }
    collect_ignored_locator_events(tokens, events);
}

fn collect_cfg_attr_token_events(tokens: &TokenStream, events: &mut Vec<MacroUnsafeFinding>) {
    let tokens: Vec<_> = tokens.clone().into_iter().collect();
    let mut segment_start = 0;
    let mut condition = true;
    loop {
        let prefix = consume_leading_macro_substitutions(&tokens[segment_start..]);
        let segment_end = tokens[segment_start + prefix..]
            .iter()
            .position(|token| matches!(token, TokenTree::Punct(punct) if punct.as_char() == ','))
            .map(|offset| segment_start + prefix + offset)
            .unwrap_or(tokens.len());
        let segment = &tokens[segment_start..segment_end];
        if condition {
            collect_ignored_locator_events(segment, events);
            condition = false;
        } else {
            collect_attribute_token_events(segment, events);
        }
        if segment_end == tokens.len() {
            break;
        }
        segment_start = segment_end + 1;
    }
}

fn collect_ignored_locator_events(tokens: &[TokenTree], events: &mut Vec<MacroUnsafeFinding>) {
    for token in tokens {
        match token {
            TokenTree::Ident(ident) => {
                if let Some(name) = ignored_locator_ident(ident) {
                    events.push(MacroUnsafeFinding::IgnoredLocator(name));
                }
            }
            TokenTree::Group(group) => {
                let nested: Vec<_> = group.stream().into_iter().collect();
                collect_ignored_locator_events(&nested, events);
            }
            TokenTree::Punct(_) | TokenTree::Literal(_) => {}
        }
    }
}

fn ignored_locator_ident(ident: &Ident) -> Option<&'static str> {
    let ident = ident.to_string();
    let ident = ident.strip_prefix("r#").unwrap_or(&ident);
    if ident == "unsafe" {
        return Some("unsafe");
    }
    UNSAFE_ATTRIBUTE_NAMES
        .iter()
        .copied()
        .find(|name| ident == *name)
}

fn scan_macro_tokens(tokens: &TokenStream, findings: &mut Vec<MacroUnsafeFinding>) {
    let mut tokens = tokens.clone().into_iter().peekable();
    while let Some(token) = tokens.next() {
        match token {
            TokenTree::Punct(punct)
                if punct.as_char() == '#'
                    && matches!(
                        tokens.peek(),
                        Some(TokenTree::Group(group))
                            if group.delimiter() == Delimiter::Bracket
                    ) =>
            {
                let Some(TokenTree::Group(group)) = tokens.next() else {
                    unreachable!("peeked token must remain a group");
                };
                match syn::parse2::<Meta>(group.stream()) {
                    Ok(_) => findings.extend(macro_attribute_events(&group.stream())),
                    Err(_) => {
                        let fallback = macro_attribute_events(&group.stream());
                        if fallback
                            .iter()
                            .any(|event| matches!(event, MacroUnsafeFinding::UnsafeAttribute(_)))
                        {
                            findings.extend(fallback);
                        } else {
                            scan_macro_tokens(&group.stream(), findings);
                        }
                    }
                }
            }
            TokenTree::Ident(ident) if ident == "unsafe" => {
                findings.push(MacroUnsafeFinding::UnsafeKeyword);
            }
            TokenTree::Ident(ident) => {
                if let Some(name) = ignored_locator_ident(&ident) {
                    findings.push(MacroUnsafeFinding::IgnoredLocator(name));
                }
            }
            TokenTree::Group(group) => scan_macro_tokens(&group.stream(), findings),
            TokenTree::Punct(_) | TokenTree::Literal(_) => {}
        }
    }
}

fn self_type_ident(self_type: &Type) -> Option<String> {
    let Type::Path(type_path) = self_type else {
        return None;
    };
    type_path
        .path
        .segments
        .last()
        .map(|segment| segment.ident.to_string())
}

fn identifier_lines(source: &str) -> BTreeMap<String, Vec<usize>> {
    let bytes = source.as_bytes();
    let mut output = BTreeMap::<String, Vec<usize>>::new();
    let mut index = 0;
    let mut line = 1;
    while index < bytes.len() {
        if bytes[index] == b'\n' {
            line += 1;
            index += 1;
            continue;
        }
        if bytes[index..].starts_with(b"//") {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }
        if bytes[index..].starts_with(b"/*") {
            index = skip_block_comment(bytes, index, &mut line);
            continue;
        }
        if let Some(end) = raw_string_end(bytes, index, &mut line) {
            index = end;
            continue;
        }
        if bytes[index] == b'"' || bytes[index..].starts_with(b"b\"") {
            index = skip_quoted_string(bytes, index + usize::from(bytes[index] == b'b'), &mut line);
            continue;
        }
        if is_char_literal_start(bytes, index) {
            index = skip_char_literal(bytes, index);
            continue;
        }
        if is_ident_start(bytes[index]) {
            let start = index;
            index += 1;
            while index < bytes.len() && is_ident_continue(bytes[index]) {
                index += 1;
            }
            let ident = String::from_utf8_lossy(&bytes[start..index]).to_string();
            output.entry(ident).or_default().push(line);
            continue;
        }
        index += 1;
    }
    output
}

fn skip_block_comment(bytes: &[u8], mut index: usize, line: &mut usize) -> usize {
    let mut depth = 0;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"/*") {
            depth += 1;
            index += 2;
        } else if bytes[index..].starts_with(b"*/") {
            depth -= 1;
            index += 2;
            if depth == 0 {
                break;
            }
        } else {
            if bytes[index] == b'\n' {
                *line += 1;
            }
            index += 1;
        }
    }
    index
}

fn raw_string_end(bytes: &[u8], index: usize, line: &mut usize) -> Option<usize> {
    let mut cursor = index;
    if bytes.get(cursor) == Some(&b'b') {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'r') {
        return None;
    }
    cursor += 1;
    let mut hashes = 0;
    while bytes.get(cursor) == Some(&b'#') {
        hashes += 1;
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'"') {
        return None;
    }
    cursor += 1;
    while cursor < bytes.len() {
        if bytes[cursor] == b'\n' {
            *line += 1;
        }
        if bytes[cursor] == b'"'
            && bytes
                .get(cursor + 1..cursor + 1 + hashes)
                .is_some_and(|suffix| suffix.iter().all(|byte| *byte == b'#'))
        {
            return Some(cursor + 1 + hashes);
        }
        cursor += 1;
    }
    Some(cursor)
}

fn skip_quoted_string(bytes: &[u8], quote: usize, line: &mut usize) -> usize {
    let mut index = quote + 1;
    while index < bytes.len() {
        if bytes[index] == b'\n' {
            *line += 1;
        }
        if bytes[index] == b'\\' {
            index = (index + 2).min(bytes.len());
        } else if bytes[index] == b'"' {
            return index + 1;
        } else {
            index += 1;
        }
    }
    index
}

fn is_char_literal_start(bytes: &[u8], index: usize) -> bool {
    if bytes.get(index) == Some(&b'b') && bytes.get(index + 1) == Some(&b'\'') {
        return true;
    }
    if bytes.get(index) != Some(&b'\'') {
        return false;
    }
    if bytes.get(index + 1) == Some(&b'\\') {
        return true;
    }
    bytes.get(index + 2) == Some(&b'\'')
}

fn skip_char_literal(bytes: &[u8], mut index: usize) -> usize {
    if bytes.get(index) == Some(&b'b') {
        index += 1;
    }
    index += 1;
    if bytes.get(index) == Some(&b'\\') {
        index += 2;
    } else {
        index += 1;
    }
    if bytes.get(index) == Some(&b'\'') {
        index += 1;
    }
    index
}

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_ident_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

fn display_path(workspace_root: &Path, path: &Path) -> String {
    relative_display(workspace_root, path)
}

fn normalize_path_display(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

const FIXTURE_APPROVED_MOD: &[ApprovedNode] = &[ApprovedNode {
    path: "crates/app/src/lib.rs",
    kind: ApprovedKind::Mod,
    name: "approved",
    owner: None,
}];

static TEMP_WORKSPACE_COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempWorkspace {
    root: PathBuf,
}

impl TempWorkspace {
    fn new() -> Self {
        let counter = TEMP_WORKSPACE_COUNTER.fetch_add(1, Relaxed);
        let root = std::env::temp_dir().join(format!(
            "solstone-unsafe-policy-{}-{counter}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create unsafe-policy fixture");
        Self { root }
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create fixture parent");
        }
        fs::write(&path, contents).expect("write fixture file");
    }

    fn write_workspace(&self, members: &[&str]) {
        let members = members
            .iter()
            .map(|member| format!("\"{member}\""))
            .collect::<Vec<_>>()
            .join(", ");
        self.write(
            "Cargo.toml",
            &format!(
                "[workspace]\nmembers = [{members}]\nresolver = \"2\"\n\n\
                 [workspace.package]\nversion = \"0.1.0\"\nedition = \"2021\"\nlicense = \"AGPL-3.0-only\"\n\n\
                 [workspace.lints.rust]\nunsafe_code = \"deny\"\n"
            ),
        );
    }

    fn write_member(
        &self,
        directory: &str,
        package_name: &str,
        inherit_lints: bool,
        target_config: &str,
    ) {
        let lints = if inherit_lints {
            "\n[lints]\nworkspace = true\n"
        } else {
            ""
        };
        self.write(
            &format!("{directory}/Cargo.toml"),
            &format!(
                "[package]\nname = \"{package_name}\"\nversion.workspace = true\nedition.workspace = true\nlicense.workspace = true\n{target_config}{lints}"
            ),
        );
    }

    fn basic(source: &str) -> Self {
        let workspace = Self::new();
        workspace.write_workspace(&["crates/app"]);
        workspace.write_member("crates/app", "app", true, "");
        workspace.write("crates/app/src/lib.rs", source);
        workspace
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn run_fixture(
    workspace: &TempWorkspace,
    approved: &[ApprovedNode],
) -> Result<UnsafePolicyWitness, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let output = Command::new(&cargo)
        .args(["generate-lockfile", "--offline"])
        .current_dir(&workspace.root)
        .output()
        .expect("run cargo generate-lockfile for fixture");
    assert!(
        output.status.success(),
        "fixture cargo generate-lockfile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    scan_unsafe_policy(&workspace.root, &cargo, approved)
}

fn assert_fixture_failure(
    workspace: &TempWorkspace,
    approved: &[ApprovedNode],
    expected: &str,
) -> String {
    let error = run_fixture(workspace, approved).expect_err("fixture should violate policy");
    assert!(
        error.contains(expected),
        "expected diagnostic containing {expected:?}, got:\n{error}"
    );
    error
}

fn assert_unsafe_attribute_failure(source: &str) {
    let workspace = TempWorkspace::basic(source);
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:1: unsafe attribute is outside an approved unsafe boundary",
    );
}

fn assert_macro_unsafe_attribute_failure(source: &str) {
    let workspace = TempWorkspace::basic(source);
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:1: macro token unsafe attribute is outside an approved unsafe boundary",
    );
}

#[test]
fn approved_fixture_node_passes() {
    let workspace = TempWorkspace::basic(
        "#[allow(unsafe_code)]\nmod approved { pub fn call() { unsafe {} } }\n",
    );
    run_fixture(&workspace, FIXTURE_APPROVED_MOD).expect("approved fixture node should pass");
}

#[test]
fn unapproved_sibling_allow_fails() {
    let workspace = TempWorkspace::basic(
        "#[allow(unsafe_code)]\nmod approved { pub fn call() { unsafe {} } }\n\
         #[allow(unsafe_code)]\nmod sibling { pub fn call() { unsafe {} } }\n",
    );
    assert_fixture_failure(
        &workspace,
        FIXTURE_APPROVED_MOD,
        "crates/app/src/lib.rs:3: unapproved allow(unsafe_code) on Mod sibling",
    );
}

#[test]
fn moved_approved_allow_fails() {
    let workspace = TempWorkspace::basic(
        "mod approved {}\n#[allow(unsafe_code)]\nmod sibling { pub fn call() { unsafe {} } }\n",
    );
    let error = assert_fixture_failure(
        &workspace,
        FIXTURE_APPROVED_MOD,
        "crates/app/src/lib.rs:2: unapproved allow(unsafe_code) on Mod sibling",
    );
    assert!(
        error.contains(
            "crates/app/src/lib.rs:1: approved node Mod approved is missing its canonical allow(unsafe_code)"
        ),
        "missing approved-node diagnostic:\n{error}"
    );
}

#[test]
fn duplicate_approved_allow_fails() {
    let workspace = TempWorkspace::basic(
        "#[allow(unsafe_code)]\n#[allow(unsafe_code)]\nmod approved { pub fn call() { unsafe {} } }\n",
    );
    assert_fixture_failure(
        &workspace,
        FIXTURE_APPROVED_MOD,
        "crates/app/src/lib.rs:2: approved node Mod approved has duplicate allow(unsafe_code)",
    );
}

#[test]
fn cfg_split_same_identity_without_allow_fails() {
    let workspace = TempWorkspace::basic(
        "#[cfg(any())]\n\
         #[allow(unsafe_code)]\n\
         mod approved { pub fn first() { unsafe {} } }\n\
         #[cfg(all())]\n\
         mod approved { pub fn second() { unsafe {} } }\n",
    );
    assert_fixture_failure(
        &workspace,
        FIXTURE_APPROVED_MOD,
        "crates/app/src/lib.rs:5: unsafe block is outside an approved unsafe boundary",
    );
}

#[test]
fn new_member_without_lint_inheritance_fails() {
    let workspace = TempWorkspace::new();
    workspace.write_workspace(&["crates/app", "crates/bad"]);
    workspace.write_member("crates/app", "app", true, "");
    workspace.write("crates/app/src/lib.rs", "pub fn app() {}\n");
    workspace.write_member("crates/bad", "bad", false, "");
    workspace.write("crates/bad/src/lib.rs", "pub fn bad() {}\n");
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/bad/Cargo.toml: member manifest must contain [lints] with workspace = true",
    );
}

#[test]
fn unsafe_block_fails() {
    let workspace = TempWorkspace::basic("pub fn call() { unsafe {} }\n");
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:1: unsafe block is outside an approved unsafe boundary",
    );
}

#[test]
fn unsafe_item_fn_fails() {
    let workspace = TempWorkspace::basic("pub unsafe fn call() {}\n");
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:1: unsafe function is outside an approved unsafe boundary",
    );
}

#[test]
fn unsafe_impl_method_fails() {
    let workspace = TempWorkspace::basic("pub struct App;\nimpl App { pub unsafe fn call() {} }\n");
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:2: unsafe impl method is outside an approved unsafe boundary",
    );
}

#[test]
fn unsafe_trait_method_fails() {
    let workspace = TempWorkspace::basic("pub trait App { unsafe fn call(); }\n");
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:1: unsafe trait method is outside an approved unsafe boundary",
    );
}

#[test]
fn unsafe_impl_fails() {
    let workspace =
        TempWorkspace::basic("pub trait App {}\npub struct State;\nunsafe impl App for State {}\n");
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:3: unsafe impl is outside an approved unsafe boundary",
    );
}

#[test]
fn unsafe_trait_fails() {
    let workspace = TempWorkspace::basic("pub unsafe trait App {}\n");
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:1: unsafe trait is outside an approved unsafe boundary",
    );
}

#[test]
fn extern_block_fails() {
    let workspace = TempWorkspace::basic("extern \"C\" { fn foreign_call(); }\n");
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:1: extern block is outside an approved unsafe boundary",
    );
}

#[test]
fn mutable_static_fails() {
    let workspace = TempWorkspace::basic("pub static mut VALUE: u8 = 0;\n");
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:1: mutable static is outside an approved unsafe boundary",
    );
}

#[test]
fn unsafe_attribute_fails() {
    let workspace =
        TempWorkspace::basic("#[unsafe(no_mangle)]\npub extern \"C\" fn callback() {}\n");
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:1: unsafe attribute is outside an approved unsafe boundary",
    );
}

#[test]
fn cfg_attr_unsafe_attribute_fails() {
    let workspace = TempWorkspace::basic(
        "#[cfg_attr(any(), unsafe(no_mangle))]\npub extern \"C\" fn callback() {}\n",
    );
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:1: unsafe attribute is outside an approved unsafe boundary",
    );
}

#[test]
fn direct_no_mangle_attribute_fails() {
    assert_unsafe_attribute_failure("#[no_mangle]\npub extern \"C\" fn callback() {}\n");
}

#[test]
fn direct_attribute_line_attribution_survives_name_collision() {
    // A function literally named `no_mangle` must still attribute the unsafe attribute to
    // the attribute's own line (1), not the function's line (2).
    let workspace = TempWorkspace::basic("#[no_mangle]\npub extern \"C\" fn no_mangle() {}\n");
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:1: unsafe attribute is outside an approved unsafe boundary",
    );
}

#[test]
fn direct_export_name_attribute_fails() {
    assert_unsafe_attribute_failure(
        "#[export_name = \"callback\"]\npub extern \"C\" fn exported() {}\n",
    );
}

#[test]
fn direct_link_section_attribute_fails() {
    assert_unsafe_attribute_failure(
        "#[link_section = \".solstone\"]\npub static CALLBACK: u8 = 0;\n",
    );
}

#[test]
fn unsafe_export_name_attribute_fails() {
    assert_unsafe_attribute_failure(
        "#[unsafe(export_name = \"callback\")]\npub extern \"C\" fn exported() {}\n",
    );
}

#[test]
fn unsafe_link_section_attribute_fails() {
    assert_unsafe_attribute_failure(
        "#[unsafe(link_section = \".solstone\")]\npub static CALLBACK: u8 = 0;\n",
    );
}

#[test]
fn unsafe_naked_attribute_fails() {
    assert_unsafe_attribute_failure("#[unsafe(naked)]\npub extern \"C\" fn callback() {}\n");
}

#[test]
fn cfg_attr_direct_no_mangle_attribute_fails() {
    assert_unsafe_attribute_failure(
        "#[cfg_attr(any(), no_mangle)]\npub extern \"C\" fn callback() {}\n",
    );
}

#[test]
fn cfg_attr_direct_export_name_attribute_fails() {
    assert_unsafe_attribute_failure(
        "#[cfg_attr(any(), export_name = \"callback\")]\npub extern \"C\" fn exported() {}\n",
    );
}

#[test]
fn cfg_attr_direct_link_section_attribute_fails() {
    assert_unsafe_attribute_failure(
        "#[cfg_attr(any(), link_section = \".solstone\")]\npub static CALLBACK: u8 = 0;\n",
    );
}

#[test]
fn cfg_attr_unsafe_export_name_attribute_fails() {
    assert_unsafe_attribute_failure(
        "#[cfg_attr(any(), unsafe(export_name = \"callback\"))]\npub extern \"C\" fn exported() {}\n",
    );
}

#[test]
fn cfg_attr_unsafe_link_section_attribute_fails() {
    assert_unsafe_attribute_failure(
        "#[cfg_attr(any(), unsafe(link_section = \".solstone\"))]\npub static CALLBACK: u8 = 0;\n",
    );
}

#[test]
fn cfg_attr_unsafe_naked_attribute_fails() {
    assert_unsafe_attribute_failure(
        "#[cfg_attr(any(), unsafe(naked))]\npub extern \"C\" fn callback() {}\n",
    );
}

#[test]
fn nested_cfg_attr_direct_attribute_fails() {
    assert_unsafe_attribute_failure(
        "#[cfg_attr(any(), cfg_attr(any(), no_mangle))]\npub extern \"C\" fn callback() {}\n",
    );
}

#[test]
fn nested_cfg_attr_wrapped_attribute_fails() {
    assert_unsafe_attribute_failure(
        "#[cfg_attr(any(), cfg_attr(any(), unsafe(naked)))]\npub extern \"C\" fn callback() {}\n",
    );
}

#[test]
fn unparsed_cfg_attr_candidate_locators_advance_before_node_name() {
    let workspace = TempWorkspace::basic(
        "#[cfg_attr(\n\
         no_mangle $condition,\n\
         $no_mangle\n\
         no_mangle\n\
         )]\n\
         pub fn no_mangle() {}\n",
    );
    let error = assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:4: unsafe attribute is outside an approved unsafe boundary",
    );
    for incorrect_line in [2, 3, 6] {
        let incorrect = format!(
            "crates/app/src/lib.rs:{incorrect_line}: unsafe attribute is outside an approved unsafe boundary"
        );
        assert!(
            !error.contains(&incorrect),
            "cfg_attr locator or node name stole the emitted attribute line:\n{error}"
        );
    }
    assert_eq!(
        error
            .matches("unsafe attribute is outside an approved unsafe boundary")
            .count(),
        1,
        "only the emitted ordinary attribute should report:\n{error}"
    );
}

#[test]
fn unparsed_cfg_attr_ordinary_attribute_locators_advance() {
    let workspace = TempWorkspace::basic(
        "#[cfg_attr(\n\
         no_mangle $condition,\n\
         $no_mangle\n\
         no_mangle\n\
         )]\n\
         pub struct Marker;\n",
    );
    let error = assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:4: unsafe attribute is outside an approved unsafe boundary",
    );
    for incorrect_line in [2, 3] {
        let incorrect = format!(
            "crates/app/src/lib.rs:{incorrect_line}: unsafe attribute is outside an approved unsafe boundary"
        );
        assert!(
            !error.contains(&incorrect),
            "ordinary cfg_attr ignored locator stole the emitted attribute line:\n{error}"
        );
    }
    assert_eq!(
        error
            .matches("unsafe attribute is outside an approved unsafe boundary")
            .count(),
        1,
        "only the emitted ordinary attribute should report:\n{error}"
    );
}

#[test]
fn similar_unsafe_attribute_name_passes() {
    let workspace =
        TempWorkspace::basic("#[export_names = \"callback\"]\npub extern \"C\" fn exported() {}\n");
    run_fixture(&workspace, &[]).expect("a similar attribute name must not match");
}

#[test]
fn qualified_unsafe_attribute_name_passes() {
    let workspace = TempWorkspace::basic("#[some::no_mangle]\npub extern \"C\" fn callback() {}\n");
    run_fixture(&workspace, &[]).expect("a qualified attribute path must not match");
}

#[test]
fn direct_unsafe_attribute_inside_approved_boundary_passes() {
    let workspace = TempWorkspace::basic(
        "#[allow(unsafe_code)]\nmod approved {\n\
         #[no_mangle]\npub extern \"C\" fn callback() {}\n}\n",
    );
    run_fixture(&workspace, FIXTURE_APPROVED_MOD)
        .expect("a direct unsafe attribute inside an approved boundary should pass");
}

#[test]
fn asm_macro_fails() {
    let workspace = TempWorkspace::basic("pub fn call() { core::arch::asm!(\"nop\"); }\n");
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:1: asm macro is outside an approved unsafe boundary",
    );
}

#[test]
fn global_asm_macro_fails() {
    let workspace = TempWorkspace::basic("core::arch::global_asm!(\"nop\");\n");
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:1: global_asm macro is outside an approved unsafe boundary",
    );
}

#[test]
fn macro_contained_unsafe_fails() {
    let workspace =
        TempWorkspace::basic("macro_rules! call { () => { unsafe { operation() } } }\n");
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:1: macro token unsafe is outside an approved unsafe boundary",
    );
}

#[test]
fn macro_contained_unparseable_attribute_keyword_still_fails() {
    let workspace = TempWorkspace::basic(
        "macro_rules! attributes { () => { #[unsafe $x] pub extern \"C\" fn callback() {} } }\n",
    );
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:1: macro token unsafe is outside an approved unsafe boundary",
    );
}

#[test]
fn macro_exact_outer_unsafe_forms_report_once() {
    for source in [
        "macro_rules! attributes { () => { #[unsafe()] } }\n",
        "macro_rules! attributes { () => { #[unsafe($attr)] } }\n",
        "macro_rules! attributes { () => { #[unsafe($($attr)*)] } }\n",
        "macro_rules! attributes { () => { #[unsafe(no_mangle export_name)] } }\n",
        "macro_rules! attributes { () => { #[unsafe(no_mangle)] } }\n",
    ] {
        let workspace = TempWorkspace::basic(source);
        let error = run_fixture(&workspace, &[]).expect_err("outer unsafe should violate policy");
        assert_eq!(
            error
                .matches("macro token unsafe attribute is outside an approved unsafe boundary")
                .count(),
            1,
            "outer unsafe attribute should report exactly once:\n{error}"
        );
        assert!(
            !error.contains("macro token unsafe is outside an approved unsafe boundary"),
            "outer unsafe attribute must not also report as an unsafe keyword:\n{error}"
        );
        assert!(
            error.contains(
                "crates/app/src/lib.rs:1: macro token unsafe attribute is outside an approved unsafe boundary"
            ),
            "outer unsafe attribute line was misattributed:\n{error}"
        );
    }
}

#[test]
fn macro_leading_substitution_prefix_forms_fail() {
    for source in [
        "macro_rules! attributes { () => { #[$prefix no_mangle] } }\n",
        "macro_rules! attributes { () => { #[$r#prefix export_name = \"callback\"] } }\n",
        "macro_rules! attributes { () => { #[$($prefix)* link_section = \".solstone\"] } }\n",
        "macro_rules! attributes { () => { #[$($prefix)+ no_mangle] } }\n",
        "macro_rules! attributes { () => { #[$($prefix)? no_mangle] } }\n",
        "macro_rules! attributes { () => { #[$($prefix);* no_mangle] } }\n",
        "macro_rules! attributes { () => { #[$($prefix)@+ no_mangle] } }\n",
        "macro_rules! attributes { () => { #[$($prefix)separator* no_mangle] } }\n",
        "macro_rules! attributes { () => { #[$($prefix)0+ no_mangle] } }\n",
        "macro_rules! attributes { () => { #[$($prefix)=>* no_mangle] } }\n",
        "macro_rules! attributes { () => { #[$($prefix)<<=+ no_mangle] } }\n",
        "macro_rules! attributes { () => { #[$($prefix)'item* no_mangle] } }\n",
        "macro_rules! attributes { () => { #[$($prefix)'item+ export_name = \"callback\"] } }\n",
        "macro_rules! attributes { () => { #[$($prefix)'r#item* link_section = \".solstone\"] } }\n",
        "macro_rules! attributes { () => { #[$($prefix)'r#item+ no_mangle] } }\n",
        "macro_rules! attributes { () => { #[$first $($second)? $($third);+ no_mangle] } }\n",
        "macro_rules! attributes { () => { #[$(([$prefix]))* no_mangle] } }\n",
    ] {
        assert_macro_unsafe_attribute_failure(source);
    }
}

#[test]
fn prefixed_attributes_fail_beneath_cfg_attr_and_nested_groups() {
    for source in [
        "macro_rules! attributes { () => { #[cfg_attr($condition, $prefix no_mangle)] } }\n",
        "macro_rules! attributes { () => { #[cfg_attr($outer, cfg_attr($inner, $($prefix)? export_name = \"callback\"))] } }\n",
        "probe!({ ([ #[$(($prefix))* link_section = \".solstone\"] ]) });\n",
    ] {
        assert_macro_unsafe_attribute_failure(source);
    }
}

#[test]
fn cfg_attr_repetition_commas_do_not_split_emitted_attributes() {
    let workspace = TempWorkspace::basic(
        "macro_rules! attributes {\n\
         () => {\n\
         #[cfg_attr(\n\
         $condition,\n\
         $($no_mangle),*\n\
         no_mangle,\n\
         $($export_name),+\n\
         export_name = \"callback\"\n\
         )] }; }\n",
    );
    let error =
        run_fixture(&workspace, &[]).expect_err("prefixed attributes should violate policy");
    let diagnostics = [
        "crates/app/src/lib.rs:6: macro token unsafe attribute is outside an approved unsafe boundary",
        "crates/app/src/lib.rs:8: macro token unsafe attribute is outside an approved unsafe boundary",
    ];
    for diagnostic in diagnostics {
        assert!(
            error.contains(diagnostic),
            "missing prefixed cfg_attr diagnostic {diagnostic:?}:\n{error}"
        );
    }
    assert_eq!(
        error
            .matches("macro token unsafe attribute is outside an approved unsafe boundary")
            .count(),
        2,
        "each emitted cfg_attr segment should report exactly once:\n{error}"
    );
}

#[test]
fn substitution_and_wrapper_locators_preserve_line_kind_and_order() {
    let workspace = TempWorkspace::basic(
        "macro_rules! attributes {\n\
         () => {\n\
         #[ $no_mangle\n\
         no_mangle ]\n\
         #[unsafe(\n\
         unsafe no_mangle)]\n\
         unsafe {}\n\
         #[no_mangle]\n\
         }; }\n",
    );
    let error = run_fixture(&workspace, &[]).expect_err("mixed unsafe forms should violate policy");
    let diagnostics = [
        "crates/app/src/lib.rs:4: macro token unsafe attribute is outside an approved unsafe boundary",
        "crates/app/src/lib.rs:5: macro token unsafe attribute is outside an approved unsafe boundary",
        "crates/app/src/lib.rs:7: macro token unsafe is outside an approved unsafe boundary",
        "crates/app/src/lib.rs:8: macro token unsafe attribute is outside an approved unsafe boundary",
    ];
    for diagnostic in diagnostics {
        assert!(
            error.contains(diagnostic),
            "missing locator-order diagnostic {diagnostic:?}:\n{error}"
        );
    }
    for incorrect in [
        "crates/app/src/lib.rs:3: macro token unsafe attribute is outside an approved unsafe boundary",
        "crates/app/src/lib.rs:5: macro token unsafe is outside an approved unsafe boundary",
        "crates/app/src/lib.rs:6: macro token unsafe attribute is outside an approved unsafe boundary",
        "crates/app/src/lib.rs:7: macro token unsafe attribute is outside an approved unsafe boundary",
    ] {
        assert!(
            !error.contains(incorrect),
            "ignored locator or diagnostic kind was assigned incorrectly {incorrect:?}:\n{error}"
        );
    }
    assert_eq!(
        error
            .matches("macro token unsafe attribute is outside an approved unsafe boundary")
            .count(),
        3,
        "mixed macro attributes should each report exactly once:\n{error}"
    );
    assert_eq!(
        error
            .matches("macro token unsafe is outside an approved unsafe boundary")
            .count(),
        1,
        "only the literal unsafe token should use the keyword diagnostic:\n{error}"
    );
}

#[test]
fn raw_substitution_locators_preserve_literal_line_and_kind() {
    let workspace = TempWorkspace::basic(
        "macro_rules! attributes {\n\
         ($r#no_mangle:vis, $r#unsafe:tt) => {\n\
         #[$r#no_mangle\n\
         no_mangle]\n\
         $r#unsafe\n\
         unsafe {}\n\
         };\n\
         }\n",
    );
    let error =
        run_fixture(&workspace, &[]).expect_err("literal unsafe forms should violate policy");
    for diagnostic in [
        "crates/app/src/lib.rs:4: macro token unsafe attribute is outside an approved unsafe boundary",
        "crates/app/src/lib.rs:6: macro token unsafe is outside an approved unsafe boundary",
    ] {
        assert!(
            error.contains(diagnostic),
            "raw substitution stole literal diagnostic {diagnostic:?}:\n{error}"
        );
    }
    for incorrect in [
        "crates/app/src/lib.rs:2: macro token unsafe attribute is outside an approved unsafe boundary",
        "crates/app/src/lib.rs:3: macro token unsafe attribute is outside an approved unsafe boundary",
        "crates/app/src/lib.rs:2: macro token unsafe is outside an approved unsafe boundary",
        "crates/app/src/lib.rs:4: macro token unsafe is outside an approved unsafe boundary",
        "crates/app/src/lib.rs:5: macro token unsafe is outside an approved unsafe boundary",
        "crates/app/src/lib.rs:6: macro token unsafe attribute is outside an approved unsafe boundary",
    ] {
        assert!(
            !error.contains(incorrect),
            "raw substitution produced a wrong locator or kind {incorrect:?}:\n{error}"
        );
    }
    assert_eq!(
        error
            .matches("macro token unsafe attribute is outside an approved unsafe boundary")
            .count(),
        1,
        "only the literal direct attribute should report:\n{error}"
    );
    assert_eq!(
        error
            .matches("macro token unsafe is outside an approved unsafe boundary")
            .count(),
        1,
        "only the literal unsafe keyword should report:\n{error}"
    );
}

#[test]
fn macro_contained_parse_failed_direct_attributes_fail() {
    for source in [
        "macro_rules! attributes { () => { #[no_mangle $($tail)*] } }\n",
        "macro_rules! attributes { () => { #[export_name = \"callback\" $($tail)*] } }\n",
        "macro_rules! attributes { () => { #[link_section = \".solstone\" $($tail)*] } }\n",
    ] {
        assert_macro_unsafe_attribute_failure(source);
    }
}

#[test]
fn macro_contained_parse_failed_wrapped_attributes_fail() {
    for source in [
        "macro_rules! attributes { () => { #[unsafe(no_mangle) $($tail)*] } }\n",
        "macro_rules! attributes { () => { #[unsafe(export_name = \"callback\") $($tail)*] } }\n",
        "macro_rules! attributes { () => { #[unsafe(link_section = \".solstone\") $($tail)*] } }\n",
        "macro_rules! attributes { () => { #[unsafe(naked) $($tail)*] } }\n",
    ] {
        assert_macro_unsafe_attribute_failure(source);
    }
}

#[test]
fn macro_contained_dynamic_cfg_attr_direct_attribute_fails() {
    assert_macro_unsafe_attribute_failure(
        "macro_rules! attributes { () => { #[cfg_attr($condition, no_mangle $($tail)*)] } }\n",
    );
}

#[test]
fn macro_contained_dynamic_cfg_attr_wrapped_attribute_fails() {
    assert_macro_unsafe_attribute_failure(
        "macro_rules! attributes { () => { #[cfg_attr($condition, unsafe(naked) $($tail)*)] } }\n",
    );
}

#[test]
fn macro_contained_full_parse_failed_cfg_attr_attribute_fails() {
    assert_macro_unsafe_attribute_failure(
        "macro_rules! attributes { () => { #[cfg_attr($condition, no_mangle) $($tail)*] } }\n",
    );
}

#[test]
fn macro_contained_recursive_dynamic_cfg_attr_attribute_fails() {
    assert_macro_unsafe_attribute_failure(
        "macro_rules! attributes { () => { #[cfg_attr($outer, cfg_attr($inner, unsafe(naked) $($tail)*) $($rest)*)] } }\n",
    );
}

#[test]
fn macro_contained_deep_parse_failed_attribute_fails() {
    assert_macro_unsafe_attribute_failure("probe!({ ( [ #[no_mangle $($tail)*] ] ) });\n");
}

#[test]
fn macro_parse_failed_findings_preserve_exact_line_order() {
    let workspace = TempWorkspace::basic(
        "macro_rules! attributes {\n\
         () => {\n\
         #[cfg_attr(\n\
         $condition,\n\
         no_mangle $tail,\n\
         unsafe(naked) $tail,\n\
         export_name = \"callback\" $tail\n\
         )]\n\
         };\n\
         }\n",
    );
    let error = run_fixture(&workspace, &[]).expect_err("fixture should violate policy");
    let diagnostics = [
        "crates/app/src/lib.rs:5: macro token unsafe attribute is outside an approved unsafe boundary",
        "crates/app/src/lib.rs:6: macro token unsafe attribute is outside an approved unsafe boundary",
        "crates/app/src/lib.rs:7: macro token unsafe attribute is outside an approved unsafe boundary",
    ];
    let positions = diagnostics.map(|diagnostic| {
        error
            .find(diagnostic)
            .unwrap_or_else(|| panic!("missing diagnostic {diagnostic:?}:\n{error}"))
    });
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "parse-failed attribute diagnostics are not in source order:\n{error}"
    );
}

#[test]
fn macro_parse_failed_mixed_findings_preserve_source_order() {
    let workspace = TempWorkspace::basic(
        "macro_rules! attributes {\n\
         () => {\n\
         #[unsafe(no_mangle) $tail]\n\
         unsafe {}\n\
         #[cfg_attr($condition, unsafe(naked) $tail)]\n\
         };\n\
         }\n",
    );
    let error = assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:3: macro token unsafe attribute is outside an approved unsafe boundary",
    );
    assert!(
        error.contains(
            "crates/app/src/lib.rs:4: macro token unsafe is outside an approved unsafe boundary"
        ),
        "literal unsafe keyword line was misattributed:\n{error}"
    );
    assert!(
        error.contains(
            "crates/app/src/lib.rs:5: macro token unsafe attribute is outside an approved unsafe boundary"
        ),
        "dynamic wrapped attribute line was misattributed:\n{error}"
    );
}

#[test]
fn excluded_cfg_attr_condition_does_not_steal_emitted_attribute_line() {
    let workspace = TempWorkspace::basic(
        "macro_rules! attributes {\n\
         () => {\n\
         #[cfg_attr(\n\
         no_mangle $condition,\n\
         no_mangle\n\
         )]\n\
         };\n\
         }\n",
    );
    let error = assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:5: macro token unsafe attribute is outside an approved unsafe boundary",
    );
    assert!(
        !error.contains(
            "crates/app/src/lib.rs:4: macro token unsafe attribute is outside an approved unsafe boundary"
        ),
        "cfg_attr condition was reported or stole the emitted attribute line:\n{error}"
    );
}

#[test]
fn macro_contained_direct_no_mangle_attribute_fails() {
    assert_macro_unsafe_attribute_failure(
        "macro_rules! attributes { () => { #[no_mangle] pub extern \"C\" fn callback() {} } }\n",
    );
}

#[test]
fn macro_contained_direct_export_name_attribute_fails() {
    assert_macro_unsafe_attribute_failure(
        "macro_rules! attributes { () => { #[export_name = \"callback\"] pub extern \"C\" fn exported() {} } }\n",
    );
}

#[test]
fn macro_contained_direct_link_section_attribute_fails() {
    assert_macro_unsafe_attribute_failure(
        "macro_rules! attributes { () => { #[link_section = \".solstone\"] pub static CALLBACK: u8 = 0; } }\n",
    );
}

#[test]
fn macro_contained_wrapped_no_mangle_attribute_fails() {
    assert_macro_unsafe_attribute_failure(
        "macro_rules! attributes { () => { #[unsafe(no_mangle)] pub extern \"C\" fn callback() {} } }\n",
    );
}

#[test]
fn macro_contained_wrapped_export_name_attribute_fails() {
    assert_macro_unsafe_attribute_failure(
        "macro_rules! attributes { () => { #[unsafe(export_name = \"callback\")] pub extern \"C\" fn exported() {} } }\n",
    );
}

#[test]
fn macro_contained_wrapped_link_section_attribute_fails() {
    assert_macro_unsafe_attribute_failure(
        "macro_rules! attributes { () => { #[unsafe(link_section = \".solstone\")] pub static CALLBACK: u8 = 0; } }\n",
    );
}

#[test]
fn macro_contained_wrapped_naked_attribute_fails() {
    assert_macro_unsafe_attribute_failure(
        "macro_rules! attributes { () => { #[unsafe(naked)] pub extern \"C\" fn callback() {} } }\n",
    );
}

#[test]
fn macro_contained_nested_cfg_attr_direct_attribute_fails() {
    assert_macro_unsafe_attribute_failure(
        "macro_rules! attributes { () => { #[cfg_attr(any(), no_mangle)] pub extern \"C\" fn callback() {} } }\n",
    );
}

#[test]
fn macro_contained_nested_cfg_attr_wrapped_attribute_fails() {
    assert_macro_unsafe_attribute_failure(
        "macro_rules! attributes { () => { #[cfg_attr(any(), unsafe(naked))] pub extern \"C\" fn callback() {} } }\n",
    );
}

#[test]
fn macro_contained_deep_cfg_attr_direct_attribute_fails() {
    assert_macro_unsafe_attribute_failure(
        "probe!({ ( [ #[cfg_attr(any(), cfg_attr(any(), cfg_attr(any(), no_mangle)))] ] ) });\n",
    );
}

#[test]
fn macro_contained_deep_cfg_attr_wrapped_attribute_fails() {
    assert_macro_unsafe_attribute_failure(
        "probe!({ ( [ #[cfg_attr(any(), cfg_attr(any(), cfg_attr(any(), unsafe(naked))))] ] ) });\n",
    );
}

#[test]
fn macro_contained_wrapped_attribute_reports_once() {
    let workspace = TempWorkspace::basic(
        "macro_rules! attributes { () => { #[unsafe(no_mangle)] pub extern \"C\" fn callback() {} } }\n",
    );
    let error = run_fixture(&workspace, &[]).expect_err("fixture should violate policy");
    assert_eq!(
        error
            .matches("macro token unsafe attribute is outside an approved unsafe boundary")
            .count(),
        1,
        "wrapped macro attribute should be reported exactly once:\n{error}"
    );
    assert!(
        !error.contains("macro token unsafe is outside an approved unsafe boundary"),
        "wrapped macro attribute must not also be reported as an unsafe keyword:\n{error}"
    );
}

#[test]
fn macro_mixed_unsafe_findings_preserve_line_attribution() {
    let workspace = TempWorkspace::basic(
        "macro_rules! attributes {\n\
         () => {\n\
         #[unsafe(no_mangle)]\n\
         unsafe {}\n\
         #[unsafe(export_name = \"callback\")]\n\
         };\n\
         }\n",
    );
    let error = assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:3: macro token unsafe attribute is outside an approved unsafe boundary",
    );
    assert!(
        error.contains(
            "crates/app/src/lib.rs:4: macro token unsafe is outside an approved unsafe boundary"
        ),
        "genuine unsafe keyword line was misattributed:\n{error}"
    );
    assert!(
        error.contains(
            "crates/app/src/lib.rs:5: macro token unsafe attribute is outside an approved unsafe boundary"
        ),
        "second wrapped attribute line was misattributed:\n{error}"
    );
}

#[test]
fn macro_contained_unsafe_attributes_inside_approved_boundary_passes() {
    let workspace = TempWorkspace::basic(
        "#[allow(unsafe_code)]\n\
         mod approved {\n\
         macro_rules! attributes { () => { #[no_mangle] #[unsafe(naked)] pub extern \"C\" fn callback() {} } }\n\
         }\n",
    );
    run_fixture(&workspace, FIXTURE_APPROVED_MOD)
        .expect("macro-contained unsafe attributes inside an approved boundary should pass");
}

#[test]
fn macro_contained_dynamic_attributes_inside_approved_boundary_pass() {
    let workspace = TempWorkspace::basic(
        "#[allow(unsafe_code)]\n\
         mod approved {\n\
         macro_rules! attributes { () => { #[no_mangle $tail] #[cfg_attr($condition, unsafe(naked) $tail)] } }\n\
         }\n",
    );
    run_fixture(&workspace, FIXTURE_APPROVED_MOD)
        .expect("dynamic macro attributes inside an approved boundary should pass");
}

#[test]
fn new_macro_witnesses_inside_approved_boundary_pass() {
    let workspace = TempWorkspace::basic(
        "#[allow(unsafe_code)]\n\
         mod approved {\n\
         macro_rules! attributes { () => { #[unsafe($attr)] #[$($prefix)* no_mangle] } }\n\
         }\n",
    );
    run_fixture(&workspace, FIXTURE_APPROVED_MOD)
        .expect("new macro witnesses inside an approved boundary should pass");
}

#[test]
fn macro_attribute_literal_text_passes() {
    let workspace = TempWorkspace::basic("macro_rules! input { () => { \"#[no_mangle]\" } }\n");
    run_fixture(&workspace, &[]).expect("literal unsafe-attribute text must not match");
}

#[test]
fn macro_parse_failed_attribute_literal_and_comment_text_passes() {
    let workspace = TempWorkspace::basic(
        "macro_rules! input { () => { #[doc = \"no_mangle export_name link_section\" $tail] #[doc /* no_mangle export_name link_section */ $tail] } }\n",
    );
    run_fixture(&workspace, &[])
        .expect("literal and comment text in parse-failed safe attributes must not match");
}

#[test]
fn macro_bare_bracket_attribute_name_passes() {
    let workspace = TempWorkspace::basic("macro_rules! input { () => { [no_mangle] } }\n");
    run_fixture(&workspace, &[]).expect("a bare bracket group must not match an attribute");
}

#[test]
fn macro_parse_failed_bare_bracket_attribute_name_passes() {
    let workspace = TempWorkspace::basic("macro_rules! input { () => { [no_mangle $tail] } }\n");
    run_fixture(&workspace, &[])
        .expect("a parse-failed bare bracket group must not match an attribute");
}

#[test]
fn macro_ordinary_unsafe_attribute_name_identifiers_pass() {
    let workspace = TempWorkspace::basic(
        "macro_rules! input { () => { no_mangle!(export_name, link_section); } }\n",
    );
    run_fixture(&workspace, &[])
        .expect("ordinary macro identifiers and arguments must not match attributes");
}

#[test]
fn macro_qualified_unsafe_attribute_name_passes() {
    let workspace = TempWorkspace::basic("macro_rules! input { () => { #[some::no_mangle] } }\n");
    run_fixture(&workspace, &[]).expect("a qualified macro attribute path must not match");
}

#[test]
fn macro_parse_failed_qualified_unsafe_attribute_name_passes() {
    let workspace =
        TempWorkspace::basic("macro_rules! input { () => { #[some::no_mangle $tail] } }\n");
    run_fixture(&workspace, &[]).expect("a parse-failed qualified attribute path must not match");
}

#[test]
fn parsed_qualified_attribute_does_not_steal_direct_attribute_line() {
    let workspace = TempWorkspace::basic(
        "macro_rules! attributes {\n\
         () => {\n\
         #[some::no_mangle]\n\
         #[no_mangle]\n\
         };\n\
         }\n",
    );
    let error = assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:4: macro token unsafe attribute is outside an approved unsafe boundary",
    );
    assert!(
        !error.contains(
            "crates/app/src/lib.rs:3: macro token unsafe attribute is outside an approved unsafe boundary"
        ),
        "qualified attribute was reported or stole the direct attribute line:\n{error}"
    );
}

#[test]
fn parsed_wrapped_attribute_inner_name_does_not_steal_direct_attribute_line() {
    let workspace = TempWorkspace::basic(
        "macro_rules! attributes {\n\
         () => {\n\
         #[unsafe(no_mangle)]\n\
         #[no_mangle]\n\
         };\n\
         }\n",
    );
    let error = assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:3: macro token unsafe attribute is outside an approved unsafe boundary",
    );
    assert!(
        error.contains(
            "crates/app/src/lib.rs:4: macro token unsafe attribute is outside an approved unsafe boundary"
        ),
        "wrapped attribute inner name stole the direct attribute line:\n{error}"
    );
    assert_eq!(
        error
            .matches("macro token unsafe attribute is outside an approved unsafe boundary")
            .count(),
        2,
        "wrapped and direct attributes should each report once:\n{error}"
    );
}

#[test]
fn macro_similar_unsafe_attribute_name_passes() {
    let workspace =
        TempWorkspace::basic("macro_rules! input { () => { #[export_names = \"x\"] } }\n");
    run_fixture(&workspace, &[]).expect("a similar macro attribute name must not match");
}

#[test]
fn macro_parse_failed_similar_unsafe_attribute_name_passes() {
    let workspace =
        TempWorkspace::basic("macro_rules! input { () => { #[export_names $tail] } }\n");
    run_fixture(&workspace, &[]).expect("a similar parse-failed attribute name must not match");
}

#[test]
fn macro_qualified_and_similar_unsafe_wrappers_are_not_attributes() {
    for source in [
        "macro_rules! input { () => { #[some::unsafe($attr)] } }\n",
        "macro_rules! input { () => { #[unsafe::wrapper($attr)] } }\n",
    ] {
        let workspace = TempWorkspace::basic(source);
        let error = assert_fixture_failure(
            &workspace,
            &[],
            "crates/app/src/lib.rs:1: macro token unsafe is outside an approved unsafe boundary",
        );
        assert!(
            !error.contains("macro token unsafe attribute is outside an approved unsafe boundary"),
            "qualified unsafe path must not become an unsafe attribute:\n{error}"
        );
    }
    let workspace =
        TempWorkspace::basic("macro_rules! input { () => { #[unsafe_wrapper($attr)] } }\n");
    run_fixture(&workspace, &[]).expect("a similarly named wrapper must not match");
}

#[test]
fn macro_unsafe_outside_attribute_keeps_keyword_diagnostic() {
    let workspace = TempWorkspace::basic("probe!(unsafe($attr));\n");
    let error = assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:1: macro token unsafe is outside an approved unsafe boundary",
    );
    assert!(
        !error.contains("macro token unsafe attribute is outside an approved unsafe boundary"),
        "unsafe outside an attribute must not use the attribute diagnostic:\n{error}"
    );
}

#[test]
fn malformed_leading_substitution_prefixes_pass() {
    for source in [
        "macro_rules! input { () => { #[$crate no_mangle] } }\n",
        "macro_rules! input { () => { #[$ no_mangle] } }\n",
        "macro_rules! input { () => { #[$($prefix) no_mangle] } }\n",
        "macro_rules! input { () => { #[$()* no_mangle] } }\n",
        "macro_rules! input { () => { #[$(literal)* no_mangle] } }\n",
        "macro_rules! input { () => { #[$($crate)* no_mangle] } }\n",
        "macro_rules! input { () => { #[$($prefix),? no_mangle] } }\n",
        "macro_rules! input { () => { #[$($prefix), no_mangle] } }\n",
        "macro_rules! input { () => { #[$($prefix),;* no_mangle] } }\n",
        "macro_rules! input { () => { #[$[$prefix]* no_mangle] } }\n",
        "macro_rules! input { () => { #[$($prefix)** no_mangle] } }\n",
        "macro_rules! input { () => { #[$($prefix)=> no_mangle] } }\n",
        "macro_rules! input { () => { #[$($prefix)'item no_mangle] } }\n",
        "macro_rules! input { () => { #[$($prefix)'r#item no_mangle] } }\n",
        "macro_rules! input { () => { #[$($prefix)'item? no_mangle] } }\n",
        "macro_rules! input { () => { #[$($prefix)'r#item? no_mangle] } }\n",
        "macro_rules! input { () => { #[$($prefix)'item;* no_mangle] } }\n",
        "macro_rules! input { () => { #[$($prefix)'x' no_mangle] } }\n",
        "macro_rules! input { () => { #[$prefix $crate no_mangle] } }\n",
        "macro_rules! input { () => { #[$($prefix)=>>* no_mangle] } }\n",
    ] {
        let workspace = TempWorkspace::basic(source);
        run_fixture(&workspace, &[])
            .unwrap_or_else(|error| panic!("malformed prefix must not match:\n{source}\n{error}"));
    }
}

#[test]
fn substitution_prefixes_do_not_search_safe_or_qualified_positions() {
    for source in [
        "macro_rules! input { () => { #[$prefix ordinary no_mangle] } }\n",
        "macro_rules! input { () => { #[$prefix some::no_mangle] } }\n",
        "macro_rules! input { () => { #[$prefix export_names] } }\n",
        "macro_rules! input { () => { #[cfg_attr($prefix no_mangle, doc)] } }\n",
        "macro_rules! input { () => { #[cfg_attr(any(), $prefix doc = \"safe\" no_mangle)] } }\n",
        "macro_rules! input { () => { #[cfg_attr(any(), $prefix doc(no_mangle))] } }\n",
    ] {
        let workspace = TempWorkspace::basic(source);
        run_fixture(&workspace, &[]).unwrap_or_else(|error| {
            panic!("safe macro position must not match:\n{source}\n{error}")
        });
    }
}

#[test]
fn raw_identifiers_are_not_literal_unsafe_attribute_paths() {
    let workspace = TempWorkspace::basic(
        "macro_rules! input { () => { #[r#no_mangle] #[r#export_name = \"callback\"] #[r#link_section = \".solstone\"] #[r#unsafe(no_mangle)] } }\n",
    );
    run_fixture(&workspace, &[])
        .expect("raw identifiers must remain locator-only, not unsafe attribute paths");
}

#[test]
fn macro_dynamic_cfg_attr_condition_name_passes() {
    let workspace = TempWorkspace::basic(
        "macro_rules! input { () => { #[cfg_attr(no_mangle $condition, doc)] } }\n",
    );
    run_fixture(&workspace, &[])
        .expect("an unsafe name in a cfg_attr condition must not match an emitted attribute");
}

#[test]
fn macro_dynamic_cfg_attr_safe_value_name_passes() {
    let workspace = TempWorkspace::basic(
        "macro_rules! input { () => { #[cfg_attr(any(), doc = \"safe\" no_mangle $value)] } }\n",
    );
    run_fixture(&workspace, &[]).expect("an unsafe name in a safe attribute value must not match");
}

#[test]
fn macro_dynamic_cfg_attr_safe_list_argument_name_passes() {
    let workspace = TempWorkspace::basic(
        "macro_rules! input { () => { #[cfg_attr(any(), doc(no_mangle) $tail)] } }\n",
    );
    run_fixture(&workspace, &[]).expect("an unsafe name in a safe attribute list must not match");
}

#[test]
fn safe_macro_input_passes() {
    let workspace = TempWorkspace::basic("pub fn values() { vec![1, 2, 3]; }\n");
    run_fixture(&workspace, &[]).expect("ordinary safe macro input should pass");
}

#[test]
fn unsafe_in_build_script_fails() {
    let workspace = TempWorkspace::basic("pub fn app() {}\n");
    workspace.write("crates/app/build.rs", "fn main() { unsafe {} }\n");
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/build.rs:1: unsafe block is outside an approved unsafe boundary",
    );
}

#[test]
fn inner_allow_fails() {
    let workspace = TempWorkspace::basic(
        "mod approved {\n#![allow(unsafe_code)]\npub fn call() { unsafe {} }\n}\n",
    );
    let error = assert_fixture_failure(
        &workspace,
        FIXTURE_APPROVED_MOD,
        "crates/app/src/lib.rs:2: inner allow(unsafe_code) cannot approve an unsafe boundary",
    );
    assert!(
        error.contains("approved node Mod approved is missing its canonical allow(unsafe_code)"),
        "missing approved-node diagnostic:\n{error}"
    );
}

#[test]
fn multi_lint_allow_fails() {
    let workspace = TempWorkspace::basic(
        "#[allow(unsafe_code, dead_code)]\nmod approved { pub fn call() { unsafe {} } }\n",
    );
    let error = assert_fixture_failure(
        &workspace,
        FIXTURE_APPROVED_MOD,
        "crates/app/src/lib.rs:1: noncanonical allow(unsafe_code) on Mod approved",
    );
    assert!(
        error.contains("approved node Mod approved is missing its canonical allow(unsafe_code)"),
        "missing approved-node diagnostic:\n{error}"
    );
}

#[test]
fn cfg_attr_allow_fails() {
    let workspace = TempWorkspace::basic(
        "#[cfg_attr(any(), allow(unsafe_code))]\nmod approved { pub fn call() { unsafe {} } }\n",
    );
    let error = assert_fixture_failure(
        &workspace,
        FIXTURE_APPROVED_MOD,
        "crates/app/src/lib.rs:1: conditional allow(unsafe_code) cannot approve an unsafe boundary",
    );
    assert!(
        error.contains("approved node Mod approved is missing its canonical allow(unsafe_code)"),
        "missing approved-node diagnostic:\n{error}"
    );
}

#[test]
fn expect_unsafe_code_fails() {
    let workspace = TempWorkspace::basic(
        "#[expect(unsafe_code)]\nmod approved { pub fn call() { unsafe {} } }\n",
    );
    let error = assert_fixture_failure(
        &workspace,
        FIXTURE_APPROVED_MOD,
        "crates/app/src/lib.rs:1: expect(unsafe_code) cannot approve an unsafe boundary",
    );
    assert!(
        error.contains("approved node Mod approved is missing its canonical allow(unsafe_code)"),
        "missing approved-node diagnostic:\n{error}"
    );
}

#[test]
fn warn_unsafe_code_fails() {
    let workspace = TempWorkspace::basic(
        "#[warn(unsafe_code)]\nmod approved { pub fn call() { unsafe {} } }\n",
    );
    let error = assert_fixture_failure(
        &workspace,
        FIXTURE_APPROVED_MOD,
        "crates/app/src/lib.rs:1: warn(unsafe_code) cannot approve an unsafe boundary",
    );
    assert!(
        error.contains("approved node Mod approved is missing its canonical allow(unsafe_code)"),
        "missing approved-node diagnostic:\n{error}"
    );
}

#[test]
fn malformed_source_fails() {
    let workspace = TempWorkspace::basic("pub fn broken( {\n");
    assert_fixture_failure(&workspace, &[], "crates/app/src/lib.rs: Rust parse failed:");
}

#[test]
fn orphan_rust_source_fails() {
    let workspace = TempWorkspace::basic("pub fn app() {}\n");
    workspace.write("crates/app/src/orphan.rs", "pub fn orphan() {}\n");
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/orphan.rs: orphan .rs unreachable from any Cargo target",
    );
}

#[test]
fn custom_target_resolves_ordinary_sibling() {
    let workspace = TempWorkspace::new();
    workspace.write_workspace(&["crates/app"]);
    workspace.write_member(
        "crates/app",
        "app",
        true,
        "\n[lib]\npath = \"source/entry.rs\"\n",
    );
    workspace.write("crates/app/source/entry.rs", "mod sibling;\n");
    workspace.write(
        "crates/app/source/sibling.rs",
        "pub fn call() { unsafe {} }\n",
    );
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/source/sibling.rs:1: unsafe block is outside an approved unsafe boundary",
    );
}

#[test]
fn target_named_source_directory_passes() {
    let workspace = TempWorkspace::new();
    workspace.write_workspace(&["crates/app"]);
    workspace.write_member(
        "crates/app",
        "app",
        true,
        "\n[lib]\npath = \"target/lib.rs\"\n",
    );
    workspace.write("crates/app/target/lib.rs", "mod helper;\n");
    workspace.write("crates/app/target/helper.rs", "pub fn helper() {}\n");
    run_fixture(&workspace, &[]).expect("a source directory merely named target must be scanned");
}

#[test]
fn literal_path_and_include_pass_once() {
    let workspace = TempWorkspace::basic(
        "#[path = \"../shared/module.rs\"]\nmod relocated;\n\
         include!(\"../shared/included.rs\");\ninclude!(\"../shared/included.rs\");\n",
    );
    workspace.write("crates/app/shared/module.rs", "pub fn relocated() {}\n");
    workspace.write("crates/app/shared/included.rs", "pub fn included() {}\n");
    run_fixture(&workspace, &[]).expect("literal path/include sources should reconcile once");
}

#[test]
fn dynamic_include_fails() {
    let workspace = TempWorkspace::basic("include!(concat!(\"included\", \".rs\"));\n");
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:1: dynamic include! is unsupported",
    );
}

#[test]
fn dynamic_path_attribute_fails() {
    let workspace = TempWorkspace::basic("#[path = concat!(\"module\", \".rs\")]\nmod dynamic;\n");
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:1: dynamic #[path] is unsupported",
    );
}

#[test]
fn escaping_module_path_fails() {
    let workspace = TempWorkspace::basic("#[path = \"../../../outside.rs\"]\nmod escaped;\n");
    workspace.write("outside.rs", "pub fn outside() {}\n");
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:2: resolved source escapes member root: outside.rs",
    );
}

#[test]
fn escaping_target_path_fails() {
    let workspace = TempWorkspace::new();
    workspace.write_workspace(&["crates/app"]);
    workspace.write_member(
        "crates/app",
        "app",
        true,
        "\n[lib]\npath = \"../../outside.rs\"\n",
    );
    workspace.write("outside.rs", "pub fn outside() {}\n");
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/Cargo.toml: declared target source escapes member root: outside.rs",
    );
}

#[test]
fn recursive_include_cycle_fails() {
    let workspace = TempWorkspace::basic("include!(\"../loop.rs\");\n");
    workspace.write("crates/app/loop.rs", "include!(\"loop.rs\");\n");
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/loop.rs:1: recursive include! cycle: crates/app/loop.rs",
    );
}

#[test]
fn target_root_self_include_cycle_fails() {
    let workspace = TempWorkspace::basic("include!(\"lib.rs\");\n");
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:1: recursive include! cycle: crates/app/src/lib.rs",
    );
}

#[test]
fn module_cycle_reentering_target_root_fails() {
    let workspace = TempWorkspace::basic("mod a;\n");
    workspace.write("crates/app/src/a.rs", "#[path = \"../lib.rs\"] mod back;\n");
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/a.rs:1: recursive source cycle: crates/app/src/lib.rs",
    );
}

#[test]
fn path_module_cycle_reentering_module_root_fails() {
    let workspace = TempWorkspace::basic("#[path = \"sub/a.rs\"] mod a;\n");
    workspace.write("crates/app/src/sub/a.rs", "mod b;\n");
    workspace.write(
        "crates/app/src/sub/a/b.rs",
        "#[path = \"../../a.rs\"] mod back;\n",
    );
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/sub/a/b.rs:1: recursive source cycle: crates/app/src/sub/a.rs",
    );
}

#[test]
fn missing_module_resolution_fails() {
    let workspace = TempWorkspace::basic("mod missing;\n");
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:1: module missing did not resolve",
    );
}

#[cfg(unix)]
#[test]
fn source_file_symlink_fails() {
    use std::os::unix::fs::symlink;

    let workspace = TempWorkspace::basic("mod linked;\n");
    workspace.write("crates/app/src/real.rs", "pub fn real() {}\n");
    symlink(
        workspace.root.join("crates/app/src/real.rs"),
        workspace.root.join("crates/app/src/linked.rs"),
    )
    .expect("create source-file symlink fixture");
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:1: resolved source-file symlink is forbidden: crates/app/src/linked.rs",
    );
}

#[cfg(unix)]
#[test]
fn source_directory_symlink_fails() {
    use std::os::unix::fs::symlink;

    let workspace = TempWorkspace::basic("mod linked;\n");
    workspace.write("crates/app/src/real_directory/mod.rs", "pub fn real() {}\n");
    symlink(
        workspace.root.join("crates/app/src/real_directory"),
        workspace.root.join("crates/app/src/linked"),
    )
    .expect("create source-directory symlink fixture");
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/src/lib.rs:1: resolved source-directory symlink is forbidden: crates/app/src/linked",
    );
}

#[cfg(unix)]
#[test]
fn unreferenced_source_file_symlink_fails() {
    use std::os::unix::fs::symlink;

    let workspace = TempWorkspace::basic("pub fn app() {}\n");
    symlink(
        workspace.root.join("crates/app/src/lib.rs"),
        workspace.root.join("crates/app/src/unreferenced.rs"),
    )
    .expect("create unreferenced source-file symlink fixture");
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/Cargo.toml: source-file symlink is forbidden in member source tree: crates/app/src/unreferenced.rs",
    );
}

#[cfg(unix)]
#[test]
fn unreferenced_source_directory_symlink_fails() {
    use std::os::unix::fs::symlink;

    let workspace = TempWorkspace::basic("pub fn app() {}\n");
    workspace.write("crates/app/assets/NOTICE", "fixture asset\n");
    symlink(
        workspace.root.join("crates/app/assets"),
        workspace.root.join("crates/app/src/linked-source"),
    )
    .expect("create unreferenced source-directory symlink fixture");
    assert_fixture_failure(
        &workspace,
        &[],
        "crates/app/Cargo.toml: source-directory symlink is forbidden in member source tree: crates/app/src/linked-source",
    );
}

#[cfg(unix)]
#[test]
fn unrelated_non_rust_file_symlink_passes() {
    use std::os::unix::fs::symlink;

    let workspace = TempWorkspace::basic("pub fn app() {}\n");
    symlink(
        workspace.root.join("crates/app/src/lib.rs"),
        workspace.root.join("crates/app/LICENSE"),
    )
    .expect("create unrelated non-Rust file symlink fixture");
    run_fixture(&workspace, &[]).expect("an unrelated non-Rust file symlink should pass");
}

#[cfg(unix)]
#[test]
fn broken_non_rust_symlink_passes() {
    use std::os::unix::fs::symlink;

    let workspace = TempWorkspace::basic("pub fn app() {}\n");
    symlink(
        workspace.root.join("crates/app/missing-license"),
        workspace.root.join("crates/app/LICENSE"),
    )
    .expect("create broken non-Rust symlink fixture");
    run_fixture(&workspace, &[]).expect("a broken non-Rust symlink should pass");
}

#[cfg(unix)]
#[test]
fn non_rust_symlink_follow_error_fails_closed() {
    use std::os::unix::fs::symlink;

    let workspace = TempWorkspace::basic("pub fn app() {}\n");
    let loop_a = workspace.root.join("crates/app/loop-a");
    let loop_b = workspace.root.join("crates/app/loop-b");
    symlink(&loop_b, &loop_a).expect("create first symlink-loop fixture");
    symlink(&loop_a, &loop_b).expect("create second symlink-loop fixture");

    let expected =
        "crates/app/Cargo.toml: non-Rust symlink target classification failed: crates/app/loop-a:";
    let error = assert_fixture_failure(&workspace, &[], expected);
    let diagnostic = error
        .lines()
        .find(|line| line.contains(expected))
        .expect("classification-failure diagnostic should be present");
    let (_, error_tail) = diagnostic
        .split_once(expected)
        .expect("classification-failure diagnostic should carry its stable prefix");
    assert!(
        !error_tail.trim().is_empty(),
        "classification-failure diagnostic should carry the underlying filesystem error:\n{error}"
    );
}

#[test]
fn comments_and_literals_are_not_policy_syntax() {
    let workspace = TempWorkspace::basic(
        "// unsafe and #[allow(unsafe_code)] are prose here.\n\
         /// unsafe and allow(unsafe_code) remain documentation.\n\
         pub const TEXT: &str = \"unsafe #[allow(unsafe_code)]\";\n\
         pub const RAW: &str = r#\"unsafe allow(unsafe_code)\"#;\n\
         pub fn show() { println!(\"unsafe allow(unsafe_code)\"); }\n",
    );
    run_fixture(&workspace, &[]).expect("comments and string literals are negative controls");
}

#[test]
fn safe_extern_function_is_not_an_extern_block() {
    let workspace = TempWorkspace::basic("pub extern \"C\" fn callback() {}\n");
    run_fixture(&workspace, &[]).expect("safe extern function should not be classified as a block");
}
