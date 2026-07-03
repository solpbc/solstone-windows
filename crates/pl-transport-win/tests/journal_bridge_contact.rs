// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::time::Duration;

use pl_transport_win::credential::{Credential, EndpointAddr, PairedState};
use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};

fn paired_state() -> PairedState {
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
    let params = CertificateParams::new(vec!["observer.test".to_string()]).unwrap();
    let cert = params.self_signed(&key).unwrap();
    let credential = Credential {
        client_key_pem: key.serialize_pem(),
        client_cert_pem: cert.pem(),
        ca_chain_pem: vec![cert.pem()],
        ca_fp_prefix: vec![0u8; 16],
        instance_id: "test-instance".into(),
        home_label: "Home".into(),
        endpoints: vec![EndpointAddr {
            host: "127.0.0.1".into(),
            port: 1,
        }],
        relay_origin: None,
        device_token: None,
        device_token_expires_at: None,
    };
    PairedState {
        credential: Some(credential),
        observer_key: Some("observer-key".into()),
        observer_name: None,
    }
}

#[tokio::test]
async fn contacted_flips_on_first_accept_before_http_parse() {
    let paired = paired_state();
    let state_path = std::env::temp_dir().join(format!(
        "journal-bridge-contact-{}.json",
        std::process::id()
    ));
    let handle = pl_transport_win::journal_bridge::start(&paired, state_path)
        .await
        .expect("bridge start");

    // Flag starts false before any connection.
    assert!(!handle.contacted(), "flag must start false");

    let port = handle.port();
    // Bare TCP connection that sends NO parseable HTTP request. The flag must
    // still flip, proving the seam is at accept (not after HTTP parse).
    let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect to bridge");

    // accept + flag store happen in the spawned accept_loop; bounded poll.
    let mut flipped = false;
    for _ in 0..200 {
        if handle.contacted() {
            flipped = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        flipped,
        "contacted() must flip true on the first accepted TCP connection"
    );

    drop(stream);
    handle.begin_shutdown();
}
