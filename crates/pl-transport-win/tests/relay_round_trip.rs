// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! End-to-end SPL relay carrier tests over a real loopback WebSocket relay.
//!
//! The loopback relay intentionally uses `ws://127.0.0.1` rather than WSS, so it
//! does not exercise the production outer WebPKI leg. It is otherwise a dumb
//! opaque pump: WS binary frames are bridged to an in-memory byte stream whose
//! far side runs the same pinned inner TLS + PL framing server used by the LAN
//! round-trip tests.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use observer_pl::frame::{Frame, FrameDecoder, FLAG_CLOSE, FLAG_DATA, FLAG_RESET, FLAG_WINDOW};
use observer_pl::http::HttpResponse;
use observer_pl::mux::INITIAL_WINDOW;
use pl_transport_win::relay::{dial_relay_ws, request_once_over_ws, request_once_relay};
use pl_transport_win::tls::pairing_config;
use pl_transport_win::{RelayError, TransportError};
use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ClientConfig, ServerConfig};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{accept_async, accept_hdr_async, WebSocketStream};

const RELAY_TOKEN: &str = "test-device-token";
const CARRIER_READ_BUF_BYTES: usize = 64 * 1024;
const LARGE_WS_FRAME_BYTES: usize = 256 * 1024;
const LARGE_RESPONSE_BYTES: usize = 512 * 1024 + 137;

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

fn client_config(pin: &[u8]) -> Arc<ClientConfig> {
    Arc::new(pairing_config(pin).unwrap())
}

fn unused_outer_config() -> Arc<ClientConfig> {
    client_config(&[0xAA; 16])
}

async fn pump_ws(
    ws: WebSocketStream<TcpStream>,
    relay_side: DuplexStream,
    capture_first_inbound: Option<Arc<Mutex<Vec<u8>>>>,
) -> io::Result<()> {
    let (mut ws_sink, mut ws_stream) = ws.split();
    let (mut relay_read, mut relay_write) = tokio::io::split(relay_side);

    let to_inner = async move {
        while let Some(message) = ws_stream.next().await {
            match message.map_err(|_| {
                io::Error::new(io::ErrorKind::BrokenPipe, "relay websocket receive failed")
            })? {
                Message::Binary(bytes) => {
                    if let Some(capture) = &capture_first_inbound {
                        let mut guard = capture.lock().unwrap();
                        if guard.is_empty() {
                            guard.extend_from_slice(&bytes);
                        }
                    }
                    relay_write.write_all(&bytes).await?;
                    relay_write.flush().await?;
                }
                Message::Close(_) => {
                    let _ = relay_write.shutdown().await;
                    return Ok(());
                }
                Message::Ping(_) | Message::Pong(_) => {}
                Message::Text(_) | Message::Frame(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "relay tunnel received non-binary message",
                    ));
                }
            }
        }
        Ok(())
    };

    let to_ws = async move {
        let mut buf = [0u8; 512];
        loop {
            let n = relay_read.read(&mut buf).await?;
            if n == 0 {
                let _ = ws_sink.close().await;
                return Ok(());
            }
            ws_sink
                .send(Message::Binary(buf[..n].to_vec().into()))
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "relay websocket send failed")
                })?;
        }
    };

    tokio::select! {
        result = to_inner => result,
        result = to_ws => result,
    }
}

