// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Regression guard for the linked-device v3 migration boundary.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallerClass {
    Migrated,
    Excluded,
}

struct BaselineCaller {
    relative: &'static str,
    needle: &'static str,
    class: CallerClass,
    v3_surface: Option<&'static str>,
}

const BASELINE_CALLERS: &[BaselineCaller] = &[
    BaselineCaller {
        relative: "src/client.rs",
        needle: "pub async fn ingest(",
        class: CallerClass::Migrated,
        v3_surface: Some("self.v3_headers()"),
    },
    BaselineCaller {
        relative: "src/client.rs",
        needle: "pub async fn ingest_manifest(",
        class: CallerClass::Migrated,
        v3_surface: Some("self.v3_headers()"),
    },
    BaselineCaller {
        relative: "src/client.rs",
        needle: "pub async fn ingest_manifest_day(",
        class: CallerClass::Migrated,
        v3_surface: Some("self.v3_headers()"),
    },
    BaselineCaller {
        relative: "src/client.rs",
        needle: "pub async fn list_segments(",
        class: CallerClass::Migrated,
        v3_surface: Some("self.v3_headers()"),
    },
    BaselineCaller {
        relative: "src/coordinator.rs",
        needle: "prove_custody(",
        class: CallerClass::Migrated,
        v3_surface: Some("self.client.ingest_manifest().await?"),
    },
    BaselineCaller {
        relative: "src/service.rs",
        needle: "run_uploader",
        class: CallerClass::Migrated,
        v3_surface: Some("setup_uploader("),
    },
    BaselineCaller {
        relative: "src/integration/ops.rs",
        needle: "async fn upload(",
        class: CallerClass::Migrated,
        v3_surface: Some("coordinator.tick_with_witness()"),
    },
    BaselineCaller {
        relative: "../../src-tauri/src/ipc.rs",
        needle: "pl_transport_win::run_uploader",
        class: CallerClass::Migrated,
        v3_surface: Some("pl_transport_win::run_uploader"),
    },
    BaselineCaller {
        relative: "../../src-tauri/src/app.rs",
        needle: "pl_transport_win::run_uploader",
        class: CallerClass::Migrated,
        v3_surface: Some("pl_transport_win::run_uploader"),
    },
    BaselineCaller {
        relative: "src/client.rs",
        needle: "pub async fn register(",
        class: CallerClass::Excluded,
        v3_surface: None,
    },
    BaselineCaller {
        relative: "src/pairing.rs",
        needle: "pub async fn pair(",
        class: CallerClass::Excluded,
        v3_surface: None,
    },
    BaselineCaller {
        relative: "../../src-tauri/src/ipc.rs",
        needle: "pl_transport_win::service::pair_and_register",
        class: CallerClass::Excluded,
        v3_surface: None,
    },
    BaselineCaller {
        relative: "src/client.rs",
        needle: "pub async fn heartbeat(",
        class: CallerClass::Excluded,
        v3_surface: None,
    },
    BaselineCaller {
        relative: "src/journal_bridge.rs",
        needle: "pub async fn start_observed(",
        class: CallerClass::Excluded,
        v3_surface: None,
    },
    BaselineCaller {
        relative: "../../src-tauri/src/windows.rs",
        needle: "pl_transport_win::journal_bridge::start(",
        class: CallerClass::Excluded,
        v3_surface: None,
    },
];

const FORBIDDEN_V2_TOKENS: &[&str] = &[
    "X-Solstone-Observer",
    "Authorization",
    "EXCLUDED_OPERATION_PROTOCOL_VERSION: u32 = 2",
    "name=\"segment\"",
    "name=\"day\"",
    "name=\"platform\"",
    "/app/observer/",
];

/// The two old auth headers and the protocol-v2 constant remain only in
/// `event_headers`/`proxy_headers`, which preserve excluded request bytes.
const EXCLUDED_TOKEN_ALLOWLIST: &[(&str, &str, usize)] = &[
    // `proxy_headers` and its bridge-header filter.
    ("src/client.rs", "X-Solstone-Observer", 2),
    // `event_headers` and `proxy_headers`.
    ("src/client.rs", "Authorization", 2),
    // The heartbeat/bridge-only protocol-version constant.
    (
        "src/client.rs",
        "EXCLUDED_OPERATION_PROTOCOL_VERSION: u32 = 2",
        1,
    ),
];

