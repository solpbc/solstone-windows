// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Conformance harness for the vendored SPL pair-link definition bundle.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::Value;
use spl_core::ca;
use spl_core::crockford;
use spl_core::pairlink::{self, PairLinkError, ParsedPairLink};
use spl_core::relay_window;

const PIN_MANIFEST_JSON: &str = "5dc0c160ed9781964de2c6debe0b6e93f6b9d72e040a2b614356d1355b26dc64";
const PIN_DEFINITION_JSON: &str =
    "0507791c12f71b595cab3b49b0e47848137c2c5f8ee9340d9e3b5205a28218df";
const PIN_DEFINITION_SCHEMA_JSON: &str =
    "2050eb864994751ba19f0ab6f5a2ab5ffbf2ce4483c0ed531a667a1e9a574f55";
const PIN_VECTORS_JSON: &str = "9a7833f0e0206b03981a14459e0a1b90070dc562e4cd1bf162cb541ce3d0a8de";
const PIN_VECTORS_SCHEMA_JSON: &str =
    "4ca9793cb383c5b393f232b63d83814c12e20c33e047ae6053800dd80e78a364";

fn bundle_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/spl-pair-link/bundle")
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn hex_decode(s: &str) -> Vec<u8> {
    assert_eq!(
        s.len() % 2,
        0,
        "hex string length must be even, got length {}",
        s.len()
    );
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .unwrap_or_else(|e| panic!("invalid hex sequence '{}': {e}", &s[i..i + 2]))
        })
        .collect()
}

#[test]
fn bundle_files_match_authority_pins() {
    let pins = [
        ("manifest.json", PIN_MANIFEST_JSON),
        ("definition.json", PIN_DEFINITION_JSON),
        ("definition.schema.json", PIN_DEFINITION_SCHEMA_JSON),
        ("vectors.json", PIN_VECTORS_JSON),
        ("vectors.schema.json", PIN_VECTORS_SCHEMA_JSON),
    ];

    for (file_name, expected_sha) in pins {
        let path = bundle_dir().join(file_name);
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("failed to read vendored bundle file {path:?}: {e}"));
        let actual_sha = ca::sha256_hex(&bytes);
        assert_eq!(
            actual_sha, expected_sha,
            "hash mismatch for vendored bundle file {file_name}"
        );
    }
}

#[test]
fn spl_pair_link_vectors_conformance() {
    let b_dir = bundle_dir();
    let vectors_path = b_dir.join("vectors.json");
    println!(
        "population construction: bundle path {:?}, querying all vectors in vectors.json with 0 filters",
        b_dir
    );
    assert!(
        vectors_path.is_file(),
        "vectors.json must exist in bundle dir"
    );

    let vectors_bytes = std::fs::read(&vectors_path)
        .unwrap_or_else(|e| panic!("failed to read {vectors_path:?}: {e}"));
    let root: Value = serde_json::from_slice(&vectors_bytes).expect("parse vectors.json");

    let vectors = root["vectors"].as_array().expect("vectors array");
    let covers = root["covers"].as_array().expect("covers array");
    let covers_names: Vec<&str> = covers
        .iter()
        .map(|v| v.as_str().expect("cover name string"))
        .collect();

    assert_eq!(
        vectors.len(),
        84,
        "population size must be exactly 84 vectors"
    );
    let gapped_vector_count = 0usize;
    assert_eq!(gapped_vector_count, 0, "gapped vector count must be 0");

    let mut driven_vector_count = 0usize;
    let mut driven_cover_keys: HashSet<String> = HashSet::new();

    for vector in vectors {
        let id = vector["id"].as_str().expect("vector id");
        let operation = vector["operation"].as_str().expect("operation");

        match operation {
            "parse_pair_link" => {
                drive_parse_pair_link(id, vector);
            }
            "decode_crockford" => {
                drive_decode_crockford(id, vector);
            }
            "derive_relay_key" => {
                drive_derive_relay_key(id, vector);
            }
            "derive_jid" => {
                drive_derive_jid(id, vector);
            }
            unknown => {
                panic!("unknown vector operation: '{unknown}' in vector {id}");
            }
        }

        driven_vector_count += 1;

        if let Some(entry_digests) = vector.get("entry_digests").and_then(|v| v.as_object()) {
            for key in entry_digests.keys() {
                driven_cover_keys.insert(key.clone());
            }
        }
    }

    assert_eq!(
        driven_vector_count, 84,
        "driven vector count must be exactly 84"
    );
    assert_eq!(
        driven_vector_count,
        vectors.len(),
        "all vectors must be driven without gaps"
    );

    // Verify all covers predicates are known
    for driven_key in &driven_cover_keys {
        assert!(
            covers_names.contains(&driven_key.as_str()),
            "driven cover key {driven_key} not in covers array"
        );
    }
}

