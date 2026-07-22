// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use serde::ser::{Serialize, SerializeMap, Serializer};
use sha2::{Digest, Sha256};
use std::process::Command;
use xtask::transparency_format::{canonicalize_transparency_json, CanonicalTransparencyJsonError};

const ENTRY_SHA256: &str = "30fa37a5d4a1b254e695339b1b0dcaa7a481bb26cca92dfd888f8186f049599f";
const LATEST_SHA256: &str = "598d1e2acd1765b6ab3bf7ebf915efe9077cb869ed6d67d39c4262de512d9061";

struct ReverseArtifact;

impl Serialize for ReverseArtifact {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("sha256", &"ab".repeat(32))?;
        map.serialize_entry("name", "example-0.0.1.tar.gz")?;
        map.serialize_entry("bytes", &100_000_000_u64)?;
        map.end()
    }
}

struct ReverseManifest;

impl Serialize for ReverseManifest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("sha256", &"cd".repeat(32))?;
        map.serialize_entry("name", "example-0.0.1.rust-release-manifest.json")?;
        map.end()
    }
}

struct ReverseEntry;

impl Serialize for ReverseEntry {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(11))?;
        map.serialize_entry("version", "0.0.1")?;
        map.serialize_entry("source_commit", "0123456789abcdef0123456789abcdef01234567")?;
        map.serialize_entry("seq", &1_u64)?;
        map.serialize_entry(
            "schema",
            "https://solpbc.org/schemas/transparency-ledger-entry/v1.json",
        )?;
        map.serialize_entry("published_utc", "2026-07-22T00:00:00Z")?;
        map.serialize_entry("proofs", &Vec::<ReverseManifest>::new())?;
        map.serialize_entry("product", "example")?;
        map.serialize_entry("prev_version", "")?;
        map.serialize_entry("prev_sha256", &"0".repeat(64))?;
        map.serialize_entry("manifests", &vec![ReverseManifest])?;
        map.serialize_entry("artifacts", &vec![ReverseArtifact])?;
        map.end()
    }
}

struct ReverseLatest;

impl Serialize for ReverseLatest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(7))?;
        map.serialize_entry("version", "0.0.1")?;
        map.serialize_entry("valid_until", "2026-08-05T00:00:00Z")?;
        map.serialize_entry("tip_sha256", ENTRY_SHA256)?;
        map.serialize_entry("signed_at", "2026-07-22T00:00:00Z")?;
        map.serialize_entry(
            "schema",
            "https://solpbc.org/schemas/transparency-latest/v1.json",
        )?;
        map.serialize_entry("product", "example")?;
        map.serialize_entry("chain_length", &1_u64)?;
        map.end()
    }
}

#[test]
fn entry_vector_matches_shared_canonical_bytes() {
    let actual = canonicalize_transparency_json(&ReverseEntry).expect("canonicalize entry vector");
    let expected = include_bytes!("fixtures/transparency/entry-vector.canonical.json");
    assert_eq!(actual, expected);
    assert_eq!(actual.len(), 611);
    assert_eq!(lower_hex(&Sha256::digest(&actual)), ENTRY_SHA256);
}

#[test]
fn latest_vector_matches_shared_canonical_bytes() {
    let actual =
        canonicalize_transparency_json(&ReverseLatest).expect("canonicalize latest vector");
    let expected = include_bytes!("fixtures/transparency/latest-vector.canonical.json");
    assert_eq!(actual, expected);
    assert_eq!(actual.len(), 275);
    assert_eq!(lower_hex(&Sha256::digest(&actual)), LATEST_SHA256);
}

#[test]
fn transparency_canonicalizer_rejects_non_ascii_floats_and_booleans_before_output() {
    assert_eq!(
        canonicalize_transparency_json(&"café"),
        Err(CanonicalTransparencyJsonError::NonAscii)
    );
    assert_eq!(
        canonicalize_transparency_json(&1.5_f64),
        Err(CanonicalTransparencyJsonError::UnsupportedType)
    );
    assert_eq!(
        canonicalize_transparency_json(&true),
        Err(CanonicalTransparencyJsonError::UnsupportedType)
    );
}

#[test]
fn transparency_feature_graph_excludes_serde_json_preserve_order() {
    let output = Command::new(env!("CARGO"))
        .args(["tree", "--locked", "-p", "xtask", "-e", "features"])
        .current_dir(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("workspace root"),
        )
        .output()
        .expect("inspect xtask feature graph");
    assert!(output.status.success());
    let graph = String::from_utf8(output.stdout).expect("ASCII cargo feature graph");
    assert!(!graph.contains("serde_json feature \"preserve_order\""));
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
