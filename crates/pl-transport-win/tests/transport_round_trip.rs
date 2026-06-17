// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! End-to-end transport round-trip against a real in-process rustls peer.
//!
//! Stands up a tokio-rustls TLS server presenting a self-signed cert, then dials
//! it with the production `request_once` path: real TCP, real TLS 1.3 handshake,
//! real CA-fingerprint pinning + leaf-signature verification, real spl framing,
//! real HTTP-over-PL. Nothing is mocked — only the journal application logic is
//! replaced by a fixed echo. This is the deterministic, host-runnable proxy for
//! the live cross-repo gate.

use std::sync::Arc;

use observer_pl::frame::{Frame, FrameDecoder, FLAG_CLOSE, FLAG_DATA, FLAG_RESET, FLAG_WINDOW};
use observer_pl::mux::INITIAL_WINDOW;
use pl_transport_win::connection::request_once;
use pl_transport_win::tls::pairing_config;
use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::ServerConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

fn self_signed() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
    let params = CertificateParams::new(vec!["spl.local".to_string()]).unwrap();
    let cert = params.self_signed(&key).unwrap();
    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
    (cert_der, key_der)
}

fn server_config(cert: CertificateDer<'static>, key: PrivateKeyDer<'static>) -> ServerConfig {
    ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap()
}

/// Accept one TLS connection, read the framed HTTP request, and frame back a
/// fixed `{"status":"ok"}` response on the same stream. Returns the request body
/// it received so the test can assert the wire bytes.
async fn serve_one(listener: TcpListener, acceptor: TlsAcceptor) -> Vec<u8> {
    let (tcp, _) = listener.accept().await.unwrap();
    let mut tls = acceptor.accept(tcp).await.unwrap();

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

    let body = b"{\"status\":\"ok\"}";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        String::from_utf8_lossy(body)
    );
    let frame = Frame::new(stream_id, FLAG_DATA | FLAG_CLOSE, response.into_bytes());
    tls.write_all(&frame.encode().unwrap()).await.unwrap();
    tls.flush().await.unwrap();
    let _ = tls.shutdown().await;
    request
}

#[tokio::test]
async fn round_trips_request_over_real_tls_and_framing() {
    let (cert, key) = self_signed();
    let pin = observer_pl::ca::sha256(cert.as_ref())[..16].to_vec();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_one(listener, acceptor));

    let config = Arc::new(pairing_config(&pin).unwrap());
    let request_body = b"{\"csr\":\"PEM\",\"device_label\":\"win\"}";
    let response = request_once(
        config,
        "127.0.0.1",
        port,
        "POST",
        "/app/link/pair?token=abc123",
        &[("Content-Type".to_string(), "application/json".to_string())],
        request_body,
    )
    .await
    .expect("request should succeed against the pinned peer");

    assert_eq!(response.status, 200);
    assert_eq!(response.body_text(), "{\"status\":\"ok\"}");

    // The server received exactly the HTTP request our transport framed.
    let received = server.await.unwrap();
    let received_text = String::from_utf8_lossy(&received);
    assert!(received_text.starts_with("POST /app/link/pair?token=abc123 HTTP/1.1\r\n"));
    assert!(received_text.contains("host: spl.local\r\n"));
    assert!(received_text.contains("Content-Type: application/json\r\n"));
    assert!(received_text.ends_with("{\"csr\":\"PEM\",\"device_label\":\"win\"}"));
}

