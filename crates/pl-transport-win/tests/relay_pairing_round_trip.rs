// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

mod support;

use std::sync::Arc;

use observer_pl::pairlink::RelayPairLink;
use pl_transport_win::credential::EndpointAddr;
use pl_transport_win::relay_pairing::pair_over_relay;
use pl_transport_win::relay_token::{refresh_device_token, RefreshOutcome};
use pl_transport_win::{transport_error_code, RelayControlEndpoint, TransportError};

use support::observer_contract::fixture as authority_fixture;
use support::relay_pairing::{
    jid_for_ca, relay_form_link, relay_link, spawn_mock_relay, HomeMode, MockState, CURRENT_TOKEN,
    ENROLL_TOKEN,
};

#[tokio::test]
async fn relay_pairing_full_ceremony_populates_credential() {
    let state = Arc::new(MockState::normal().with_same_tls_ca());
    let origin = spawn_mock_relay(state.clone()).await;
    let link = relay_link(origin.clone(), state.json_ca.spki_pin());

    let credential = pair_over_relay(&link, "win-test").await.unwrap();

    assert_eq!(credential.relay_origin.as_deref(), Some(origin.as_str()));
    assert_eq!(credential.instance_id, jid_for_ca(state.json_ca.as_ref()));
    assert_eq!(credential.device_token.as_deref(), Some(ENROLL_TOKEN));
    assert_eq!(credential.device_token_expires_at, Some(9_999_999_999));
    assert!(credential.client_key_pem.contains("BEGIN PRIVATE KEY"));
    assert!(credential.client_cert_pem.contains("BEGIN CERTIFICATE"));
    assert_eq!(credential.ca_chain_pem.len(), 1);
    assert_eq!(credential.ca_fp_prefix, state.json_ca.cert_der_pin());
    assert_eq!(
        credential.endpoints,
        vec![EndpointAddr {
            host: "10.0.0.2".into(),
            port: 7657
        }]
    );
}

#[tokio::test]
async fn observer_contract_authority_relay_pairing_uses_real_ceremony() {
    let fixture = authority_fixture("example.link.pair.request.body.application-json.default");
    let nonce = fixture["payload"]["nonce"].as_str().unwrap();
    let mut secret = [0u8; 8];
    for (index, byte) in secret.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&nonce[index * 2..index * 2 + 2], 16).unwrap();
    }
    let state = Arc::new(MockState::normal().with_same_tls_ca());
    *state.expected_pair_token.lock().unwrap() = nonce[..16].to_owned();
    let origin = spawn_mock_relay(state.clone()).await;
    let link = RelayPairLink {
        s: secret,
        ca_fp_spki: state.json_ca.spki_pin(),
        relay_origin: origin,
    };
    let device_label = fixture["payload"]["device_label"].as_str().unwrap();

    let credential = pair_over_relay(&link, device_label).await.unwrap();
    let captured = state.pair_request.lock().unwrap().clone().unwrap();
    assert_eq!(captured.device_label, device_label);
    assert!(captured.csr.contains("BEGIN CERTIFICATE REQUEST"));
    assert!(credential.client_cert_pem.contains("BEGIN CERTIFICATE"));
    assert_eq!(credential.home_label, "Home");
}

#[tokio::test]
async fn observer_contract_authority_pair_from_link_dispatches_relay_ceremony() {
    let fixture = authority_fixture("example.link.pair.request.body.application-json.default");
    let nonce = fixture["payload"]["nonce"].as_str().unwrap();
    let mut secret = [0u8; 8];
    for (index, byte) in secret.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&nonce[index * 2..index * 2 + 2], 16).unwrap();
    }
    let state = Arc::new(MockState::normal().with_same_tls_ca());
    *state.expected_pair_token.lock().unwrap() = nonce[..16].to_owned();
    let origin = spawn_mock_relay(state.clone()).await;
    let link = relay_form_link(&origin, &secret, &state.json_ca.spki_pin());
    let device_label = fixture["payload"]["device_label"].as_str().unwrap();

    let credential = pl_transport_win::pairing::pair_from_link(&link, device_label)
        .await
        .unwrap();
    assert_eq!(
        state
            .pair_request
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .device_label,
        device_label
    );
    assert!(credential.client_cert_pem.contains("BEGIN CERTIFICATE"));
}

