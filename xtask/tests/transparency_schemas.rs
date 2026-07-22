// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use xtask::transparency_format::{
    validate_transparency_entry_value, validate_transparency_latest_value,
    verify_vendored_transparency_entry_schema, verify_vendored_transparency_latest_schema,
    TRANSPARENCY_ENTRY_SCHEMA_ID, TRANSPARENCY_ENTRY_SCHEMA_SHA256, TRANSPARENCY_LATEST_SCHEMA_ID,
    TRANSPARENCY_LATEST_SCHEMA_SHA256, TRANSPARENCY_PUBLIC_KEY_FILENAME,
    TRANSPARENCY_PUBLIC_KEY_PATH,
};

#[test]
fn transparency_schemas_are_exact_and_validate_runtime_values() {
    let root = repo_root();
    let entry_path = root.join("schemas/transparency-ledger-entry/v1.json");
    let latest_path = root.join("schemas/transparency-latest/v1.json");
    let entry = fs::read(&entry_path).expect("read entry schema");
    let latest = fs::read(&latest_path).expect("read latest schema");
    assert_eq!(entry.len(), 2_805);
    assert_eq!(latest.len(), 1_140);
    assert_eq!(
        lower_hex(&Sha256::digest(&entry)),
        TRANSPARENCY_ENTRY_SCHEMA_SHA256
    );
    assert_eq!(
        lower_hex(&Sha256::digest(&latest)),
        TRANSPARENCY_LATEST_SCHEMA_SHA256
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&entry).unwrap()["$id"],
        TRANSPARENCY_ENTRY_SCHEMA_ID
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&latest).unwrap()["$id"],
        TRANSPARENCY_LATEST_SCHEMA_ID
    );
    assert_eq!(
        TRANSPARENCY_PUBLIC_KEY_FILENAME,
        "solpbc-transparency-1.pub"
    );
    assert_eq!(
        TRANSPARENCY_PUBLIC_KEY_PATH,
        "releases/keys/solpbc-transparency-1.pub"
    );
    verify_vendored_transparency_entry_schema(&root).expect("verify entry schema");
    verify_vendored_transparency_latest_schema(&root).expect("verify latest schema");
    validate_transparency_entry_value(&entry_vector()).expect("validate entry vector");
    validate_transparency_latest_value(&latest_vector()).expect("validate latest vector");
}

#[test]
fn transparency_runtime_schemas_reject_boolean_numeric_fields_and_floats() {
    let mut entry = entry_vector();
    entry["seq"] = json!(true);
    assert!(validate_transparency_entry_value(&entry).is_err());

    let mut entry = entry_vector();
    entry["artifacts"][0]["bytes"] = json!(1.5);
    assert!(validate_transparency_entry_value(&entry).is_err());

    let mut latest = latest_vector();
    latest["chain_length"] = json!(false);
    assert!(validate_transparency_latest_value(&latest).is_err());
}

#[test]
fn transparency_timestamps_require_canonical_utc_seconds() {
    for invalid in ["2026-07-22T00:00:00+00:00", "2026-07-22T00:00:00.1Z"] {
        let mut entry = entry_vector();
        entry["published_utc"] = json!(invalid);
        assert!(validate_transparency_entry_value(&entry).is_err());

        let mut latest = latest_vector();
        latest["signed_at"] = json!(invalid);
        assert!(validate_transparency_latest_value(&latest).is_err());
    }
}

fn entry_vector() -> Value {
    serde_json::from_slice(include_bytes!(
        "fixtures/transparency/entry-vector.canonical.json"
    ))
    .expect("parse entry vector")
}

fn latest_vector() -> Value {
    serde_json::from_slice(include_bytes!(
        "fixtures/transparency/latest-vector.canonical.json"
    ))
    .expect("parse latest vector")
}

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has workspace parent")
        .to_path_buf()
}
