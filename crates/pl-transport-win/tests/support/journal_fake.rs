// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Shared TLS/journal fakes for the integration-test binaries.
//!
//! `self_signed` and `server_config` were byte-identical copies in
//! `relay_round_trip.rs` and `upload_lifecycle_log.rs`; they live here once now.
//! The credential builders and frame readers those files carry are *not*
//! identical to each other, so they stay where they are.

#![allow(dead_code)] // Each integration-test binary compiles this shared helper independently.

use std::sync::Arc;

use observer_pl::frame::{FrameDecoder, FLAG_CLOSE, FLAG_DATA};
use pl_transport_win::credential::{Credential, EndpointAddr};
use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::ServerConfig;
use tokio::io::AsyncReadExt;

pub fn self_signed() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
    let params = CertificateParams::new(vec!["spl.local".to_string()]).unwrap();
    let cert = params.self_signed(&key).unwrap();
    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
    (cert_der, key_der)
}

pub fn server_config(cert: CertificateDer<'static>, key: PrivateKeyDer<'static>) -> ServerConfig {
    ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap()
}

/// A direct-only observer credential pointing at one loopback endpoint.
pub fn direct_credential(pin: Vec<u8>, port: u16) -> Credential {
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
    let params = CertificateParams::new(vec!["observer.test".to_string()]).unwrap();
    let cert = params.self_signed(&key).unwrap();
    Credential {
        client_key_pem: key.serialize_pem(),
        client_cert_pem: cert.pem(),
        ca_chain_pem: vec![cert.pem()],
        ca_fp_prefix: pin,
        instance_id: "test-instance".into(),
        home_label: "Home".into(),
        endpoints: vec![EndpointAddr {
            host: "127.0.0.1".into(),
            port,
        }],
        relay_origin: None,
        device_token: None,
        device_token_expires_at: None,
    }
}

/// Read one framed PL request off a server-side TLS stream, returning the stream
/// id it arrived on and the reassembled request bytes.
pub async fn read_framed_request(
    tls: &mut tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
) -> (u32, Vec<u8>) {
    let mut decoder = FrameDecoder::new();
    let mut request = Vec::new();
    let mut stream_id = 1u32;
    let mut closed = false;
    let mut buf = [0u8; 4096];
    while !closed {
        let n = tls.read(&mut buf).await.unwrap();
        if n == 0 {
            break;
        }
        decoder.feed(&buf[..n]);
        for frame in decoder.drain().unwrap() {
            stream_id = frame.stream_id;
            if frame.flags & FLAG_DATA != 0 {
                request.extend_from_slice(&frame.payload);
            }
            if frame.flags & FLAG_CLOSE != 0 {
                closed = true;
            }
        }
    }
    (stream_id, request)
}