async fn pump_ws_large_response_frames(
    ws: WebSocketStream<TcpStream>,
    relay_side: DuplexStream,
    large_response_mode: Arc<AtomicBool>,
    sent_frame_sizes: Arc<Mutex<Vec<usize>>>,
) -> io::Result<()> {
    let (mut ws_sink, mut ws_stream) = ws.split();
    let (mut relay_read, mut relay_write) = tokio::io::split(relay_side);

    let to_inner = async move {
        while let Some(message) = ws_stream.next().await {
            match message.map_err(|_| {
                io::Error::new(io::ErrorKind::BrokenPipe, "relay websocket receive failed")
            })? {
                Message::Binary(bytes) => {
                    relay_write.write_all(&bytes).await?;
                    relay_write.flush().await?;
                }
                Message::Close(_) => {
                    let _ = relay_write.shutdown().await;
                    return Ok(());
                }
                Message::Ping(_) | Message::Pong(_) => {}
                Message::Text(_) | Message::Frame(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "relay tunnel received non-binary message",
                    ));
                }
            }
        }
        Ok(())
    };

    let to_ws = async move {
        let mut buf = vec![0u8; LARGE_WS_FRAME_BYTES];
        let mut pending = Vec::new();
        loop {
            let n = relay_read.read(&mut buf).await?;
            if n == 0 {
                if !pending.is_empty() {
                    let frame = std::mem::take(&mut pending);
                    sent_frame_sizes.lock().unwrap().push(frame.len());
                    ws_sink
                        .send(Message::Binary(frame.into()))
                        .await
                        .map_err(|_| {
                            io::Error::new(io::ErrorKind::BrokenPipe, "relay websocket send failed")
                        })?;
                }
                let _ = ws_sink.close().await;
                return Ok(());
            }

            if !large_response_mode.load(Ordering::SeqCst) {
                ws_sink
                    .send(Message::Binary(buf[..n].to_vec().into()))
                    .await
                    .map_err(|_| {
                        io::Error::new(io::ErrorKind::BrokenPipe, "relay websocket send failed")
                    })?;
                continue;
            }

            pending.extend_from_slice(&buf[..n]);
            if pending.len() >= LARGE_WS_FRAME_BYTES {
                let frame = std::mem::take(&mut pending);
                sent_frame_sizes.lock().unwrap().push(frame.len());
                ws_sink
                    .send(Message::Binary(frame.into()))
                    .await
                    .map_err(|_| {
                        io::Error::new(io::ErrorKind::BrokenPipe, "relay websocket send failed")
                    })?;
            }
        }
    };

    tokio::select! {
        result = to_inner => result,
        result = to_ws => result,
    }
}

async fn accept_relay_stream(
    listener: TcpListener,
    capture_first_inbound: Option<Arc<Mutex<Vec<u8>>>>,
    duplex_capacity: usize,
) -> DuplexStream {
    let (tcp, _) = listener.accept().await.unwrap();
    let ws = accept_async(tcp).await.unwrap();
    let (relay_side, server_side) = tokio::io::duplex(duplex_capacity);
    tokio::spawn(async move {
        let _ = pump_ws(ws, relay_side, capture_first_inbound).await;
    });
    server_side
}

async fn accept_relay_stream_large_response_frames(
    listener: TcpListener,
    large_response_mode: Arc<AtomicBool>,
    sent_frame_sizes: Arc<Mutex<Vec<usize>>>,
) -> DuplexStream {
    let (tcp, _) = listener.accept().await.unwrap();
    let ws = accept_async(tcp).await.unwrap();
    let (relay_side, server_side) = tokio::io::duplex(LARGE_RESPONSE_BYTES);
    tokio::spawn(async move {
        let _ =
            pump_ws_large_response_frames(ws, relay_side, large_response_mode, sent_frame_sizes)
                .await;
    });
    server_side
}

async fn serve_stream_response<S>(
    stream: S,
    acceptor: TlsAcceptor,
    status: &'static str,
    body: &'static [u8],
) -> Vec<u8>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut tls = acceptor.accept(stream).await.unwrap();

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

    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        String::from_utf8_lossy(body)
    );
    let frame = Frame::new(stream_id, FLAG_DATA | FLAG_CLOSE, response.into_bytes());
    tls.write_all(&frame.encode().unwrap()).await.unwrap();
    tls.flush().await.unwrap();
    let _ = tls.shutdown().await;
    request
}

async fn serve_stream_large_response<S>(
    stream: S,
    acceptor: TlsAcceptor,
    body: Vec<u8>,
    large_response_mode: Arc<AtomicBool>,
) -> Vec<u8>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut tls = acceptor.accept(stream).await.unwrap();

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

    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(&body);
    let frame = Frame::new(stream_id, FLAG_DATA | FLAG_CLOSE, response);
    large_response_mode.store(true, Ordering::SeqCst);
    tls.write_all(&frame.encode().unwrap()).await.unwrap();
    tls.flush().await.unwrap();
    let _ = tls.shutdown().await;
    request
}