/// Every baseline caller deliberately left outside the v3 upload/reconciliation
/// surface. A new exclusion must name its preserved operation here rather than
/// silently reclassifying a linked-device caller.
const EXCLUDED_CALLER_ALLOWLIST: &[(&str, &str)] = &[
    ("src/client.rs", "pub async fn register("),
    ("src/pairing.rs", "pub async fn pair("),
    (
        "../../src-tauri/src/ipc.rs",
        "pl_transport_win::service::pair_and_register",
    ),
    ("src/client.rs", "pub async fn heartbeat("),
    ("src/journal_bridge.rs", "pub async fn start_observed("),
    (
        "../../src-tauri/src/windows.rs",
        "pl_transport_win::journal_bridge::start(",
    ),
];

#[test]
fn linked_device_v3_callers_are_migrated_or_explicitly_excluded() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for caller in BASELINE_CALLERS {
        let path = root.join(caller.relative);
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        assert!(
            source.contains(caller.needle),
            "baseline {:?} caller {} no longer contains {:?}; classify its replacement here before changing the linked-device boundary",
            caller.class,
            caller.relative,
            caller.needle,
        );

        match caller.class {
            CallerClass::Migrated => {
                let v3_surface = caller
                    .v3_surface
                    .expect("a migrated caller must name the v3 route/header surface it reaches");
                assert!(
                    source.contains(v3_surface),
                    "migrated caller {} ({:?}) no longer reaches v3 surface {:?}; restore its v3 path or reclassify it explicitly",
                    caller.relative,
                    caller.needle,
                    v3_surface,
                );
                let scope = caller_scope(&source, caller.needle);
                for token in FORBIDDEN_V2_TOKENS {
                    assert!(
                        !scope.contains(token),
                        "migrated caller {} ({:?}) retains forbidden v2 token {:?}; move it to an excluded operation or migrate the caller fully",
                        caller.relative,
                        caller.needle,
                        token,
                    );
                }
            }
            CallerClass::Excluded => {
                assert!(
                    caller.v3_surface.is_none(),
                    "excluded caller {} ({:?}) must not claim a v3 surface",
                    caller.relative,
                    caller.needle,
                );
                assert!(
                    EXCLUDED_CALLER_ALLOWLIST
                        .iter()
                        .any(|(file, needle)| *file == caller.relative && *needle == caller.needle),
                    "excluded caller {} ({:?}) is not in EXCLUDED_CALLER_ALLOWLIST; migrate it to v3 or add a narrowly named preserved-operation entry",
                    caller.relative,
                    caller.needle,
                );
            }
        }
    }

    for (prefix, directory) in [
        ("src/", root.join("src")),
        ("../../src-tauri/src/", root.join("../../src-tauri/src")),
    ] {
        for path in rust_sources(&directory) {
            let relative = format!(
                "{prefix}{}",
                path.strip_prefix(&directory)
                    .expect("source is under scanned root")
                    .to_string_lossy()
                    .replace('\\', "/")
            );
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            for token in FORBIDDEN_V2_TOKENS {
                let occurrences = source.match_indices(token).count();
                if occurrences == 0 {
                    continue;
                }
                if let Some((_, _, allowed_occurrences)) = EXCLUDED_TOKEN_ALLOWLIST
                    .iter()
                    .find(|(file, allowed, _)| *file == relative && *allowed == *token)
                {
                    assert_eq!(
                        occurrences, *allowed_occurrences,
                        "excluded-operation token {token:?} changed in {relative}; preserve its exact heartbeat/proxy scope or migrate the new caller to v3"
                    );
                } else {
                    panic!(
                        "forbidden v2 token {token:?} in {relative}; migrate this linked-device caller to v3, or add a narrowly named excluded-operation allowlist entry with its preserved-wire justification"
                    );
                }
            }
        }
    }
}

fn caller_scope<'a>(source: &'a str, needle: &str) -> &'a str {
    let start = source
        .find(needle)
        .unwrap_or_else(|| panic!("baseline caller no longer contains {needle:?}"));
    let Some(open) = source[start..].find('{').map(|offset| start + offset) else {
        return source;
    };
    let mut depth = 0usize;
    for (offset, byte) in source[open..].bytes().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[start..=open + offset];
                }
            }
            _ => {}
        }
    }
    panic!("baseline caller {needle:?} has an unclosed item body")
}

fn rust_sources(directory: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(directory).expect("read source directory") {
        let path = entry.expect("read source entry").path();
        if path.is_dir() {
            paths.extend(rust_sources(&path));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            paths.push(path);
        }
    }
    paths.sort();
    paths
}
