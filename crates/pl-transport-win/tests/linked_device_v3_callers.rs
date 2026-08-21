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

const BASELINE_CALLERS: &[(&str, &str, CallerClass)] = &[
    (
        "src/client.rs",
        "pub async fn ingest(",
        CallerClass::Migrated,
    ),
    (
        "src/client.rs",
        "pub async fn ingest_manifest(",
        CallerClass::Migrated,
    ),
    (
        "src/client.rs",
        "pub async fn ingest_manifest_day(",
        CallerClass::Migrated,
    ),
    (
        "src/client.rs",
        "pub async fn list_segments(",
        CallerClass::Migrated,
    ),
    (
        "src/coordinator.rs",
        "prove_custody(",
        CallerClass::Migrated,
    ),
    ("src/service.rs", "run_uploader", CallerClass::Migrated),
    (
        "src/integration/ops.rs",
        "async fn upload(",
        CallerClass::Migrated,
    ),
    (
        "../../src-tauri/src/ipc.rs",
        "pl_transport_win::run_uploader",
        CallerClass::Migrated,
    ),
    (
        "../../src-tauri/src/app.rs",
        "pl_transport_win::run_uploader",
        CallerClass::Migrated,
    ),
    (
        "src/client.rs",
        "pub async fn register(",
        CallerClass::Excluded,
    ),
    (
        "src/pairing.rs",
        "pub async fn pair(",
        CallerClass::Excluded,
    ),
    (
        "../../src-tauri/src/ipc.rs",
        "pl_transport_win::service::pair_and_register",
        CallerClass::Excluded,
    ),
    (
        "src/client.rs",
        "pub async fn heartbeat(",
        CallerClass::Excluded,
    ),
    (
        "src/journal_bridge.rs",
        "pub async fn start_observed(",
        CallerClass::Excluded,
    ),
    (
        "../../src-tauri/src/windows.rs",
        "pl_transport_win::journal_bridge::start(",
        CallerClass::Excluded,
    ),
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

#[test]
fn linked_device_v3_callers_are_migrated_or_explicitly_excluded() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for (relative, needle, class) in BASELINE_CALLERS {
        let path = root.join(relative);
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        assert!(
            source.contains(needle),
            "baseline {class:?} caller {relative} no longer contains {needle:?}; classify its replacement here before changing the linked-device boundary"
        );
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
