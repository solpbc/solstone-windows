// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Authority-derived observer-client bundle conformance.

use observer_pl::wire::{
    IngestResponse, PairRequest, PairResponse, RegisterRequest, RegisterResponse, SegmentsResponse,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use xtask::observer_contract::{
    ADOPTED_FIXTURE_IDS, ADOPTED_VECTOR_IDS, WINDOWS_OPERATION_MAPPINGS,
};

fn bundle_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../contracts/observer-client/bundle")
        .join(relative)
}

fn document(relative: &str) -> Value {
    serde_json::from_slice(&std::fs::read(bundle_path(relative)).expect("read authority bundle"))
        .expect("parse verified authority JSON")
}

fn records(relative: &str, field: &str) -> BTreeMap<String, Value> {
    let document = document(relative);
    let rows = document[field].as_array().expect("authority record array");
    let mut indexed = BTreeMap::new();
    for row in rows {
        let id = row["id"].as_str().expect("authority record ID").to_owned();
        assert!(
            indexed.insert(id.clone(), row.clone()).is_none(),
            "duplicate {id}"
        );
    }
    indexed
}

fn adopted_fixtures() -> BTreeMap<String, Value> {
    let all = records("fixtures/wire-behavior.json", "fixtures");
    ADOPTED_FIXTURE_IDS
        .iter()
        .map(|id| {
            (
                (*id).to_owned(),
                all.get(*id)
                    .unwrap_or_else(|| panic!("missing {id}"))
                    .clone(),
            )
        })
        .collect()
}

fn adopted_vectors() -> BTreeMap<String, Value> {
    let all = records("vectors.json", "vectors");
    ADOPTED_VECTOR_IDS
        .iter()
        .map(|id| {
            (
                (*id).to_owned(),
                all.get(*id)
                    .unwrap_or_else(|| panic!("missing {id}"))
                    .clone(),
            )
        })
        .collect()
}

fn assert_provenance(
    fixture: &Value,
    operation: &str,
    direction: &str,
    media_type: &str,
    variant: &str,
    status: Option<u64>,
) {
    let provenance = &fixture["provenance"];
    assert_eq!(provenance["operation_id"], operation);
    assert_eq!(provenance["direction"], direction);
    assert_eq!(provenance["media_type"], media_type);
    assert_eq!(provenance["named_variant"], variant);
    assert_eq!(provenance["status"].as_u64(), status);
    assert!(
        fixture["schema_validation"]["validates"].is_boolean()
            || fixture["schema_validation"]["validates"].is_null()
    );
}