async fn serve_stream_with_flow_control<S>(stream: S, acceptor: TlsAcceptor) -> Vec<u8>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut tls = acceptor.accept(stream).await.unwrap();

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
                    let reset = Frame::new(stream_id, FLAG_RESET, vec![0x03]);
                    tls.write_all(&reset.encode().unwrap()).await.unwrap();
                    tls.flush().await.unwrap();
                    return request;
                }
                recv_credit -= len;
                unacked += len;
                request.extend_from_slice(&frame.payload);
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

async fn spawn_response_relay(
    acceptor: TlsAcceptor,
    status: &'static str,
    body: &'static [u8],
    capture_first_inbound: Option<Arc<Mutex<Vec<u8>>>>,
    duplex_capacity: usize,
) -> (String, JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let stream = accept_relay_stream(listener, capture_first_inbound, duplex_capacity).await;
        serve_stream_response(stream, acceptor, status, body).await
    });
    (origin, task)
}

async fn spawn_flow_relay(acceptor: TlsAcceptor) -> (String, JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let stream = accept_relay_stream(listener, None, 1024).await;
        serve_stream_with_flow_control(stream, acceptor).await
    });
    (origin, task)
}

async fn spawn_large_response_relay(
    acceptor: TlsAcceptor,
    body: Vec<u8>,
) -> (String, JoinHandle<Vec<u8>>, Arc<Mutex<Vec<usize>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let large_response_mode = Arc::new(AtomicBool::new(false));
    let sent_frame_sizes = Arc::new(Mutex::new(Vec::new()));
    let task = tokio::spawn({
        let large_response_mode = large_response_mode.clone();
        let sent_frame_sizes = sent_frame_sizes.clone();
        async move {
            let stream = accept_relay_stream_large_response_frames(
                listener,
                large_response_mode.clone(),
                sent_frame_sizes,
            )
            .await;
            serve_stream_large_response(stream, acceptor, body, large_response_mode).await
        }
    });
    (origin, task, sent_frame_sizes)
}

fn tls_pair() -> (Arc<ClientConfig>, TlsAcceptor) {
    let (cert, key) = self_signed();
    let pin = observer_pl::ca::sha256(cert.as_ref())[..16].to_vec();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    (client_config(&pin), acceptor)
}