fn drive_parse_pair_link(id: &str, vector: &Value) {
    let input = &vector["input"];
    let encoding = input["encoding"].as_str().expect("input.encoding");
    let input_value = input["value"].as_str().expect("input.value");

    let actual_result = match encoding {
        "link" => pairlink::parse(input_value),
        "blob_hex" => {
            let blob = hex_decode(input_value);
            pairlink::parse_blob(&blob)
        }
        other => panic!("unknown input encoding '{other}' in vector {id}"),
    };

    let expected = &vector["expected"];
    let expected_result = expected["result"].as_str().expect("expected.result");

    match expected_result {
        "direct" => {
            let parsed = actual_result
                .unwrap_or_else(|e| panic!("vector {id}: expected Direct result, got Err({e:?})"));
            let direct = match parsed {
                ParsedPairLink::Direct(d) => d,
                ParsedPairLink::Relay(r) => {
                    panic!("vector {id}: expected ParsedPairLink::Direct, got Relay({r:?})")
                }
            };

            let expected_candidates = expected["candidates"].as_array().expect("candidates array");
            assert_eq!(
                direct.candidates.len(),
                expected_candidates.len(),
                "vector {id}: candidate count mismatch"
            );
            for (i, exp_cand) in expected_candidates.iter().enumerate() {
                let exp_host = exp_cand["host"].as_str().expect("candidate host");
                let exp_port = exp_cand["port"].as_u64().expect("candidate port") as u16;
                assert_eq!(
                    direct.candidates[i].host, exp_host,
                    "vector {id}: candidate {i} host mismatch"
                );
                assert_eq!(
                    direct.candidates[i].port, exp_port,
                    "vector {id}: candidate {i} port mismatch"
                );
            }

            let expected_nonce_hex = expected["nonce_hex"].as_str().expect("nonce_hex");
            assert_eq!(
                direct.nonce_hex, expected_nonce_hex,
                "vector {id}: nonce_hex mismatch"
            );

            let expected_ca_fp_hex = expected["ca_fp_hex"].as_str().expect("ca_fp_hex");
            assert_eq!(
                hex_lower(&direct.ca_fp_prefix),
                expected_ca_fp_hex,
                "vector {id}: ca_fp_prefix mismatch"
            );
        }
        "relay" => {
            let parsed = actual_result
                .unwrap_or_else(|e| panic!("vector {id}: expected Relay result, got Err({e:?})"));
            let relay = match parsed {
                ParsedPairLink::Relay(r) => r,
                ParsedPairLink::Direct(d) => {
                    panic!("vector {id}: expected ParsedPairLink::Relay, got Direct({d:?})")
                }
            };

            let expected_secret_hex = expected["secret_hex"].as_str().expect("secret_hex");
            assert_eq!(
                hex_lower(&relay.s),
                expected_secret_hex,
                "vector {id}: relay secret (s) mismatch"
            );

            let expected_ca_fp_spki_hex =
                expected["ca_fp_spki_hex"].as_str().expect("ca_fp_spki_hex");
            assert_eq!(
                hex_lower(&relay.ca_fp_spki),
                expected_ca_fp_spki_hex,
                "vector {id}: ca_fp_spki mismatch"
            );

            let expected_relay_origin = expected["relay_origin"].as_str().expect("relay_origin");
            assert_eq!(
                relay.relay_origin, expected_relay_origin,
                "vector {id}: relay_origin mismatch"
            );
        }
        "error" => {
            let err = match actual_result {
                Ok(ok) => panic!("vector {id}: expected Error, got Ok({ok:?})"),
                Err(e) => e,
            };

            let expected_error = &expected["error"];
            let expected_kind = expected_error["kind"].as_str().expect("error.kind");

            match expected_kind {
                "disallowed_direct_ipv4" => {
                    let expected_address = expected_error["address"].as_str().expect("address");
                    match err {
                        PairLinkError::DisallowedDirectIpv4 { address } => {
                            assert_eq!(
                                address, expected_address,
                                "vector {id}: disallowed address mismatch"
                            );
                        }
                        other => {
                            panic!("vector {id}: expected DisallowedDirectIpv4, got {other:?}")
                        }
                    }
                }
                "truncated" => {
                    let expected_len =
                        expected_error["expected"].as_u64().expect("expected len") as usize;
                    let expected_got = expected_error["got"].as_u64().expect("got len") as usize;
                    match err {
                        PairLinkError::Truncated { expected: exp, got } => {
                            assert_eq!(
                                exp, expected_len,
                                "vector {id}: truncated expected-len mismatch"
                            );
                            assert_eq!(
                                got, expected_got,
                                "vector {id}: truncated got-len mismatch"
                            );
                        }
                        other => panic!("vector {id}: expected Truncated, got {other:?}"),
                    }
                }
                "invalid_candidate_count" => {
                    let expected_count = expected_error["count"].as_u64().expect("count") as u8;
                    match err {
                        PairLinkError::InvalidCandidateCount { count } => {
                            assert_eq!(
                                count, expected_count,
                                "vector {id}: invalid candidate count mismatch"
                            );
                        }
                        other => {
                            panic!("vector {id}: expected InvalidCandidateCount, got {other:?}")
                        }
                    }
                }
                "unsupported_address_type" => {
                    let expected_type = expected_error["address_type"]
                        .as_u64()
                        .expect("address_type") as u8;
                    match err {
                        PairLinkError::UnsupportedAddressType(t) => {
                            assert_eq!(
                                t, expected_type,
                                "vector {id}: unsupported address type mismatch"
                            );
                        }
                        other => {
                            panic!("vector {id}: expected UnsupportedAddressType, got {other:?}")
                        }
                    }
                }
                "unknown_ca_fp_tag" => {
                    let expected_tag = expected_error["tag"].as_u64().expect("tag") as u8;
                    match err {
                        PairLinkError::UnknownCaFpTag(t) => {
                            assert_eq!(t, expected_tag, "vector {id}: unknown ca_fp_tag mismatch");
                        }
                        other => panic!("vector {id}: expected UnknownCaFpTag, got {other:?}"),
                    }
                }
                "bad_relay_origin" => match err {
                    PairLinkError::BadRelayOrigin => {}
                    other => panic!("vector {id}: expected BadRelayOrigin, got {other:?}"),
                },
                other => panic!("unknown expected error kind '{other}' in vector {id}"),
            }
        }
        other => panic!("unknown expected result shape '{other}' in vector {id}"),
    }
}

