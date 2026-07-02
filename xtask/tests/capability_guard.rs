// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Guard the journal window's no-IPC capability boundary.
//!
//! The journal is external content. It must not gain Tauri IPC/event/window/opener
//! permissions as part of opening the native journal window.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

#[test]
fn journal_window_has_no_tauri_capability_permissions() {
    let capabilities_dir = capabilities_dir();
    let default_path = capabilities_dir.join("default.json");
    let default = read_json(&default_path);
    let default_windows = string_set(&default, "windows", &default_path);
    let expected = BTreeSet::from(["about".to_string(), "settings".to_string()]);
    assert_eq!(
        default_windows, expected,
        "default.json windows must be exactly settings + about"
    );

    for entry in std::fs::read_dir(&capabilities_dir)
        .unwrap_or_else(|e| panic!("read capabilities dir {}: {e}", capabilities_dir.display()))
    {
        let path = entry.expect("read capability dir entry").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }

        let capability = read_json(&path);
        let windows = string_set(&capability, "windows", &path);
        if !windows.contains("journal") {
            continue;
        }

        let permissions = capability
            .get("permissions")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        assert!(
            permissions.is_empty(),
            "{} lists journal but grants permissions: {}",
            path.display(),
            serde_json::to_string(permissions).expect("serialize permissions")
        );
    }
}

fn capabilities_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("src-tauri")
        .join("capabilities")
}

fn read_json(path: &Path) -> Value {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn string_set(value: &Value, key: &str, path: &Path) -> BTreeSet<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{} missing array field `{key}`", path.display()))
        .iter()
        .map(|item| {
            item.as_str()
                .unwrap_or_else(|| panic!("{} field `{key}` contains a non-string", path.display()))
                .to_string()
        })
        .collect()
}