fn assert_relay_error<T>(result: Result<T, TransportError>, expected: RelayError) {
    match result {
        Err(TransportError::Relay(actual)) => assert_eq!(actual, expected),
        Err(other) => panic!("expected relay error {expected:?}, got {other:?}"),
        Ok(_) => panic!("expected relay error {expected:?}, got success"),
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn deterministic_bytes(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| ((i.wrapping_mul(31) + (i / 7)) % 251) as u8)
        .collect()
}

#[allow(clippy::result_large_err)]
fn reject_upgrade(status: u16) -> impl Fn(&Request, Response) -> Result<Response, ErrorResponse> {
    move |_request: &Request, _response: Response| {
        let response: ErrorResponse = tokio_tungstenite::tungstenite::http::Response::builder()
            .status(status)
            .body(Some("rejected".to_string()))
            .unwrap();
        Err(response)
    }
}

#[tokio::test]
async fn relay_round_trips_small_body() {
    let (config, acceptor) = tls_pair();
    let (origin, server) =
        spawn_response_relay(acceptor, "200 OK", b"{\"status\":\"ok\"}", None, 4096).await;

    let response = request_once_relay(
        config,
        &origin,
        "inst-small",
        RELAY_TOKEN,
        "POST",
        "/app/observer/register",
        &[("Content-Type".to_string(), "application/json".to_string())],
        b"{\"device\":\"win\"}",
    )
    .await
    .expect("relay request should round-trip");

    assert_eq!(response.status, 200);
    assert_eq!(response.body_text(), "{\"status\":\"ok\"}");
    let received = server.await.unwrap();
    let received_text = String::from_utf8_lossy(&received);
    assert!(received_text.starts_with("POST /app/observer/register HTTP/1.1\r\n"));
    assert!(received_text.ends_with("{\"device\":\"win\"}"));
}

#[tokio::test]
async fn relay_streams_multi_mib_body_under_flow_control() {
    let (config, acceptor) = tls_pair();
    let (origin, server) = spawn_flow_relay(acceptor).await;
    let big_body = vec![0x7Cu8; INITIAL_WINDOW * 2 + INITIAL_WINDOW / 2 + 123];

    let response = request_once_relay(
        config,
        &origin,
        "inst-flow",
        RELAY_TOKEN,
        "POST",
        "/app/observer/ingest",
        &[(
            "Content-Type".to_string(),
            "application/octet-stream".to_string(),
        )],
        &big_body,
    )
    .await
    .expect("relay must preserve the windowed upload over partial byte reads");

    assert_eq!(response.status, 200);
    assert_eq!(response.body_text(), "{\"status\":\"accepted\"}");
    let received = server.await.unwrap();
    assert!(
        received.len() > INITIAL_WINDOW * 2,
        "server should receive the whole multi-MiB request, got {} bytes",
        received.len()
    );
    assert!(received.ends_with(&big_body));
}

#[tokio::test]
async fn relay_reassembles_large_response_across_partial_frames() {
    let (config, acceptor) = tls_pair();
    let expected_body = deterministic_bytes(LARGE_RESPONSE_BYTES);
    let (origin, server, sent_frame_sizes) =
        spawn_large_response_relay(acceptor, expected_body.clone()).await;

    let response = request_once_relay(
        config,
        &origin,
        "inst-large-response",
        RELAY_TOKEN,
        "GET",
        "/app/observer/large-response",
        &[],
        b"",
    )
    .await
    .expect("large relay response should round-trip");

    assert_eq!(response.status, 200);
    assert_eq!(response.body, expected_body);
    assert!(
        sent_frame_sizes
            .lock()
            .unwrap()
            .iter()
            .any(|size| *size > CARRIER_READ_BUF_BYTES),
        "test must deliver at least one inbound WS frame larger than the carrier read buffer"
    );
    let received = server.await.unwrap();
    let received_text = String::from_utf8_lossy(&received);
    assert!(received_text.starts_with("GET /app/observer/large-response HTTP/1.1\r\n"));
}

#[tokio::test]
async fn relay_is_blind_to_inner_http_and_tokens() {
    let (config, acceptor) = tls_pair();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let instance_id = "instance-blind";
    let token = "blind-token-secret";
    let (origin, server) = spawn_response_relay(
        acceptor,
        "200 OK",
        b"{\"status\":\"ok\"}",
        Some(captured.clone()),
        4096,
    )
    .await;

    let response = request_once_relay(
        config,
        &origin,
        instance_id,
        token,
        "POST",
        "/app/observer/register",
        &[(
            observer_pl::OBSERVER_HANDLE_HEADER.to_string(),
            "observer-key".to_string(),
        )],
        b"{\"device\":\"win\"}",
    )
    .await
    .expect("relay request should round-trip");
    assert_eq!(response.status, 200);
    let _ = server.await.unwrap();

    let captured = captured.lock().unwrap().clone();
    assert!(captured.len() > 6, "expected a captured TLS record");
    assert_eq!(
        captured[0], 0x16,
        "first WS binary payload is a TLS handshake record"
    );
    assert_eq!(
        captured[5], 0x01,
        "first TLS handshake message is ClientHello"
    );
    assert!(!contains_bytes(&captured, b"POST"));
    assert!(!contains_bytes(&captured, token.as_bytes()));
    assert!(!contains_bytes(
        &captured,
        observer_pl::OBSERVER_HANDLE_HEADER.as_bytes()
    ));
    assert!(!contains_bytes(&captured, instance_id.as_bytes()));
}

#[tokio::test]
async fn relay_wrong_inner_pin_stays_tls_error() {
    let (cert, key) = self_signed();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let _server = tokio::spawn(async move {
        let stream = accept_relay_stream(listener, None, 4096).await;
        let _ = acceptor.accept(stream).await;
    });

    let wrong_pin = vec![0xFFu8; 16];
    let result = request_once_relay(
        client_config(&wrong_pin),
        &origin,
        "inst-wrong-pin",
        RELAY_TOKEN,
        "GET",
        "/healthz",
        &[],
        b"",
    )
    .await;

    match result {
        Err(TransportError::Tls(_)) => {}
        Err(TransportError::Relay(err)) => panic!("wrong pin must not map to relay error: {err:?}"),
        Err(other) => panic!("wrong pin should surface as TLS, got {other:?}"),
        Ok(response) => panic!("wrong pin unexpectedly succeeded: {response:?}"),
    }
}

#[tokio::test]
async fn relay_upgrade_reject_maps_http_statuses() {
    for (status, expected) in [
        (503, RelayError::HomeOffline),
        (402, RelayError::Unpaid),
        (401, RelayError::Unauthorized),
        (404, RelayError::UnknownInstance),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "ws://{}/session/dial?instance=inst",
            listener.local_addr().unwrap()
        );
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let _ = accept_hdr_async(tcp, reject_upgrade(status)).await;
        });

        let result = dial_relay_ws(&url, RELAY_TOKEN, unused_outer_config()).await;
        assert_relay_error(result, expected);
        server.await.unwrap();
    }
}