fn drive_decode_crockford(id: &str, vector: &Value) {
    let input = vector["input"].as_str().expect("input");
    let expected_hex = vector["expected_hex"].as_str().expect("expected_hex");

    let actual_bytes = crockford::decode(input)
        .unwrap_or_else(|e| panic!("vector {id}: crockford::decode failed: {e:?}"));
    assert_eq!(
        hex_lower(&actual_bytes),
        expected_hex,
        "vector {id}: crockford decoded bytes mismatch"
    );
}

fn drive_derive_relay_key(id: &str, vector: &Value) {
    let secret_hex = vector["secret_hex"].as_str().expect("secret_hex");
    let expected_hex = vector["expected_hex"].as_str().expect("expected_hex");

    let secret_bytes = hex_decode(secret_hex);
    let secret_fixed: [u8; 8] = secret_bytes
        .try_into()
        .unwrap_or_else(|_| panic!("vector {id}: secret_hex must decode to 8 bytes"));

    let derived = relay_window::derive_rk(&secret_fixed);
    assert_eq!(
        hex_lower(&derived),
        expected_hex,
        "vector {id}: derived relay key mismatch"
    );
}

fn drive_derive_jid(id: &str, vector: &Value) {
    let spki_der_hex = vector["spki_der_hex"].as_str().expect("spki_der_hex");
    let spki_der = hex_decode(spki_der_hex);

    let actual_result = relay_window::jid_from_spki(&spki_der);
    let expected = &vector["expected"];
    let expected_result = expected["result"].as_str().expect("expected.result");

    match expected_result {
        "jid" => {
            let actual_jid = actual_result
                .unwrap_or_else(|e| panic!("vector {id}: expected Ok(jid), got Err({e:?})"));
            let expected_jid = expected["jid"].as_str().expect("expected.jid");
            assert_eq!(actual_jid, expected_jid, "vector {id}: jid output mismatch");
        }
        "error" => match actual_result {
            Ok(jid) => {
                panic!("vector {id}: expected Error, got Ok({jid})");
            }
            Err(relay_window::JidError::NotP256)
            | Err(relay_window::JidError::InvalidPoint)
            | Err(relay_window::JidError::MalformedSpki) => {}
        },
        other => panic!("unknown expected result shape '{other}' in vector {id}"),
    }
}
