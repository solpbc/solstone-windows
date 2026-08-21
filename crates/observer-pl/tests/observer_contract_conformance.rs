// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Authority-derived protocol-v3 ingest-status conformance.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use observer_pl::ingest::IngestResponse;
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
            "observer.ingestUpload",
            "POST",
            observer_pl::paths::INGEST.to_owned(),
        ),
        (
            "observer.ingestManifest",
            "GET",
            observer_pl::paths::INGEST_MANIFEST.to_owned(),
        ),
        (
            "observer.ingestManifestDay",
            "GET",
            format!("{}/{{day}}", observer_pl::paths::INGEST_MANIFEST),
        ),
        (
            "observer.ingestSegments",
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
        let response: IngestResponse =
            serde_json::from_value(fixture["payload"].clone()).expect("v3 status parses");
        let decision = &vector["decision"];
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
        let valid = fixture["schema_validation"]["valid"]
            .as_bool()
            .expect("boolean schema validation result");
        assert_eq!(
            valid,
            *vector_id != "observer.ingestUpload.status.failed",
            "{vector_id}"
        );
    }
}
