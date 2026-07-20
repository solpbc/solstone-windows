// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use proc_macro2::{TokenStream, TokenTree};
use serde_json::Value;
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{
    Attribute, Expr, ForeignItemFn, ForeignItemStatic, ImplItemFn, ItemFn, ItemForeignMod,
    ItemImpl, ItemMod, ItemStatic, ItemTrait, Lit, LitStr, Macro, Meta, Token, TraitItemFn, Type,
};

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
        form_count: usize,
        source_path: &str,
        line: usize,
        scope: Option<&NodeIdentity>,
    ) {
        for _ in 0..form_count {
            self.record_unsafe("unsafe attribute", source_path, line, scope);
        }
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
        for attribute in attributes {
            let form_count = unsafe_attribute_form_count(attribute);
            let line = if form_count > 0 {
                self.locator.next("unsafe")
            } else {
                node_line
            };
            self.policy.inspect_unsafe_attribute(
                form_count,
                self.source_path,
                line,
                effective_scope.as_ref(),
            );
        }
        effective_scope
    }

    fn inspect_ordinary_attribute(&mut self, attribute: &Attribute) {
        let unsafe_attribute_count = unsafe_attribute_form_count(attribute);
        let line = if attribute_mentions_unsafe_code(attribute) {
            self.locator.next("unsafe_code")
        } else if unsafe_attribute_count > 0 {
            self.locator.next("unsafe")
        } else {
            1
        };
        self.policy
            .inspect_level_attribute(attribute, None, self.source_path, line);
        self.policy.inspect_unsafe_attribute(
            unsafe_attribute_count,
            self.source_path,
            line,
            self.scope.as_ref(),
        );
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
        let mut nested_count = 0;
        count_unsafe_identifiers(&node.tokens, &mut nested_count);
        for _ in 0..nested_count {
            self.record_unsafe("macro token unsafe", "unsafe");
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
    visited: BTreeMap<PathBuf, VisitRecord>,
    visited_rs: Vec<BTreeSet<PathBuf>>,
    active_includes: Vec<PathBuf>,
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
            active_includes: Vec::new(),
            policy: PolicyState::new(approved)?,
        })
    }

    fn build_inventory(&mut self) -> Result<(), String> {
        for index in 0..self.members.len() {
            let root = self.members[index].root.clone();
            collect_rust_inventory(&root, &self.target_directory, &mut self.inventories[index])
                .map_err(|error| {
                    format!(
                        "{}: source inventory failed: {error}",
                        display_path(&self.workspace_root, &root)
                    )
                })?;
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
        if is_include && self.active_includes.contains(&path) {
            self.policy.violations.push(format!(
                "{referring_source}:{referring_line}: recursive include! cycle: {}",
                display_path(&self.workspace_root, &path)
            ));
            return;
        }
        if let Some(previous) = self.visited.get(&path) {
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
            path.clone(),
            VisitRecord {
                member_index,
                scope: scope.clone(),
            },
        );
        if path.extension().and_then(OsStr::to_str) == Some("rs") {
            self.visited_rs[member_index].insert(path.clone());
        }
        if is_include {
            self.active_includes.push(path.clone());
        }

        let source_path = display_path(&self.workspace_root, &path);
        let source = match fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                self.policy
                    .violations
                    .push(format!("{source_path}: source read/UTF-8 failed: {error}"));
                if is_include {
                    self.active_includes.pop();
                }
                return;
            }
        };
        let file = match syn::parse_file(&source) {
            Ok(file) => file,
            Err(error) => {
                self.policy
                    .violations
                    .push(format!("{source_path}: Rust parse failed: {error}"));
                if is_include {
                    self.active_includes.pop();
                }
                return;
            }
        };
        let Some(physical_dir) = path.parent() else {
            self.policy
                .violations
                .push(format!("{source_path}: source has no parent directory"));
            if is_include {
                self.active_includes.pop();
            }
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
        if is_include {
            self.active_includes.pop();
        }
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
        if !candidate.starts_with(member_root) {
            self.policy.violations.push(format!(
                "{source_path}:{line}: resolved source escapes member root: {}",
                display_path(&self.workspace_root, candidate)
            ));
            return None;
        }
        let relative = candidate.strip_prefix(member_root).ok()?;
        let components: Vec<_> = relative.components().collect();
        let mut current = member_root.clone();
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
        let canonical = match fs::canonicalize(candidate) {
            Ok(path) => path,
            Err(error) => {
                self.policy.violations.push(format!(
                    "{source_path}:{line}: resolved source canonicalization failed: {}: {error}",
                    display_path(&self.workspace_root, candidate)
                ));
                return None;
            }
        };
        if !canonical.starts_with(member_root) {
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
            for orphan in self.inventories[index].difference(&self.visited_rs[index]) {
                self.policy.violations.push(format!(
                    "{}: orphan .rs unreachable from any Cargo target",
                    display_path(&self.workspace_root, orphan)
                ));
            }
            for unexpected in self.visited_rs[index].difference(&self.inventories[index]) {
                self.policy.violations.push(format!(
                    "{}: visited Rust source is absent from member inventory",
                    display_path(&self.workspace_root, unexpected)
                ));
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
        .args(["metadata", "--locked", "--format-version", "1", "--no-deps"])
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
    if metadata_root != requested_root {
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
        let manifest_path = fs::canonicalize(manifest_path).map_err(|error| {
            format!(
                "unsafe-policy: manifest_path {} is unavailable: {error}",
                normalize_path_display(Path::new(manifest_path))
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
        if !root.starts_with(workspace_root) {
            return Err(format!(
                "unsafe-policy: member root {} escapes workspace root {}",
                display_path(workspace_root, &root),
                normalize_path_display(workspace_root)
            ));
        }
        if root.starts_with(target_directory) {
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
            if !lexical.starts_with(&root) {
                return Err(format!(
                    "unsafe-policy: {}: declared target source escapes member root: {}",
                    display_path(workspace_root, &manifest_path),
                    display_path(workspace_root, &lexical)
                ));
            }
            ensure_no_symlink_components(&root, &lexical).map_err(|(kind, path)| {
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
            if !src_path.starts_with(&root) || !src_path.is_file() {
                return Err(format!(
                    "unsafe-policy: target source {} is not a regular in-member file",
                    display_path(workspace_root, &src_path)
                ));
            }
            target_sources.push(TargetSource { path: src_path });
        }
        target_sources.sort_by(|left, right| left.path.cmp(&right.path));
        target_sources.dedup_by(|left, right| left.path == right.path);
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
            if member.root.starts_with(&other.root) || other.root.starts_with(&member.root) {
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

fn collect_rust_inventory(
    directory: &Path,
    target_directory: &Path,
    output: &mut BTreeSet<PathBuf>,
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
            continue;
        }
        if metadata.is_dir() {
            let canonical = fs::canonicalize(&path).map_err(|error| {
                format!("canonicalize {}: {error}", normalize_path_display(&path))
            })?;
            if canonical.starts_with(target_directory) {
                continue;
            }
            collect_rust_inventory(&canonical, target_directory, output)?;
        } else if metadata.is_file() && path.extension().and_then(OsStr::to_str) == Some("rs") {
            let canonical = fs::canonicalize(&path).map_err(|error| {
                format!("canonicalize {}: {error}", normalize_path_display(&path))
            })?;
            if !canonical.starts_with(target_directory) {
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
    let Ok(relative) = candidate.strip_prefix(root) else {
        return Ok(());
    };
    let components: Vec<_> = relative.components().collect();
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

fn unsafe_attribute_form_count(attribute: &Attribute) -> usize {
    if attribute.path().is_ident("unsafe") {
        return 1;
    }
    if !attribute.path().is_ident("cfg_attr") {
        return 0;
    }
    nested_unsafe_attribute_form_count(&attribute.meta)
}

fn nested_unsafe_attribute_form_count(meta: &Meta) -> usize {
    let Meta::List(list) = meta else {
        return 0;
    };
    if list.path.is_ident("unsafe") {
        return 1;
    }
    if !list.path.is_ident("cfg_attr") {
        return 0;
    }
    let parser = Punctuated::<Meta, Token![,]>::parse_terminated;
    match parser.parse2(list.tokens.clone()) {
        Ok(arguments) => arguments
            .iter()
            .skip(1)
            .map(nested_unsafe_attribute_form_count)
            .sum(),
        Err(_) => usize::from(token_stream_contains_ident(&list.tokens, "unsafe")),
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

fn count_unsafe_identifiers(tokens: &TokenStream, count: &mut usize) {
    for token in tokens.clone() {
        match token {
            TokenTree::Ident(ident) if ident == "unsafe" => *count += 1,
            TokenTree::Group(group) => count_unsafe_identifiers(&group.stream(), count),
            TokenTree::Ident(_) | TokenTree::Punct(_) | TokenTree::Literal(_) => {}
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
    let shown = path.strip_prefix(workspace_root).unwrap_or(path);
    normalize_path_display(shown)
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
