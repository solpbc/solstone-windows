// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Production-helper coverage for the ordinary-request authority registry.

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use observer_pl::frame::{
    Frame, FrameDecoder, FLAG_CLOSE, FLAG_DATA, FLAG_WINDOW, RECOMMENDED_CHUNK,
};
use observer_pl::ingest::{FilePart, IngestStatus};
use observer_pl::mux::{MuxError, INITIAL_WINDOW};
use pl_transport_win::client::ObserverClient;
use pl_transport_win::{transport_error_code, ClientSlot, TransportError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

use support::journal_fake::{direct_credential, read_framed_request, self_signed, server_config};

const DAY: &str = "20260914";
const POST_CONNECT_CAP: usize = 64 * 1024;

#[derive(Debug, Clone, Copy)]
enum DirectDropPoint {
    Before,
    Partial,
    Complete,
}

fn http_response_with_total(total: usize, prefix: &[u8], fill: u8) -> Vec<u8> {
    let mut body = prefix.to_vec();
    loop {
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        let desired_body_len = total
            .checked_sub(head.len())
            .expect("response cap accommodates HTTP response head");
        assert!(desired_body_len >= prefix.len());
        if desired_body_len == body.len() {
            let mut response = head.into_bytes();
            response.extend_from_slice(&body);
            return response;
        }
        body.truncate(prefix.len());
        body.resize(desired_body_len, fill);
    }
}

async fn write_response(
    tls: &mut tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    stream_id: u32,
    response: Vec<u8>,
) {
    // Stay below one entire receive window so the final assembled-cap chunk
    // cannot race the peer's WINDOW grant with TLS close notification.
    let chunks = response.chunks(RECOMMENDED_CHUNK / 4).collect::<Vec<_>>();
    let mut credit = INITIAL_WINDOW as i64;
    let mut decoder = FrameDecoder::new();
    let mut read_buf = [0u8; 4096];
    for (index, chunk) in chunks.iter().enumerate() {
        while credit < chunk.len() as i64 {
            let read = tls.read(&mut read_buf).await.unwrap();
            assert!(read > 0, "client closed before granting response credit");
            decoder.feed(&read_buf[..read]);
            for frame in decoder.drain().unwrap() {
                if frame.stream_id == stream_id
                    && frame.flags & FLAG_WINDOW != 0
                    && frame.payload.len() == 4
                {
                    credit += i64::from(u32::from_be_bytes(frame.payload[..4].try_into().unwrap()));
                }
            }
        }
        let flags = FLAG_DATA | ((index + 1 == chunks.len()) as u8 * FLAG_CLOSE);
        let frame = Frame::new(stream_id, flags, chunk.to_vec());
        tls.write_all(&frame.encode().unwrap()).await.unwrap();
        credit -= chunk.len() as i64;
    }
    tls.flush().await.unwrap();
    // The mux CLOSE is the response terminator. Keep TLS alive until the
    // production client closes so an exact-cap final DATA frame is not raced by
    // a TLS close notification from this test peer.
    let mut drain = [0u8; 1024];
    while tls.read(&mut drain).await.unwrap_or(0) != 0 {}
}

async fn serve_direct_once(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    accepts: Arc<AtomicUsize>,
    response: Vec<u8>,
) -> Vec<u8> {
    let (tcp, _) = listener.accept().await.unwrap();
    accepts.fetch_add(1, Ordering::SeqCst);
    let mut tls = acceptor.accept(tcp).await.unwrap();
    let (stream_id, request) = read_framed_request(&mut tls).await;
    write_response(&mut tls, stream_id, response).await;
    request
}

async fn serve_direct_until_quiet(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    accepts: Arc<AtomicUsize>,
    response: Vec<u8>,
) {
    while let Ok(Ok((tcp, _))) =
        tokio::time::timeout(Duration::from_secs(2), listener.accept()).await
    {
        accepts.fetch_add(1, Ordering::SeqCst);
        let mut tls = acceptor.accept(tcp).await.unwrap();
        let (stream_id, _) = read_framed_request(&mut tls).await;
        write_response(&mut tls, stream_id, response.clone()).await;
    }
}

async fn serve_direct_drop_then_response(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    accepts: Arc<AtomicUsize>,
    drop_point: DirectDropPoint,
    response: Vec<u8>,
) -> Vec<Vec<u8>> {
    let (tcp, _) = listener.accept().await.unwrap();
    accepts.fetch_add(1, Ordering::SeqCst);
    match drop_point {
        DirectDropPoint::Before => drop(tcp),
        DirectDropPoint::Partial | DirectDropPoint::Complete => {
            let mut tls = acceptor.accept(tcp).await.unwrap();
            match drop_point {
                DirectDropPoint::Partial => {
                    let mut one_byte = [0u8; 1];
                    let _ = tls.read(&mut one_byte).await.unwrap();
                }
                DirectDropPoint::Complete => {
                    let _ = read_framed_request(&mut tls).await;
                }
                DirectDropPoint::Before => unreachable!(),
            }
        }
    }

    let (tcp, _) = listener.accept().await.unwrap();
    accepts.fetch_add(1, Ordering::SeqCst);
    let mut tls = acceptor.accept(tcp).await.unwrap();
    let (stream_id, request) = read_framed_request(&mut tls).await;
    write_response(&mut tls, stream_id, response).await;
    vec![request]
}

async fn serve_direct_drop_once(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    accepts: Arc<AtomicUsize>,
    drop_point: DirectDropPoint,
) {
    let (tcp, _) = listener.accept().await.unwrap();
    accepts.fetch_add(1, Ordering::SeqCst);
    match drop_point {
        DirectDropPoint::Before => drop(tcp),
        DirectDropPoint::Partial | DirectDropPoint::Complete => {
            let mut tls = acceptor.accept(tcp).await.unwrap();
            match drop_point {
                DirectDropPoint::Partial => {
                    let mut one_byte = [0u8; 1];
                    let _ = tls.read(&mut one_byte).await.unwrap();
                }
                DirectDropPoint::Complete => {
                    let _ = read_framed_request(&mut tls).await;
                }
                DirectDropPoint::Before => unreachable!(),
            }
        }
    }
}

fn response_body(request: &[u8]) -> &'static [u8] {
    let request = String::from_utf8_lossy(request);
    if request.starts_with("POST /app/devices/ingest ") {
        br#"{"status":"ok","segment":"120000_300"}"#
    } else if request.starts_with("GET /app/devices/ingest/manifest/") {
        br#"{"version":1,"day":"20260914","segments":{}}"#
    } else if request.starts_with("GET /app/devices/ingest/manifest ") {
        br#"{"days":{"20260914":{"segments":0}}}"#
    } else if request.starts_with("GET /app/devices/ingest/segments/") {
        br#"{"items":[],"total":0,"protocol_version":3}"#
    } else if request.starts_with("GET /api/system/status ") {
        br#"{"version":{"current":"2026.9.14"}}"#
    } else {
        br#"{}"#
    }
}

