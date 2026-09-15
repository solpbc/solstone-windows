// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Proves that `PairedState` compatibility is strictly preserved across the
//! transition to `spl-rust`. Uses the frozen pre-wave fixture and verifies
//! that new pairing results round-trip cleanly without extra fields.

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use pl_transport_win::credential::{Credential, EndpointAddr, PairedState};
use pl_transport_win::pairing::convert_shared_credential;
use pl_transport_win::relay_pairing::pair_over_relay;

use support::relay_pairing::{relay_link, spawn_mock_relay, MockState};

const FIXTURE_JSON: &str = include_str!("fixtures/pre-wave-paired-state.json");

fn temp_pairing_path(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("plw-compat-{name}-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("pairing.json")
}

fn windows_to_shared_credential(w: &Credential) -> spl_transport::credential::Credential {
    spl_transport::credential::Credential {
        client_key_pem: w.client_key_pem.clone(),
        client_cert_pem: w.client_cert_pem.clone(),
        ca_chain_pem: w.ca_chain_pem.clone(),
        ca_fp_prefix: w.ca_fp_prefix.clone(),
        instance_id: w.instance_id.clone(),
        home_label: w.home_label.clone(),
        endpoints: w
            .endpoints
            .iter()
            .map(|e| spl_transport::credential::EndpointAddr {
                host: e.host.clone(),
                port: e.port,
            })
            .collect(),
        relay_origin: w.relay_origin.clone(),
        device_token: w.device_token.clone(),
        device_token_expires_at: w.device_token_expires_at,
        home_attestation: None,
        local_endpoints: None,
    }
}

#[test]
fn pre_wave_paired_state_fixture_deserializes_and_round_trips_identically() {
    let state: PairedState = serde_json::from_str(FIXTURE_JSON).expect("valid fixture JSON");

    assert_eq!(state.access_mutation_generation, 1);
    let cred = state.credential.as_ref().expect("credential present");
    assert_eq!(
        cred.client_key_pem,
        "-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEIKb39Nqj+F9kY3g0o+6bM5/s9q3vL9v3K9v3K9v3K9v3\n-----END PRIVATE KEY-----\n"
    );
    assert_eq!(
        cred.client_cert_pem,
        "-----BEGIN CERTIFICATE-----\nMIIBojCCAUqgAwIBAgIBATAKBggqhkjOPQQDAjAzMR8wHQYDVQQDDBZzb2xzdG9u\nZS10ZXN0LWpvdXJuYWwxEDAOBgNVBAoMB3NvbHBiYzAeFw0yNjAxMDEwMDAwMDBa\nFw0yNzAxMDEwMDAwMDBaMDMxHzAdBgNVBAMMFnNvbHN0b25lLXRlc3Qtam91cm5h\nbDEQMA4GA1UECgwHc29scGJjMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE7Z5N\n3O6lU+9x2Y7mH9j7Z9x2Y7mH9j7Z9x2Y7mH9j7Z9x2Y7mH9j7Z9x2Y7mH9j7Z9x2\nY7mH9j7Z9x2Y7mH9jw==\n-----END CERTIFICATE-----\n"
    );
    assert_eq!(
        cred.ca_chain_pem,
        vec![
            "-----BEGIN CERTIFICATE-----\nMIIBojCCAUqgAwIBAgIBATAKBggqhkjOPQQDAjAzMR8wHQYDVQQDDBZzb2xzdG9u\nZS10ZXN0LWpvdXJuYWwxEDAOBgNVBAoMB3NvbHBiYzAeFw0yNjAxMDEwMDAwMDBa\nFw0yNzAxMDEwMDAwMDBaMDMxHzAdBgNVBAMMFnNvbHN0b25lLXRlc3Qtam91cm5h\nbDEQMA4GA1UECgwHc29scGJjMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE7Z5N\n3O6lU+9x2Y7mH9j7Z9x2Y7mH9j7Z9x2Y7mH9j7Z9x2Y7mH9j7Z9x2Y7mH9j7Z9x2\nY7mH9j7Z9x2Y7mH9jw==\n-----END CERTIFICATE-----\n".to_string()
        ]
    );
    assert_eq!(
        cred.ca_fp_prefix,
        vec![16, 32, 48, 64, 80, 96, 112, 128, 144, 160, 176, 192, 208, 224, 240, 255]
    );
    assert_eq!(cred.instance_id, "01234567-89ab-cdef-0123-456789abcdef");
    assert_eq!(cred.home_label, "Living Room Journal");
    assert_eq!(
        cred.endpoints,
        vec![EndpointAddr {
            host: "192.168.1.100".to_string(),
            port: 7657,
        }]
    );
    assert_eq!(
        cred.relay_origin.as_deref(),
        Some("https://link.solstone.app")
    );
    assert_eq!(
        cred.device_token.as_deref(),
        Some("header.eyJpYXQiOjE3ODAwMDAwMDAsImV4cCI6MTc4MDAxMDAwMCwianRpIjoidGVzdC1qdGkiLCJzdWIiOiJkZXZpY2UifQ.signature")
    );
    assert_eq!(cred.device_token_expires_at, Some(1780010000));

    // Prove field-equivalence via round-trip conversion with spl_transport::credential::Credential
    let shared = windows_to_shared_credential(cred);
    let converted_back = convert_shared_credential(shared);
    assert_eq!(&converted_back, cred);

    let reserialized = serde_json::to_string(&state).expect("serialize state");
    let reparsed: PairedState = serde_json::from_str(&reserialized).expect("reparse state");
    assert_eq!(
        reparsed.access_mutation_generation,
        state.access_mutation_generation
    );
    assert_eq!(reparsed.credential, state.credential);
}

#[test]
fn paired_state_disk_round_trip_with_fixture() {
    let state_file = temp_pairing_path("roundtrip");

    let state: PairedState = serde_json::from_str(FIXTURE_JSON).expect("valid fixture JSON");
    let cred = state.credential.as_ref().unwrap();

    state.save(&state_file).expect("save paired state");

    // Read raw JSON bytes on disk and assert forbidden keys are absent
    let raw_saved = std::fs::read(&state_file).expect("read saved file");
    let json_val: serde_json::Value = serde_json::from_slice(&raw_saved).expect("parse saved json");
    let cred_json = json_val.get("credential").expect("credential json");
    assert!(cred_json.get("home_attestation").is_none());
    assert!(cred_json.get("local_endpoints").is_none());
    assert!(json_val.get("home_attestation").is_none());
    assert!(json_val.get("local_endpoints").is_none());

    let loaded = PairedState::load(&state_file).expect("load paired state");
    let loaded_cred = loaded.credential.expect("credential loaded");
    assert_eq!(loaded_cred.instance_id, cred.instance_id);
    assert_eq!(loaded_cred.home_label, cred.home_label);
    assert_eq!(loaded_cred.endpoints, cred.endpoints);
    assert_eq!(loaded_cred.relay_origin, cred.relay_origin);
    assert_eq!(loaded_cred.device_token, cred.device_token);
    assert_eq!(
        loaded_cred.device_token_expires_at,
        cred.device_token_expires_at
    );
    assert_eq!(
        loaded.access_mutation_generation,
        state.access_mutation_generation
    );

    let _ = std::fs::remove_file(&state_file);
    if let Some(parent) = state_file.parent() {
        let _ = std::fs::remove_dir_all(parent);
    }
}

#[tokio::test]
async fn newly_generated_pairing_result_saves_and_loads_through_protector_path() {
    let state = Arc::new(MockState::normal().with_same_tls_ca());
    let origin = spawn_mock_relay(state.clone()).await;
    let link = relay_link(origin.clone(), state.json_ca.spki_pin());

    let credential = pair_over_relay(&link, "win-test")
        .await
        .expect("successful pairing");

    let state_file = temp_pairing_path("new-pairing");
    let paired_state = PairedState {
        access_mutation_generation: 42,
        credential: Some(credential.clone()),
    };

    paired_state.save(&state_file).expect("save paired state");

    // Verify raw JSON on disk contains no home_attestation or local_endpoints
    let raw_saved = std::fs::read(&state_file).expect("read saved file");
    let json_val: serde_json::Value = serde_json::from_slice(&raw_saved).expect("parse saved json");
    let cred_json = json_val.get("credential").expect("credential json");
    assert!(cred_json.get("home_attestation").is_none());
    assert!(cred_json.get("local_endpoints").is_none());

    let loaded = PairedState::load(&state_file).expect("load paired state");
    assert_eq!(loaded.access_mutation_generation, 42);
    let loaded_cred = loaded.credential.expect("credential loaded");
    assert_eq!(loaded_cred, credential);

    let _ = std::fs::remove_file(&state_file);
    if let Some(parent) = state_file.parent() {
        let _ = std::fs::remove_dir_all(parent);
    }
}
