// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The vendored native-browser contract: pinned bytes, and the shared vector
//! corpus and frame recipes run through the shared decoder on this platform.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

fn vendor_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/native-browser")
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn every_vendored_file_matches_its_upstream_hash() {
    let root = vendor_root();
    let list = std::fs::read_to_string(root.join("vendored-files.sha256")).unwrap();
    let mut listed = Vec::new();
    for line in list.lines() {
        let (hash, path) = line.split_once("  ").expect("sha256sum line");
        let bytes = std::fs::read(root.join(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
        assert_eq!(sha256_hex(&bytes), hash, "{path} drifted from upstream");
        listed.push(path.to_string());
    }
    // Nothing vendored goes unlisted (Cargo.toml carries the one local change).
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                out.push(
                    p.strip_prefix(base)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    let mut on_disk = Vec::new();
    walk(&root, &root.join("contracts"), &mut on_disk);
    walk(&root, &root.join("crates"), &mut on_disk);
    on_disk.retain(|p| !p.ends_with("native-browser-frame/Cargo.toml"));
    on_disk.sort();
    listed.sort();
    assert_eq!(on_disk, listed);
}

#[test]
fn the_adoption_pin_matches_the_manifest_and_the_crate() {
    let root = vendor_root();
    let adoption: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("adoption.json")).unwrap()).unwrap();
    let manifest_bytes =
        std::fs::read(root.join("contracts/native-browser/manifest.json")).unwrap();
    assert_eq!(adoption["manifest_sha256"], sha256_hex(&manifest_bytes));
    let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).unwrap();
    assert_eq!(adoption["bundle_version"], manifest["bundle_version"]);
    assert_eq!(adoption["wire_protocol"], manifest["wire_protocol"]);
    assert_eq!(
        adoption["journal_schema_sha256"],
        manifest["journal"]["sha256"]
    );
    assert_eq!(
        adoption["journal_schema_revision"],
        manifest["journal"]["revision"]
    );
    assert_eq!(
        adoption["bundle_version"],
        native_browser_frame::BUNDLE_VERSION
    );
    assert_eq!(
        adoption["wire_protocol"],
        native_browser_frame::WIRE_PROTOCOL
    );
    // Every artifact the manifest names hashes as the manifest says.
    for (path, hash) in manifest["artifacts"].as_object().unwrap() {
        let bytes = std::fs::read(root.join(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
        assert_eq!(sha256_hex(&bytes), hash.as_str().unwrap(), "{path}");
    }
}

#[test]
fn the_windows_registration_rows_render_from_the_shared_table() {
    for (channel, host) in [
        ("production", "app.solstone.browser"),
        ("dev", "app.solstone.browser.dev"),
    ] {
        for browser in ["chrome", "edge", "firefox"] {
            let r =
                native_browser_frame::render_registration(channel, browser, "windows", None, None)
                    .unwrap();
            assert_eq!(r.host, host);
            let golden = std::fs::read(vendor_root().join(format!(
                "contracts/native-browser/registration/{channel}/{browser}/windows.json"
            )))
            .unwrap();
            let golden: serde_json::Value = serde_json::from_slice(&golden).unwrap();
            let rendered: serde_json::Value = serde_json::from_str(&r.json).unwrap();
            assert_eq!(rendered, golden, "{channel}/{browser}");
        }
    }
}

fn read_contract(name: &str) -> Vec<u8> {
    std::fs::read(vendor_root().join("contracts/native-browser").join(name)).unwrap()
}

/// Every corpus vector decodes as the contract says: accepted (and
/// re-encodable), unsupported with the named side behind, or refused with the
/// named code and cause, never echoing the vector's sentinel text.
#[test]
fn every_corpus_vector_decodes_as_the_contract_says() {
    use native_browser_frame::{decode, encode, DecodeOutcome, Direction};
    let corpus: Vec<serde_json::Value> =
        serde_json::from_slice(&read_contract("corpus.json")).unwrap();
    assert!(
        corpus.len() >= 100,
        "corpus unexpectedly small: {}",
        corpus.len()
    );
    for v in &corpus {
        let id = v["id"].as_str().unwrap();
        let direction = match v["direction"].as_str().unwrap() {
            "extension_to_host" => Direction::ExtensionToHost,
            "host_to_extension" => Direction::HostToExtension,
            other => panic!("{id}: unknown direction {other}"),
        };
        let payload = v["payload"]
            .as_str()
            .or_else(|| v["raw"].as_str())
            .unwrap_or_else(|| panic!("{id}: no payload"));
        let outcome = decode(payload.as_bytes(), direction);
        match (v["expect"].as_str().unwrap(), &outcome) {
            ("accept", DecodeOutcome::Accept(value)) => {
                assert!(!encode(value).unwrap().is_empty(), "{id}");
            }
            ("unsupported", DecodeOutcome::Unsupported { behind, .. }) => {
                if let Some(expected) = v["behind"].as_str() {
                    assert_eq!(behind, expected, "{id}");
                }
            }
            ("refuse", DecodeOutcome::Refuse(error)) => {
                if let Some(code) = v["code"].as_str() {
                    assert_eq!(error.code, code, "{id}");
                }
                if let Some(cause) = v["cause"].as_str() {
                    assert_eq!(error.cause.as_deref(), Some(cause), "{id}");
                }
                assert!(
                    !error.to_string().contains("ZQ_SENTINEL_do_not_echo"),
                    "{id}"
                );
            }
            (expect, outcome) => panic!("{id}: expected {expect}, got {outcome:?}"),
        }
    }
}

/// The boundary-size frames rebuild to the contract's exact lengths and bytes.
#[test]
fn every_frame_recipe_rebuilds_byte_for_byte() {
    let recipes: Vec<serde_json::Value> =
        serde_json::from_slice(&read_contract("recipes.json")).unwrap();
    assert!(!recipes.is_empty());
    for r in &recipes {
        let id = r["id"].as_str().unwrap();
        let built = native_browser_frame::build_recipe(id).unwrap();
        assert_eq!(
            built.len() as u64,
            r["target_length"].as_u64().unwrap(),
            "{id}"
        );
        assert_eq!(sha256_hex(&built), r["sha256"].as_str().unwrap(), "{id}");
    }
}