/// A flow-control-enforcing peer, byte-identical in policy to the journal's
/// `convey/secure_listener/mux.py`: it advertises a 1 MiB recv window, **RESETs**
/// the stream if a DATA frame would overrun the un-granted window, and grants a
/// `WINDOW` frame once 50% is consumed. A client that blasted the whole body
/// up front (the old non-windowed path) would overrun and get RESET here; only a
/// correctly-paced [`WindowedUpload`] completes. Returns the assembled request.
async fn serve_one_with_flow_control(listener: TcpListener, acceptor: TlsAcceptor) -> Vec<u8> {
    let (tcp, _) = listener.accept().await.unwrap();
    let mut tls = acceptor.accept(tcp).await.unwrap();

    let mut decoder = FrameDecoder::new();
    let mut request = Vec::new();
    let mut stream_id = 1u32;
    let mut closed = false;
    let mut recv_credit: i64 = INITIAL_WINDOW as i64;
    let mut unacked: i64 = 0;
    let mut buf = [0u8; 16 * 1024];
    while !closed {
        let n = tls.read(&mut buf).await.unwrap();
        if n == 0 {
            break;
        }
        decoder.feed(&buf[..n]);
        for frame in decoder.drain().unwrap() {
            stream_id = frame.stream_id;
            if frame.flags & FLAG_DATA != 0 {
                let len = frame.payload.len() as i64;
                if len > recv_credit {
                    // Window overrun — exactly what the journal refuses. Prove the
                    // client never does this by RESETing if it ever happens.
                    let reset = Frame::new(stream_id, FLAG_RESET, vec![0x03]); // protocol error
                    tls.write_all(&reset.encode().unwrap()).await.unwrap();
                    tls.flush().await.unwrap();
                    return request; // request stays short → test assertion fails loudly
                }
                recv_credit -= len;
                unacked += len;
                request.extend_from_slice(&frame.payload);
                // Replenish at 50% consumed, granting back exactly what we drained.
                if unacked >= (INITIAL_WINDOW as i64) / 2 {
                    let grant = unacked as u32;
                    recv_credit += unacked;
                    unacked = 0;
                    let window = Frame::new(stream_id, FLAG_WINDOW, grant.to_be_bytes().to_vec());
                    tls.write_all(&window.encode().unwrap()).await.unwrap();
                    tls.flush().await.unwrap();
                }
            }
            if frame.flags & FLAG_CLOSE != 0 {
                closed = true;
            }
        }
    }

    let body = b"{\"status\":\"accepted\"}";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        String::from_utf8_lossy(body)
    );
    let frame = Frame::new(stream_id, FLAG_DATA | FLAG_CLOSE, response.into_bytes());
    tls.write_all(&frame.encode().unwrap()).await.unwrap();
    tls.flush().await.unwrap();
    let _ = tls.shutdown().await;
    request
}

#[tokio::test]
async fn streams_multi_mib_body_under_window_flow_control() {
    let (cert, key) = self_signed();
    let pin = observer_pl::ca::sha256(cert.as_ref())[..16].to_vec();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_one_with_flow_control(listener, acceptor));

    // ~2.5 MiB body — well past the 1 MiB initial window, so the upload only
    // completes if the client paces to WINDOW grants (an encoded screen segment
    // is ~37.5 MB; this is the same path at test scale).
    let big_body = vec![0x7Cu8; INITIAL_WINDOW * 2 + INITIAL_WINDOW / 2 + 123];
    let config = Arc::new(pairing_config(&pin).unwrap());
    let response = request_once(
        config,
        "127.0.0.1",
        port,
        "POST",
        "/app/observer/ingest",
        &[(
            "Content-Type".to_string(),
            "application/octet-stream".to_string(),
        )],
        &big_body,
    )
    .await
    .expect("a >1 MiB body must stream to completion under flow control");

    assert_eq!(response.status, 200);
    assert_eq!(response.body_text(), "{\"status\":\"accepted\"}");

    // The server received the entire framed request, body intact and in order.
    let received = server.await.unwrap();
    assert!(
        received.len() > INITIAL_WINDOW * 2,
        "server should have received the whole multi-MiB request, got {} bytes",
        received.len()
    );
    let received_text = String::from_utf8_lossy(&received[..received.len().min(256)]);
    assert!(received_text.starts_with("POST /app/observer/ingest HTTP/1.1\r\n"));
    assert!(received.ends_with(&big_body));
}

#[tokio::test]
async fn wrong_pin_fails_the_handshake() {
    let (cert, key) = self_signed();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    // Server task may error when the client aborts the handshake; ignore it.
    let _server = tokio::spawn(async move {
        if let Ok((tcp, _)) = listener.accept().await {
            let _ = acceptor.accept(tcp).await;
        }
    });

    // Pin a fingerprint that does not match the server cert.
    let wrong_pin = vec![0xFFu8; 16];
    let config = Arc::new(pairing_config(&wrong_pin).unwrap());
    let result = request_once(config, "127.0.0.1", port, "GET", "/healthz", &[], b"").await;
    assert!(result.is_err(), "a wrong CA-fp pin must fail the handshake");
}
