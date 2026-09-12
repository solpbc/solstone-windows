// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Lock-bound check for the committed Windows app Rust dependency notices.

use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};

pub const SCHEMA: &str = "solstone.windows-app-rust-notices.v1";
pub const PREBUILT_SCHEMA: &str = "solstone.windows-app-prebuilt-artifacts.v1";
pub const INDEX_RELATIVE: &str = "packaging/rust-notices/index.json";
pub const PREBUILT_RELATIVE: &str = "packaging/rust-notices/prebuilt-artifacts.json";
pub const NOTICES_RELATIVE: &str = "RUST_DEPENDENCY_NOTICES.txt";

pub fn validate(index: &[u8], notices: &[u8], lock_sha256: &str) -> Result<(), String> {
    let index: serde_json::Value =
        serde_json::from_slice(index).map_err(|error| error.to_string())?;
    if index["schema"].as_str() != Some(SCHEMA)
        || index["cargo_lock_sha256"].as_str() != Some(lock_sha256)
        || index["notices_sha256"].as_str() != Some(&sha256_hex(notices))
    {
        return Err(
            "Rust dependency notices do not match the current lock and notice bytes".into(),
        );
    }
    Ok(())
}

pub fn check_repo(root: &Path) -> Result<(), String> {
    let lock = fs::read(root.join("Cargo.lock")).map_err(|error| error.to_string())?;
    let index = fs::read(root.join(INDEX_RELATIVE)).map_err(|error| error.to_string())?;
    let notices = fs::read(root.join(NOTICES_RELATIVE)).map_err(|error| error.to_string())?;
    validate(&index, &notices, &sha256_hex(&lock))
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_rust_notices_match_workspace_lock() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask lives under the workspace");
        check_repo(root).expect("refresh Rust dependency notices when Cargo.lock changes");
    }

    #[test]
    fn prebuilt_allowlist_packages_are_in_the_shipped_set() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask lives under the workspace");
        let allowlist: serde_json::Value = serde_json::from_slice(
            &fs::read(root.join(PREBUILT_RELATIVE)).expect("read prebuilt allowlist"),
        )
        .expect("parse prebuilt allowlist");
        assert_eq!(allowlist["schema"].as_str(), Some(PREBUILT_SCHEMA));
        let index: serde_json::Value = serde_json::from_slice(
            &fs::read(root.join(INDEX_RELATIVE)).expect("read notices index"),
        )
        .expect("parse notices index");
        let shipped: std::collections::HashSet<(String, String)> = index["packages"]
            .as_array()
            .expect("packages array")
            .iter()
            .map(|package| {
                (
                    package["name"].as_str().expect("package name").to_string(),
                    package["version"]
                        .as_str()
                        .expect("package version")
                        .to_string(),
                )
            })
            .collect();
        for row in allowlist["packages"]
            .as_array()
            .expect("prebuilt packages array")
        {
            let name = row["name"].as_str().expect("prebuilt name");
            let version = row["version"].as_str().expect("prebuilt version");
            assert!(
                shipped.contains(&(name.to_string(), version.to_string())),
                "{name}@{version} is on the prebuilt allowlist but not in the shipped notices set"
            );
        }
    }

    #[test]
    fn stale_lock_or_changed_notices_refuse() {
        let notices = b"original upstream notices";
        let index = serde_json::json!({
            "schema": SCHEMA,
            "cargo_lock_sha256": "current-lock",
            "notices_sha256": sha256_hex(notices),
        });
        let encoded = serde_json::to_vec(&index).expect("encode index");
        assert!(validate(&encoded, notices, "current-lock").is_ok());
        assert!(validate(&encoded, notices, "changed-lock").is_err());
        assert!(validate(&encoded, b"replaced", "current-lock").is_err());
        assert!(validate(b"{}", notices, "current-lock").is_err());
        assert!(validate(b"not-json", notices, "current-lock").is_err());
    }
}