#[tokio::test]
async fn relay_pairing_rejects_jid_mismatch_before_enroll() {
    let state = Arc::new(MockState::normal().with_same_tls_ca());
    *state.pair_instance_id.lock().unwrap() =
        Some("00000000-0000-8000-8000-000000000001".to_string());
    let origin = spawn_mock_relay(state.clone()).await;
    let link = relay_link(origin, state.json_ca.spki_pin());

    let err = pair_over_relay(&link, "win-test").await.unwrap_err();
    assert!(matches!(err, TransportError::Pairing(_)));
}

#[tokio::test]
async fn relay_pairing_rejects_anti_pin_theater_leaf() {
    let state = Arc::new(MockState::normal());
    let origin = spawn_mock_relay(state.clone()).await;
    let link = relay_link(origin, state.json_ca.spki_pin());

    let err = pair_over_relay(&link, "win-test").await.unwrap_err();
    assert!(matches!(err, TransportError::Pairing(_)));
}

#[tokio::test]
async fn relay_pairing_rejects_wrong_spki_before_enroll() {
    let state = Arc::new(MockState::normal().with_same_tls_ca());
    let origin = spawn_mock_relay(state.clone()).await;
    let link = relay_link(origin, vec![0u8; 16]);

    let err = pair_over_relay(&link, "win-test").await.unwrap_err();
    assert!(matches!(err, TransportError::Pairing(_)));
}

#[tokio::test]
async fn relay_pairing_inner_410_maps_to_http_410() {
    let mut state = MockState::normal().with_same_tls_ca();
    state.home_mode = HomeMode::InnerGone;
    let state = Arc::new(state);
    let origin = spawn_mock_relay(state.clone()).await;
    let link = relay_link(origin, state.json_ca.spki_pin());

    let err = pair_over_relay(&link, "win-test").await.unwrap_err();
    assert!(matches!(err, TransportError::Rejected { status: 410, .. }));
    assert_eq!(transport_error_code(&err), "http_410");
}

#[tokio::test]
async fn relay_pairing_rejects_missing_home_attestation() {
    let mut state = MockState::normal().with_same_tls_ca();
    state.home_mode = HomeMode::MissingHomeAttestation;
    let state = Arc::new(state);
    let origin = spawn_mock_relay(state.clone()).await;
    let link = relay_link(origin, state.json_ca.spki_pin());

    let err = pair_over_relay(&link, "win-test").await.unwrap_err();
    assert!(matches!(err, TransportError::Pairing(_)));
}

#[tokio::test]
async fn relay_pairing_enroll_statuses_are_control_rejections() {
    for status in [409, 401, 403, 404] {
        let state = Arc::new(MockState::normal().with_same_tls_ca());
        *state.enroll_status.lock().unwrap() = Some(status);
        let origin = spawn_mock_relay(state.clone()).await;
        let link = relay_link(origin, state.json_ca.spki_pin());

        let err = pair_over_relay(&link, "win-test").await.unwrap_err();
        assert!(matches!(
            err,
            TransportError::RelayControlRejected {
                endpoint: RelayControlEndpoint::EnrollDevice,
                status: actual
            } if actual == status
        ));
        let code = transport_error_code(&err);
        assert_eq!(code, format!("relay_enroll_device_http_{status}"));
        assert!(!code.contains("attestation"));
    }
}

#[tokio::test]
async fn forced_refresh_reconnect_statuses() {
    for status in [401, 403, 404] {
        let state = Arc::new(MockState::normal().with_same_tls_ca());
        *state.refresh_status.lock().unwrap() = Some(status);
        let origin = spawn_mock_relay(state).await;
        assert_eq!(
            refresh_device_token(&origin, CURRENT_TOKEN).await,
            RefreshOutcome::ReconnectNeeded
        );
    }
}