#[test]
fn observer_contract_authority_projection_paths_equal_production_constants() {
    let expected = [
        ("link.pair", "POST", observer_pl::paths::PAIR.to_owned()),
        (
            "observer.register",
            "POST",
            observer_pl::paths::REGISTER.to_owned(),
        ),
        (
            "observer.ingestEvent",
            "POST",
            observer_pl::paths::INGEST_EVENT.to_owned(),
        ),
        (
            "observer.ingestSegments",
            "GET",
            format!("{}/{{day}}", observer_pl::paths::INGEST_SEGMENTS),
        ),
        (
            "observer.ingestUpload",
            "POST",
            observer_pl::paths::INGEST.to_owned(),
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
fn observer_contract_authority_dispatches_every_adopted_fixture_provenance() {
    let fixtures = adopted_fixtures();
    assert_eq!(fixtures.len(), ADOPTED_FIXTURE_IDS.len());
    for id in ADOPTED_FIXTURE_IDS {
        let fixture = &fixtures[*id];
        match *id {
            "declared.observer.ingestSegments.custody_unknown_rejected" => assert_provenance(
                fixture,
                "observer.ingestSegments",
                "response",
                "application/json",
                "custody_unknown",
                Some(200),
            ),
            "declared.observer.ingestSegments.envelope_total_mismatch" => assert_provenance(
                fixture,
                "observer.ingestSegments",
                "response",
                "application/json",
                "envelope_total_mismatch",
                Some(200),
            ),
            "declared.observer.ingestUpload.status_unknown_rejected" => assert_provenance(
                fixture,
                "observer.ingestUpload",
                "response",
                "application/json",
                "status_unknown",
                Some(200),
            ),
            "example.callosum.rootEvents.response.200.text-event-stream.default" => {
                assert_provenance(
                    fixture,
                    "callosum.rootEvents",
                    "response",
                    "text/event-stream",
                    "default",
                    Some(200),
                )
            }
            "example.link.pair.request.body.application-json.default" => assert_provenance(
                fixture,
                "link.pair",
                "request",
                "application/json",
                "default",
                None,
            ),
            "example.link.pair.response.200.application-json.default" => assert_provenance(
                fixture,
                "link.pair",
                "response",
                "application/json",
                "default",
                Some(200),
            ),
            "example.observer.ingestEvent.request.body.application-json.default" => {
                assert_provenance(
                    fixture,
                    "observer.ingestEvent",
                    "request",
                    "application/json",
                    "default",
                    None,
                )
            }
            "example.observer.ingestEvent.response.200.application-json.default" => {
                assert_provenance(
                    fixture,
                    "observer.ingestEvent",
                    "response",
                    "application/json",
                    "default",
                    Some(200),
                )
            }
            "example.observer.ingestSegments.response.200.application-json.legacy" => {
                assert_provenance(
                    fixture,
                    "observer.ingestSegments",
                    "response",
                    "application/json",
                    "legacy",
                    Some(200),
                )
            }
            "example.observer.ingestSegments.response.200.application-json.v2" => {
                assert_provenance(
                    fixture,
                    "observer.ingestSegments",
                    "response",
                    "application/json",
                    "v2",
                    Some(200),
                )
            }
            "example.observer.ingestUpload.request.body.multipart-form-data.default" => {
                assert_provenance(
                    fixture,
                    "observer.ingestUpload",
                    "request",
                    "multipart/form-data",
                    "default",
                    None,
                )
            }
            "example.observer.ingestUpload.response.200.application-json.duplicate" => {
                assert_provenance(
                    fixture,
                    "observer.ingestUpload",
                    "response",
                    "application/json",
                    "duplicate",
                    Some(200),
                )
            }
            "example.observer.ingestUpload.response.200.application-json.normal" => {
                assert_provenance(
                    fixture,
                    "observer.ingestUpload",
                    "response",
                    "application/json",
                    "normal",
                    Some(200),
                )
            }
            "example.observer.register.request.body.application-json.default" => assert_provenance(
                fixture,
                "observer.register",
                "request",
                "application/json",
                "default",
                None,
            ),
            "example.observer.register.response.200.application-json.default" => assert_provenance(
                fixture,
                "observer.register",
                "response",
                "application/json",
                "default",
                Some(200),
            ),
            "recorded.auth.bearer.segments" => assert_provenance(
                fixture,
                "observer.ingestSegments",
                "response",
                "application/json",
                "bearer",
                Some(200),
            ),
            "recorded.auth.handle.segments" => assert_provenance(
                fixture,
                "observer.ingestSegments",
                "response",
                "application/json",
                "observer_handle",
                Some(200),
            ),
            "recorded.ingestUpload.collision" => assert_provenance(
                fixture,
                "observer.ingestUpload",
                "response",
                "application/json",
                "collision",
                Some(200),
            ),
            "recorded.ingestUpload.conflict" => assert_provenance(
                fixture,
                "observer.ingestUpload",
                "response",
                "application/json",
                "conflict",
                Some(409),
            ),
            "recorded.ingestUpload.duplicate" => assert_provenance(
                fixture,
                "observer.ingestUpload",
                "response",
                "application/json",
                "duplicate",
                Some(200),
            ),
            "recorded.ingestUpload.failed" => assert_provenance(
                fixture,
                "observer.ingestUpload",
                "response",
                "application/json",
                "failed",
                Some(422),
            ),
            "recorded.ingestUpload.ok" => assert_provenance(
                fixture,
                "observer.ingestUpload",
                "response",
                "application/json",
                "ok",
                Some(200),
            ),
            "recorded.segments.custody_statuses" => assert_provenance(
                fixture,
                "observer.ingestSegments",
                "response",
                "application/json",
                "custody_statuses",
                Some(200),
            ),
            "recorded.segments.legacy.absent_header" => assert_provenance(
                fixture,
                "observer.ingestSegments",
                "response",
                "application/json",
                "legacy_array_absent_header",
                Some(200),
            ),
            "recorded.segments.legacy.unparseable_header" => assert_provenance(
                fixture,
                "observer.ingestSegments",
                "response",
                "application/json",
                "legacy_array_unparseable_header",
                Some(200),
            ),
            "recorded.segments.submitted_name_omitted" => assert_provenance(
                fixture,
                "observer.ingestSegments",
                "response",
                "application/json",
                "submitted_name_omitted",
                Some(200),
            ),
            "recorded.segments.v2.envelope" => assert_provenance(
                fixture,
                "observer.ingestSegments",
                "response",
                "application/json",
                "v2_envelope",
                Some(200),
            ),
            "recorded.sse.root.data_unknown_event" => assert_provenance(
                fixture,
                "callosum.rootEvents",
                "response",
                "text/event-stream",
                "data_unknown_event",
                Some(200),
            ),
            "recorded.sse.root.heartbeat" => assert_provenance(
                fixture,
                "callosum.rootEvents",
                "response",
                "text/event-stream",
                "heartbeat",
                Some(200),
            ),
            _ => panic!("adopted fixture lacks an explicit dispatch: {id}"),
        }
    }
}

#[test]
fn observer_contract_authority_real_wire_types_parse_applicable_examples() {
    let fixtures = adopted_fixtures();

    let pair_request: PairRequest = serde_json::from_value(
        fixtures["example.link.pair.request.body.application-json.default"]["payload"].clone(),
    )
    .expect("real PairRequest tolerates authority-only example fields");
    assert_eq!(pair_request.device_label, "Jer iPhone");
    assert!(pair_request.csr.contains("CERTIFICATE REQUEST"));

    let pair_response: PairResponse = serde_json::from_value(
        fixtures["example.link.pair.response.200.application-json.default"]["payload"].clone(),
    )
    .expect("real PairResponse parses authority example");
    assert_eq!(pair_response.home_label, "home");
    assert!(pair_response.home_attestation.is_some());
    assert!(pair_response.local_endpoints.is_some());

    let register_request: RegisterRequest = serde_json::from_value(
        fixtures["example.observer.register.request.body.application-json.default"]["payload"]
            .clone(),
    )
    .expect("real RegisterRequest parses authority example");
    assert_eq!(register_request.hostname, "archon");
    assert_eq!(register_request.stream_type, "desktop");
    let register_response: RegisterResponse = serde_json::from_value(
        fixtures["example.observer.register.response.200.application-json.default"]["payload"]
            .clone(),
    )
    .expect("real RegisterResponse parses authority example");
    assert_eq!(
        register_response.protocol_version,
        Some(observer_pl::OBSERVER_PROTOCOL_VERSION)
    );
    assert_eq!(register_response.key, "x7J7k2observerHandle");

    for id in [
        "example.observer.ingestUpload.response.200.application-json.duplicate",
        "example.observer.ingestUpload.response.200.application-json.normal",
    ] {
        let response: IngestResponse =
            serde_json::from_value(fixtures[id]["payload"].clone()).expect("real ingest response");
        assert!(response.is_accepted(), "{id}");
    }
}

#[test]
fn observer_contract_authority_ingest_status_vectors_bind_real_acceptance_predicate() {
    let fixtures = adopted_fixtures();
    let vectors = adopted_vectors();
    for id in [
        "observer.ingestUpload.status.collision",
        "observer.ingestUpload.status.conflict",
        "observer.ingestUpload.status.duplicate",
        "observer.ingestUpload.status.failed",
        "observer.ingestUpload.status.ok",
        "observer.ingestUpload.status_unknown_rejected",
    ] {
        let vector = &vectors[id];
        let fixture = &fixtures[vector["fixture_id"].as_str().expect("fixture reference")];
        let response: IngestResponse =
            serde_json::from_value(fixture["payload"].clone()).expect("real ingest response");
        assert_eq!(
            response.is_accepted(),
            vector["decision"]["accepted"].as_bool().unwrap_or(false)
        );
        assert_eq!(response.status, vector["decision"]["status"]);
    }
}

#[test]
fn observer_contract_authority_custody_vectors_bind_real_predicate() {
    let fixtures = adopted_fixtures();
    let vectors = adopted_vectors();

    let fixture = &fixtures[vectors["observer.ingestSegments.custody_statuses"]["fixture_id"]
        .as_str()
        .unwrap()];
    let response: SegmentsResponse = serde_json::from_value(fixture["payload"].clone()).unwrap();
    let segment = &response.items[0];
    for file in &segment.files {
        let status = file.status.as_deref().unwrap();
        let expected = vectors["observer.ingestSegments.custody_statuses"]["decision"]
            ["holding_by_status"][status]
            == "held";
        assert_eq!(
            response.proves_file_held(
                &segment.key,
                &file.name,
                file.sha256.as_deref().expect("authority sha")
            ),
            expected,
            "status {status}"
        );
    }

    for vector_id in [
        "observer.ingestSegments.custody_unknown_rejected",
        "observer.ingestSegments.submitted_name_fallback",
    ] {
        let vector = &vectors[vector_id];
        let fixture = &fixtures[vector["fixture_id"].as_str().unwrap()];
        let response: SegmentsResponse =
            serde_json::from_value(fixture["payload"].clone()).unwrap();
        let segment = &response.items[0];
        let file = &segment.files[0];
        let held =
            response.proves_file_held(&segment.key, &file.name, file.sha256.as_deref().unwrap());
        assert_eq!(held, vector_id.ends_with("submitted_name_fallback"));
    }
}

#[test]
fn observer_contract_authority_total_mismatch_decision_matches_tolerant_wire_parse() {
    let fixtures = adopted_fixtures();
    let vectors = adopted_vectors();
    let vector = &vectors["observer.ingestSegments.envelope_total_mismatch"];
    let fixture = &fixtures[vector["fixture_id"].as_str().unwrap()];
    let response: SegmentsResponse = serde_json::from_value(fixture["payload"].clone()).unwrap();
    let relation = response.total == Some(response.items.len() as u64);
    assert_eq!(relation, vector["decision"]["valid"].as_bool().unwrap());
    assert_eq!(vector["decision"]["expected"], "total_equals_items_length");
}

#[test]
fn observer_contract_authority_dispatches_every_adopted_vector_decision() {
    let fixtures = adopted_fixtures();
    let vectors = adopted_vectors();
    assert_eq!(vectors.len(), ADOPTED_VECTOR_IDS.len());
    for id in ADOPTED_VECTOR_IDS {
        let vector = &vectors[*id];
        assert!(fixtures.contains_key(vector["fixture_id"].as_str().expect("fixture_id")));
        assert!(matches!(
            vector["kind"].as_str(),
            Some("recorded" | "declared")
        ));
        let decision = &vector["decision"];
        match *id {
            "callosum.rootEvents.sse.data_unknown_event" => {
                assert_eq!(decision["action"], "pass_through");
                assert_eq!(decision["unknown_event_behavior"], "preserve");
            }
            "callosum.rootEvents.sse.heartbeat" => {
                assert_eq!(decision["action"], "ignore_keepalive");
                assert_eq!(decision["frame_kind"], "heartbeat");
            }
            "observer.auth.bearer" => {
                assert_eq!(decision["accepted"], true);
                assert_eq!(decision["auth_form"], "authorization_bearer");
            }
            "observer.auth.handle" => {
                assert_eq!(decision["accepted"], true);
                assert_eq!(decision["auth_form"], "x_solstone_observer");
            }
            "observer.ingestSegments.custody_statuses" => {
                assert_eq!(decision["holding_by_status"]["processed"], "held");
                assert_eq!(decision["holding_by_status"]["present"], "held");
                assert_eq!(decision["holding_by_status"]["missing"], "not_held");
            }
            "observer.ingestSegments.custody_unknown_rejected" => {
                assert_eq!(decision["unknown_status"], "reject");
            }
            "observer.ingestSegments.envelope_total_mismatch" => {
                assert_eq!(decision["valid"], false);
            }
            "observer.ingestSegments.legacy_array.absent_header" => {
                assert_eq!(decision["header"], "absent");
                assert_eq!(decision["parsed_version"], 1);
            }
            "observer.ingestSegments.legacy_array.unparseable_header" => {
                assert_eq!(decision["header"], "unparseable");
                assert_eq!(decision["parsed_version"], 1);
            }
            "observer.ingestSegments.submitted_name_fallback" => {
                assert_eq!(decision["fallback"], "name");
                assert_eq!(decision["submitted_name_present"], false);
            }
            "observer.ingestSegments.v2_envelope" => {
                let protocol_version = u64::from(observer_pl::OBSERVER_PROTOCOL_VERSION);
                assert_eq!(
                    decision["current_protocol_version"].as_u64(),
                    Some(protocol_version)
                );
                assert_eq!(decision["parsed_version"].as_u64(), Some(protocol_version));
            }
            "observer.ingestUpload.status.collision" => {
                assert_eq!(decision["accepted"], true);
                assert_eq!(decision["stored_key_source"], "segment");
            }
            "observer.ingestUpload.status.conflict" => {
                assert_eq!(decision["accepted"], false);
                assert_eq!(decision["http_status"], 409);
            }
            "observer.ingestUpload.status.duplicate" => {
                assert_eq!(decision["accepted"], true);
                assert_eq!(decision["stored_key_source"], "existing_segment");
            }
            "observer.ingestUpload.status.failed" => {
                assert_eq!(decision["accepted"], false);
                assert_eq!(decision["http_status"], 422);
            }
            "observer.ingestUpload.status.ok" => {
                assert_eq!(decision["accepted"], true);
                assert_eq!(decision["stored_key_source"], "segment");
            }
            "observer.ingestUpload.status_unknown_rejected" => {
                assert_eq!(decision["unknown_value_behavior"], "reject");
            }
            _ => panic!("adopted vector lacks an explicit dispatch: {id}"),
        }
    }
}
