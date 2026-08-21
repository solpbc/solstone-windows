// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use xtask::observer_contract::{FIXTURE_IDS, VECTOR_IDS};

#[allow(dead_code)] // The shared helper is compiled separately into each integration-test binary.
pub fn v3_upload_capture_matches(
    request: &[u8],
    day: &str,
    segment: &str,
    filenames: &[&str],
) -> bool {
    let text = String::from_utf8_lossy(request);
    let submitted = filenames
        .iter()
        .map(|filename| format!(r#"{{"submitted":"{filename}"}}"#))
        .collect::<Vec<_>>()
        .join(",");
    text.starts_with("POST /app/devices/ingest HTTP/1.1\r\n")
        && text.contains("X-Solstone-Protocol-Version: 3\r\n")
        && !text.contains("X-Solstone-Protocol-Version: 2\r\n")
        && text.contains("Content-Type: multipart/form-data; boundary=")
        && !text.contains("X-Solstone-Observer:")
        && !text.contains("Authorization:")
        && text.contains("name=\"envelope\"\r\nContent-Type: application/json")
        && !text.contains("name=\"envelope\"; filename=")
        && text.contains(&format!("\"day\":\"{day}\""))
        && text.contains(&format!("\"segment\":\"{segment}\""))
        && text.contains(&format!("\"files\":[{submitted}]"))
        && !text.contains("platform")
        && filenames
            .iter()
            .all(|filename| text.contains(&format!("name=\"files\"; filename=\"{filename}\"")))
        && ["host", "meta", "segment", "day", "platform"]
            .iter()
            .all(|field| !text.contains(&format!("name=\"{field}\"")))
}

#[allow(dead_code)] // The shared helper is compiled separately into each integration-test binary.
pub fn v3_read_capture_matches(request: &[u8], method: &str, path: &str) -> bool {
    let text = String::from_utf8_lossy(request);
    text.starts_with(&format!("{method} {path} HTTP/1.1\r\n"))
        && text.contains("X-Solstone-Protocol-Version: 3\r\n")
        && !text.contains("X-Solstone-Protocol-Version: 2\r\n")
        && !text.contains("Authorization:")
        && !text.contains("X-Solstone-Observer:")
}

fn consumer_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/observer-client")
}

fn verify_once() {
    static VERIFIED: OnceLock<()> = OnceLock::new();
    VERIFIED.get_or_init(|| {
        let root = consumer_root();
        xtask::observer_contract::verify(&root.join("bundle"), &root.join("adoption.json"))
            .expect("committed observer authority bundle must verify before conformance tests");
    });
}

fn record(document: &str, array: &str, id: &str) -> Value {
    verify_once();
    let path = consumer_root().join("bundle").join(document);
    let value: Value =
        serde_json::from_slice(&std::fs::read(path).expect("read authority document"))
            .expect("parse authority document");
    value[array]
        .as_array()
        .expect("authority record array")
        .iter()
        .find(|row| row["id"] == id)
        .unwrap_or_else(|| panic!("authority record {id} is absent"))
        .clone()
}

#[allow(dead_code)] // Each integration-test binary compiles this shared helper independently.
pub fn fixture(id: &str) -> Value {
    if FIXTURE_IDS.contains(&id) {
        return record("fixtures/wire-behavior.json", "fixtures", id);
    }
    // Upstream follow-up: v9 intentionally no longer projects pair, register,
    // ingestEvent, or callosum. These local fixtures preserve excluded-wire tests.
    let fixtures: Value =
        serde_json::from_str(include_str!("../fixtures/excluded_operations.json"))
            .expect("parse local excluded-operation fixtures");
    let payload = fixtures["fixtures"]
        .get(id)
        .unwrap_or_else(|| {
            panic!("fixture is neither authority-projected nor locally excluded: {id}")
        })
        .clone();
    serde_json::json!({ "payload": payload })
}

#[allow(dead_code)] // Each integration-test binary compiles this shared helper independently.
pub fn vector(id: &str) -> Value {
    assert!(
        VECTOR_IDS.contains(&id),
        "vector is not Windows-adopted: {id}"
    );
    record("vectors.json", "vectors", id)
}
