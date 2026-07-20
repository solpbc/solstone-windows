// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use xtask::observer_contract::{ADOPTED_FIXTURE_IDS, ADOPTED_VECTOR_IDS};

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

pub fn fixture(id: &str) -> Value {
    assert!(
        ADOPTED_FIXTURE_IDS.contains(&id),
        "fixture is not Windows-adopted: {id}"
    );
    record("fixtures/wire-behavior.json", "fixtures", id)
}

#[allow(dead_code)] // Each integration-test binary compiles this shared helper independently.
pub fn vector(id: &str) -> Value {
    assert!(
        ADOPTED_VECTOR_IDS.contains(&id),
        "vector is not Windows-adopted: {id}"
    );
    record("vectors.json", "vectors", id)
}