async fn serve_ordinary_routes(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    accepts: Arc<AtomicUsize>,
) -> Vec<Vec<u8>> {
    let mut requests = Vec::new();
    for _ in 0..8 {
        let (tcp, _) = listener.accept().await.unwrap();
        accepts.fetch_add(1, Ordering::SeqCst);
        let mut tls = acceptor.accept(tcp).await.unwrap();
        let (stream_id, request) = read_framed_request(&mut tls).await;
        let body = response_body(&request);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            String::from_utf8_lossy(body)
        );
        let frame = Frame::new(stream_id, FLAG_DATA | FLAG_CLOSE, response.into_bytes());
        tls.write_all(&frame.encode().unwrap()).await.unwrap();
        tls.flush().await.unwrap();
        let _ = tls.shutdown().await;
        requests.push(request);
    }
    requests
}

async fn serve_one_system_status(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    accepts: Arc<AtomicUsize>,
) {
    let (tcp, _) = listener.accept().await.unwrap();
    accepts.fetch_add(1, Ordering::SeqCst);
    let mut tls = acceptor.accept(tcp).await.unwrap();
    let (stream_id, request) = read_framed_request(&mut tls).await;
    assert!(String::from_utf8_lossy(&request).starts_with("GET /api/system/status HTTP/1.1"));
    let body = br#"{"version":{"current":"2026.9.14"}}"#;
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        String::from_utf8_lossy(body)
    );
    let frame = Frame::new(stream_id, FLAG_DATA | FLAG_CLOSE, response.into_bytes());
    tls.write_all(&frame.encode().unwrap()).await.unwrap();
    tls.flush().await.unwrap();
    let _ = tls.shutdown().await;
}

async fn ordinary_client() -> (ObserverClient, Arc<AtomicUsize>, JoinHandle<Vec<Vec<u8>>>) {
    let (cert, key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepts = Arc::new(AtomicUsize::new(0));
    let server = tokio::spawn(serve_ordinary_routes(
        listener,
        TlsAcceptor::from(Arc::new(server_config(cert, key))),
        accepts.clone(),
    ));
    (
        ObserverClient::new(direct_credential(pin, port)).unwrap(),
        accepts,
        server,
    )
}

fn relay_only_credential(pin: Vec<u8>, relay_port: u16) -> pl_transport_win::Credential {
    let mut credential = direct_credential(pin, 9);
    credential.endpoints.clear();
    credential.relay_origin = Some(format!("http://127.0.0.1:{relay_port}"));
    credential.device_token = Some("test-device-token".into());
    credential.device_token_expires_at = Some(1_900_000_000);
    credential
}

async fn assert_no_relay_tcp_dial(listener: &TcpListener) {
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err(),
        "fenced ordinary request reached the relay listener"
    );
}

