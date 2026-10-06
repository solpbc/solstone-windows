// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Authority-derived protocol-v3 ingest-status conformance.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use observer_pl::ingest::{
    validate_segments_envelope, CustodyFailure, IngestError, IngestResponse, SegmentsEnvelope,
};
use serde_json::Value;
use xtask::observer_contract::{FIXTURE_IDS, VECTOR_IDS, WINDOWS_OPERATION_MAPPINGS};

fn bundle_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../contracts/observer-client/bundle")
        .join(relative)
}

fn records(relative: &str, field: &str) -> BTreeMap<String, Value> {
    let document: Value = serde_json::from_slice(
        &std::fs::read(bundle_path(relative)).expect("read authority bundle"),
    )
    .expect("parse verified authority JSON");
    document[field]
        .as_array()
        .expect("authority record array")
        .iter()
        .map(|row| {
            (
                row["id"].as_str().expect("authority record ID").to_owned(),
                row.clone(),
            )
        })
        .collect()
}

#[test]
fn observer_contract_authority_projection_paths_equal_production_constants() {
    let expected = [
        (
            "client.ingestUpload",
            "POST",
            observer_pl::paths::INGEST.to_owned(),
        ),
        (
            "client.ingestManifest",
            "GET",
            observer_pl::paths::INGEST_MANIFEST.to_owned(),
        ),
        (
            "client.ingestManifestDay",
            "GET",
            format!("{}/{{day}}", observer_pl::paths::INGEST_MANIFEST),
        ),
        (
            "client.ingestSegments",
            "GET",
            format!("{}/{{day}}", observer_pl::paths::INGEST_SEGMENTS),
        ),
    ];
    for (operation, method, path) in expected {
        let mapping = WINDOWS_OPERATION_MAPPINGS
            .iter()
            .find(|mapping| mapping.operation_id == operation)
            .expect("Windows mapping pin");
        assert_eq!((mapping.method, mapping.path), (method, path.as_str()));
    }
}

#[test]
fn observer_contract_authority_status_fixtures_and_vectors_match_real_wire_types() {
    let fixtures = records("fixtures/wire-behavior.json", "fixtures");
    let vectors = records("vectors.json", "vectors");
    assert_eq!(fixtures.len(), FIXTURE_IDS.len());
    assert_eq!(vectors.len(), VECTOR_IDS.len());

    for vector_id in VECTOR_IDS {
        let vector = &vectors[*vector_id];
        let fixture_id = vector["fixture_id"].as_str().expect("fixture ID");
        assert!(FIXTURE_IDS.contains(&fixture_id));
        let fixture = &fixtures[fixture_id];
        let decision = &vector["decision"];
        let valid = fixture["schema_validation"]["valid"]
            .as_bool()
            .expect("boolean schema validation result");
        match decision["kind"].as_str().expect("decision kind") {
            "ingest_status" => {
                let response: IngestResponse =
                    serde_json::from_value(fixture["payload"].clone()).expect("v3 status parses");
                assert_eq!(
                    response.status.is_accepted(),
                    decision["accepted"].as_bool().expect("accepted flag"),
                    "{vector_id}"
                );
                assert_eq!(
                    fixture["provenance"]["http_status"], decision["http_status"],
                    "{vector_id}"
                );
                assert_eq!(
                    fixture["payload"]["status"], decision["status"],
                    "{vector_id}"
                );
                assert_eq!(valid, *vector_id != "client.ingestUpload.status.failed");
            }
            "refusal" => {
                let response: IngestError =
                    serde_json::from_value(fixture["payload"].clone()).expect("v3 refusal parses");
                assert!(!decision["accepted"].as_bool().expect("accepted flag"));
                assert_eq!(response.reason_code, decision["reason_code"]);
                assert_eq!(
                    fixture["provenance"]["http_status"],
                    decision["http_status"]
                );
                assert!(valid);
            }
            "listing_collision_identity" => {
                let envelope: SegmentsEnvelope = serde_json::from_value(fixture["payload"].clone())
                    .expect("collision listing parses");
                validate_segments_envelope(&envelope).expect("distinct listing identities pass");
                let selected: Vec<Value> = envelope
                    .items
                    .iter()
                    .map(|item| {
                        serde_json::json!({
                            "key": item.key,
                            "segment": item.segment,
                            "stream": item.stream,
                        })
                    })
                    .collect();
                assert!(decision["accepted"].as_bool().expect("accepted flag"));
                assert_eq!(Value::Array(selected), decision["selected"]);
                assert_eq!(fixture["provenance"]["http_status"], 200);
                assert_eq!(fixture["provenance"]["protocol_version"], 3);
                assert!(valid);
            }
            "consumer_refusal" => {
                let envelope: SegmentsEnvelope = serde_json::from_value(fixture["payload"].clone())
                    .expect("duplicate-key listing parses");
                assert!(matches!(
                    validate_segments_envelope(&envelope),
                    Err(CustodyFailure::DuplicateListingKey { .. })
                ));
                assert!(!decision["accepted"].as_bool().expect("accepted flag"));
                assert_eq!(decision["reason_code"], "duplicate_listing_key");
                assert_eq!(decision["selected_keys"], serde_json::json!([]));
                assert_eq!(fixture["provenance"]["http_status"], 200);
                assert_eq!(fixture["provenance"]["protocol_version"], 3);
                assert!(valid);
            }
            kind => panic!("unhandled observer-client decision kind {kind}"),
        }
    }
}