#[tokio::test]
async fn relay_post_upgrade_close_maps_codes() {
    for (code, expected) in [
        (4401, RelayError::Unauthorized),
        (4402, RelayError::Unpaid),
        (1009, RelayError::Overflow),
        (1012, RelayError::Abnormal),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "ws://{}/session/dial?instance=inst",
            listener.local_addr().unwrap()
        );
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(tcp).await.unwrap();
            ws.send(Message::Close(Some(CloseFrame {
                code: CloseCode::from(code),
                reason: "".into(),
            })))
            .await
            .unwrap();
        });

        let ws = dial_relay_ws(&url, RELAY_TOKEN, unused_outer_config())
            .await
            .unwrap();
        let result = request_once_over_ws(
            ws,
            client_config(&[0xAA; 16]),
            Duration::from_secs(5),
            "GET",
            "/healthz",
            &[],
            b"",
        )
        .await;
        assert_relay_error(result, expected);
        server.await.unwrap();
    }
}

#[tokio::test]
async fn relay_abnormal_drop_maps_to_abnormal() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/session/dial?instance=inst",
        listener.local_addr().unwrap()
    );
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let ws = accept_async(tcp).await.unwrap();
        drop(ws);
    });

    let ws = dial_relay_ws(&url, RELAY_TOKEN, unused_outer_config())
        .await
        .unwrap();
    let result = request_once_over_ws(
        ws,
        client_config(&[0xAA; 16]),
        Duration::from_secs(5),
        "GET",
        "/healthz",
        &[],
        b"",
    )
    .await;

    assert_relay_error(result, RelayError::Abnormal);
    server.await.unwrap();
}

#[tokio::test]
async fn relay_inner_handshake_stall_maps_to_stalled() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/session/dial?instance=inst",
        listener.local_addr().unwrap()
    );
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(tcp).await.unwrap();
        let _ = ws.next().await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    });

    let ws = dial_relay_ws(&url, RELAY_TOKEN, unused_outer_config())
        .await
        .unwrap();
    let result = request_once_over_ws(
        ws,
        client_config(&[0xAA; 16]),
        Duration::from_millis(200),
        "GET",
        "/healthz",
        &[],
        b"",
    )
    .await;

    assert_relay_error(result, RelayError::Stalled);
    server.abort();
}

#[tokio::test]
async fn relay_inner_app_503_returns_http_response_not_home_offline() {
    let (config, acceptor) = tls_pair();
    let (origin, server) = spawn_response_relay(
        acceptor,
        "503 Service Unavailable",
        b"{\"error\":\"busy\"}",
        None,
        4096,
    )
    .await;

    let response: HttpResponse = request_once_relay(
        config,
        &origin,
        "inst-inner-503",
        RELAY_TOKEN,
        "GET",
        "/app/observer/ingest",
        &[],
        b"",
    )
    .await
    .expect("inner app 503 stays an HTTP response inside the tunnel");

    assert_eq!(response.status, 503);
    assert!(!response.is_success());
    assert_eq!(response.body_text(), "{\"error\":\"busy\"}");
    let _ = server.await.unwrap();
}