#[tokio::test]
async fn all_eight_production_helpers_use_the_shared_ordinary_request_authority() {
    let (client, accepts, server) = ordinary_client().await;

    client.get_clients_self().await.unwrap();
    client
        .put_clients_self(br#"{"label":"desk"}"#)
        .await
        .unwrap();
    client.get_relay_access().await.unwrap();
    let (ingest, _) = client
        .ingest(
            "120000_300",
            DAY,
            vec![FilePart {
                filename: "proof.bin".into(),
                content_type: "application/octet-stream".into(),
                bytes: vec![1, 2, 3],
            }],
        )
        .await
        .unwrap();
    assert_eq!(ingest.status, IngestStatus::Ok);
    client.ingest_manifest().await.unwrap();
    client.ingest_manifest_day(DAY).await.unwrap();
    client.list_segments(DAY).await.unwrap();
    assert_eq!(client.system_status().await.unwrap(), "2026.9.14");

    let requests = server.await.unwrap();
    assert_eq!(accepts.load(Ordering::SeqCst), 8);
    let targets: Vec<_> = requests
        .iter()
        .map(|request| {
            String::from_utf8_lossy(request)
                .lines()
                .next()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(
        targets,
        [
            "GET /app/network/api/clients/self HTTP/1.1",
            "PUT /app/network/api/clients/self HTTP/1.1",
            "GET /app/network/api/relay/access HTTP/1.1",
            "POST /app/devices/ingest HTTP/1.1",
            "GET /app/devices/ingest/manifest HTTP/1.1",
            "GET /app/devices/ingest/manifest/20260914 HTTP/1.1",
            "GET /app/devices/ingest/segments/20260914 HTTP/1.1",
            "GET /api/system/status HTTP/1.1",
        ]
    );
    for request in &requests {
        let request = String::from_utf8_lossy(request);
        assert_eq!(request.matches("X-Solstone-Protocol-Version: 3").count(), 1);
        assert!(!request.contains("Authorization:"));
        assert!(!request.contains("X-Solstone-Observer:"));
    }
    assert!(String::from_utf8_lossy(&requests[1]).contains("content-type: application/json"));
    assert!(String::from_utf8_lossy(&requests[3]).contains("Content-Type: multipart/form-data"));
    assert!(String::from_utf8_lossy(&requests[7]).contains("Cache-Control: no-cache"));
}

#[tokio::test]
async fn relay_fencing_does_not_block_a_reachable_direct_ordinary_request() {
    let (cert, key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepts = Arc::new(AtomicUsize::new(0));
    let server = tokio::spawn(serve_one_system_status(
        listener,
        TlsAcceptor::from(Arc::new(server_config(cert, key))),
        accepts.clone(),
    ));
    let client = Arc::new(ObserverClient::new(direct_credential(pin, port)).unwrap());
    let slot = ClientSlot::new(client);

    slot.disable_relay();
    assert_eq!(slot.load().system_status().await.unwrap(), "2026.9.14");
    server.await.unwrap();
    assert_eq!(accepts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn direct_replay_safe_helpers_retry_before_and_during_request_writes() {
    for drop_point in [DirectDropPoint::Before, DirectDropPoint::Partial] {
        let (cert, key) = self_signed();
        let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepts = Arc::new(AtomicUsize::new(0));
        let server = tokio::spawn(serve_direct_drop_then_response(
            listener,
            TlsAcceptor::from(Arc::new(server_config(cert, key))),
            accepts.clone(),
            drop_point,
            http_response_with_total(256, br#"{"items":[],"total":0,"protocol_version":3}"#, b' '),
        ));
        let client = ObserverClient::new(direct_credential(pin, port)).unwrap();

        client.list_segments(DAY).await.unwrap();
        server.await.unwrap();
        assert!(
            accepts.load(Ordering::SeqCst) > 1,
            "ReplaySafe list_segments did not retry after {drop_point:?}"
        );
    }
}

#[tokio::test]
async fn direct_forbid_after_write_never_retries_partial_or_complete_put() {
    for drop_point in [DirectDropPoint::Partial, DirectDropPoint::Complete] {
        let (cert, key) = self_signed();
        let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepts = Arc::new(AtomicUsize::new(0));
        let server = tokio::spawn(serve_direct_drop_once(
            listener,
            TlsAcceptor::from(Arc::new(server_config(cert, key))),
            accepts.clone(),
            drop_point,
        ));
        let client = ObserverClient::new(direct_credential(pin, port)).unwrap();

        let error = client
            .put_clients_self(br#"{"label":"desk"}"#)
            .await
            .unwrap_err();
        assert!(matches!(error, TransportError::ReplayUnsafe));
        assert_eq!(transport_error_code(&error), "replay_unsafe");
        server.await.unwrap();
        assert_eq!(
            accepts.load(Ordering::SeqCst),
            1,
            "ForbidAfterWrite PUT retried after a write"
        );
    }
}

#[tokio::test]
async fn direct_shared_request_caps_use_exact_assembled_wire_lengths() {
    for (total, prefix, fill, manifest) in [
        (POST_CONNECT_CAP, b"x".as_slice(), b'x', false),
        (
            observer_pl::mux::MAX_ASSEMBLED_BYTES,
            br#"{"days":{}}"#.as_slice(),
            b' ',
            true,
        ),
    ] {
        for over_cap in [false, true] {
            let (cert, key) = self_signed();
            let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let accepts = Arc::new(AtomicUsize::new(0));
            let response = http_response_with_total(total + usize::from(over_cap), prefix, fill);
            let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
            let server: JoinHandle<()> = if over_cap {
                tokio::spawn(serve_direct_until_quiet(
                    listener,
                    acceptor,
                    accepts.clone(),
                    response,
                ))
            } else {
                let server_accepts = accepts.clone();
                tokio::spawn(async move {
                    let _ = serve_direct_once(listener, acceptor, server_accepts, response).await;
                })
            };
            let client = ObserverClient::new(direct_credential(pin, port)).unwrap();

            let result = if manifest {
                client.ingest_manifest().await.map(|_| ())
            } else {
                client.get_clients_self().await.map(|_| ())
            };
            server.await.unwrap();
            if over_cap {
                assert!(matches!(
                    result,
                    Err(TransportError::Mux(MuxError::CapExceeded))
                ));
            } else {
                assert!(
                    result.is_ok(),
                    "exact assembled cap {total} (manifest={manifest}) failed: {result:?}"
                );
            }
            assert_eq!(accepts.load(Ordering::SeqCst), 1);
        }
    }
}

#[tokio::test]
async fn stale_slot_incarnation_blocks_old_relay_only_helper_without_a_relay_dial() {
    let (relay_cert, _) = self_signed();
    let relay_pin = spl_core::ca::sha256(relay_cert.as_ref())[..16].to_vec();
    let relay_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let credential = relay_only_credential(relay_pin, relay_listener.local_addr().unwrap().port());
    let old = Arc::new(ObserverClient::new(credential.clone()).unwrap());
    let slot = ClientSlot::new(old.clone());
    let cas = old.current_cas_key().unwrap();
    slot.replace_from_incumbent(credential, cas).unwrap();

    assert!(matches!(
        old.system_status().await,
        Err(TransportError::RelayRetired)
    ));
    assert_no_relay_tcp_dial(&relay_listener).await;

    let (cert, key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepts = Arc::new(AtomicUsize::new(0));
    let server = tokio::spawn(serve_one_system_status(
        listener,
        TlsAcceptor::from(Arc::new(server_config(cert, key))),
        accepts.clone(),
    ));
    let direct = ObserverClient::new(direct_credential(pin, port)).unwrap();
    assert_eq!(direct.system_status().await.unwrap(), "2026.9.14");
    server.await.unwrap();
    assert_eq!(accepts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn retired_slot_blocks_relay_only_helper_without_a_relay_dial() {
    let (relay_cert, _) = self_signed();
    let relay_pin = spl_core::ca::sha256(relay_cert.as_ref())[..16].to_vec();
    let relay_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let credential = relay_only_credential(relay_pin, relay_listener.local_addr().unwrap().port());
    let client = Arc::new(ObserverClient::new(credential).unwrap());
    let slot = ClientSlot::new(client.clone());
    slot.retire();

    assert!(matches!(
        client.system_status().await,
        Err(TransportError::RelayRetired)
    ));
    assert_no_relay_tcp_dial(&relay_listener).await;

    let (cert, key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepts = Arc::new(AtomicUsize::new(0));
    let server = tokio::spawn(serve_one_system_status(
        listener,
        TlsAcceptor::from(Arc::new(server_config(cert, key))),
        accepts.clone(),
    ));
    let direct = ObserverClient::new(direct_credential(pin, port)).unwrap();
    assert_eq!(direct.system_status().await.unwrap(), "2026.9.14");
    server.await.unwrap();
    assert_eq!(accepts.load(Ordering::SeqCst), 1);
}
