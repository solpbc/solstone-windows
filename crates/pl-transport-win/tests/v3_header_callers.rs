// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Regression guard for v3 request headers and retired linked-device identity.

use std::fs;
use std::path::{Path, PathBuf};

struct BaselineCaller {
    relative: &'static str,
    needle: &'static str,
    v3_surface: &'static str,
}

const BASELINE_CALLERS: &[BaselineCaller] = &[
    BaselineCaller {
        relative: "src/client.rs",
        needle: "pub async fn ingest(",
        v3_surface: "self.v3_headers()",
    },
    BaselineCaller {
        relative: "src/client.rs",
        needle: "pub async fn ingest_manifest(",
        v3_surface: "self.v3_headers()",
    },
    BaselineCaller {
        relative: "src/client.rs",
        needle: "pub async fn ingest_manifest_day(",
        v3_surface: "self.v3_headers()",
    },
    BaselineCaller {
        relative: "src/client.rs",
        needle: "pub async fn list_segments(",
        v3_surface: "self.v3_headers()",
    },
    BaselineCaller {
        relative: "src/coordinator.rs",
        needle: "prove_custody(",
        v3_surface: "self.client.ingest_manifest().await?",
    },
    BaselineCaller {
        relative: "src/service.rs",
        needle: "run_uploader",
        v3_surface: "setup_uploader(",
    },
    BaselineCaller {
        relative: "src/integration/ops.rs",
        needle: "async fn upload(",
        v3_surface: "coordinator.tick_with_witness()",
    },
    BaselineCaller {
        relative: "../../src-tauri/src/ipc.rs",
        needle: "pl_transport_win::run_uploader",
        v3_surface: "pl_transport_win::run_uploader",
    },
    BaselineCaller {
        relative: "../../src-tauri/src/app.rs",
        needle: "pl_transport_win::run_uploader",
        v3_surface: "pl_transport_win::run_uploader",
    },
    BaselineCaller {
        relative: "src/client.rs",
        needle: "pub(crate) fn proxy_headers(",
        v3_surface: "self.v3_headers()",
    },
    BaselineCaller {
        relative: "src/journal_bridge_carrier.rs",
        needle: "async fn open_stream(",
        v3_surface: "self.client.proxy_headers(upstream_headers)",
    },
];

const FORBIDDEN_RETIRED_TOKENS: &[&str] = &[
    "paths::REGISTER",
    "paths::INGEST_EVENT",
    "RegisterRequest",
    "RegisterResponse",
    "run_heartbeat",
    "HEARTBEAT_INTERVAL_SECS",
    "EXCLUDED_OPERATION_PROTOCOL_VERSION",
    "with_observer_key",
    "observer_key",
    "Bearer {key}",
    "name=\"segment\"",
    "name=\"day\"",
    "name=\"platform\"",
    "/app/observer/",
];

#[test]
fn v3_callers_use_v3_headers_and_retired_identity_is_absent() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for caller in BASELINE_CALLERS {
        let path = root.join(caller.relative);
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        assert!(
            source.contains(caller.needle),
            "baseline v3 caller {} no longer contains {:?}; classify its replacement here before changing the linked-device boundary",
            caller.relative,
            caller.needle,
        );

        assert!(
            source.contains(caller.v3_surface),
            "v3 caller {} ({:?}) no longer reaches v3 surface {:?}",
            caller.relative,
            caller.needle,
            caller.v3_surface,
        );
        let scope = caller_scope(&source, caller.needle);
        for token in FORBIDDEN_RETIRED_TOKENS {
            assert!(
                !scope.contains(token),
                "v3 caller {} ({:?}) retains retired token {:?}",
                caller.relative,
                caller.needle,
                token,
            );
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
            let production = production_source(&source);
            for token in FORBIDDEN_RETIRED_TOKENS {
                assert!(
                    !production.contains(token),
                    "retired token {token:?} remains in {relative}"
                );
            }
        }
    }

    let client = fs::read_to_string(root.join("src/client.rs")).unwrap();
    let proxy_scope = caller_scope(&client, "pub(crate) fn proxy_headers(");
    assert!(proxy_scope.contains("self.v3_headers()"));
    assert!(proxy_scope.contains("!is_observer_auth_header(name)"));
    assert!(!proxy_scope.contains("Authorization"));
    assert!(!proxy_scope.contains("X-Solstone-Observer"));
    assert!(!proxy_scope.contains("Bearer"));
}

fn production_source(source: &str) -> &str {
    source.split("\n#[cfg(test)]").next().unwrap_or(source)
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
