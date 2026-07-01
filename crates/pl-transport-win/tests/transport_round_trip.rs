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

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use observer_model::{
    AppPhase, HealthDump, PairingPhase, PairingState, SyncSnapshot, UploadStatus,
    LAST_ERROR_REASON_MAX_LEN,
};
use observer_pl::frame::{Frame, FrameDecoder, FLAG_CLOSE, FLAG_DATA, FLAG_RESET, FLAG_WINDOW};
use observer_pl::mux::{HttpHead, StreamEnd, StreamItem, INITIAL_WINDOW};
use pl_transport_win::client::ObserverClient;
use pl_transport_win::connection::request_once;
use pl_transport_win::credential::{Credential, EndpointAddr, PairedState};
use pl_transport_win::heartbeat::run_heartbeat;
use pl_transport_win::tls::pairing_config;
use pl_transport_win::{journal_bridge, TransportError};
use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::ServerConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
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

fn observer_credential(pin: Vec<u8>, port: u16) -> Credential {
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

fn observer_relay_credential(
    pin: Vec<u8>,
    port: u16,
    relay_origin: String,
    token: &str,
) -> Credential {
    let mut credential = observer_credential(pin, port);
    credential.relay_origin = Some(relay_origin);
    credential.device_token = Some(token.to_string());
    credential.device_token_expires_at = Some(200);
    credential
}

fn test_health(app_state: AppPhase) -> HealthDump {
    HealthDump {
        app_state,
        sources: vec![],
        frame_rate: None,
        segment_dir: None,
        segment_seconds_remaining: None,
        engine_ready: true,
        version: "0.3.1".into(),
        sync: SyncSnapshot::default(),
        screen_encoder: None,
        exclusions: None,
        pause: None,
        views: Default::default(),
    }
}

fn request_body(request: &[u8]) -> serde_json::Value {
    let request = String::from_utf8_lossy(request);
    let (_, body) = request.split_once("\r\n\r\n").unwrap();
    serde_json::from_str(body).unwrap()
}

async fn read_framed_request(
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

/// Accept one TLS connection, read the framed HTTP request, and frame back a
/// fixed `{"status":"ok"}` response on the same stream. Returns the request body
/// it received so the test can assert the wire bytes.
async fn serve_one_response(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    status: &str,
    body: &'static [u8],
) -> Vec<u8> {
    serve_one_response_with_content_length(listener, acceptor, status, body, body.len()).await
}

async fn serve_one_response_with_content_length(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    status: &str,
    body: &'static [u8],
    content_length: usize,
) -> Vec<u8> {
    let (tcp, _) = listener.accept().await.unwrap();
    let mut tls = acceptor.accept(tcp).await.unwrap();

    let (stream_id, request) = read_framed_request(&mut tls).await;

    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        content_length,
        String::from_utf8_lossy(body)
    );
    let frame = Frame::new(stream_id, FLAG_DATA | FLAG_CLOSE, response.into_bytes());
    tls.write_all(&frame.encode().unwrap()).await.unwrap();
    tls.flush().await.unwrap();
    let _ = tls.shutdown().await;
    request
}

async fn serve_one(listener: TcpListener, acceptor: TlsAcceptor) -> Vec<u8> {
    serve_one_response(listener, acceptor, "200 OK", b"{\"status\":\"ok\"}").await
}

async fn serve_drop_then_one(listener: TcpListener, acceptor: TlsAcceptor) -> Vec<u8> {
    let (tcp, _) = listener.accept().await.unwrap();
    drop(tcp);
    serve_one(listener, acceptor).await
}

#[derive(Clone, Copy)]
enum SseMode {
    Close,
    ResetAfterFirst,
    EofBeforeHead,
    ChunkedClose,
}

async fn write_response_frame(
    tls: &mut tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    stream_id: u32,
    flags: u8,
    payload: &[u8],
) {
    let frame = Frame::new(stream_id, flags, payload.to_vec());
    tls.write_all(&frame.encode().unwrap()).await.unwrap();
    tls.flush().await.unwrap();
}

async fn serve_sse_stream(listener: TcpListener, acceptor: TlsAcceptor, mode: SseMode) -> Vec<u8> {
    let (tcp, _) = listener.accept().await.unwrap();
    let mut tls = acceptor.accept(tcp).await.unwrap();
    let (stream_id, request) = read_framed_request(&mut tls).await;

    if matches!(mode, SseMode::EofBeforeHead) {
        return request;
    }

    match mode {
        SseMode::Close | SseMode::ResetAfterFirst => {
            write_response_frame(
                &mut tls,
                stream_id,
                FLAG_DATA,
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n",
            )
            .await;
            write_response_frame(&mut tls, stream_id, FLAG_DATA, b"data: 1\n\n").await;
            if matches!(mode, SseMode::ResetAfterFirst) {
                write_response_frame(&mut tls, stream_id, FLAG_RESET, b"").await;
                return request;
            }
            write_response_frame(&mut tls, stream_id, FLAG_DATA, b"data: 2\n\n").await;
            write_response_frame(&mut tls, stream_id, FLAG_CLOSE, b"").await;
        }
        SseMode::ChunkedClose => {
            write_response_frame(
                &mut tls,
                stream_id,
                FLAG_DATA,
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n",
            )
            .await;
            write_response_frame(&mut tls, stream_id, FLAG_DATA, b"9\r\ndata: 1\n\n\r\n").await;
            write_response_frame(&mut tls, stream_id, FLAG_DATA, b"9").await;
            write_response_frame(
                &mut tls,
                stream_id,
                FLAG_DATA,
                b"\r\ndata: 2\n\n\r\n0\r\n\r\n",
            )
            .await;
            write_response_frame(&mut tls, stream_id, FLAG_CLOSE, b"").await;
        }
        SseMode::EofBeforeHead => unreachable!("handled before writing a head"),
    }

    let _ = tls.shutdown().await;
    request
}

async fn collect_stream_items(rx: mpsc::Receiver<StreamItem>) -> Vec<StreamItem> {
    let mut rx = rx;
    let mut items = Vec::new();
    while let Some(item) = rx.recv().await {
        items.push(item);
    }
    items
}

fn assert_sse_head(item: &StreamItem) {
    match item {
        StreamItem::Head(HttpHead { status, headers }) => {
            assert_eq!(*status, 200);
            assert!(headers.iter().any(|(name, value)| {
                name == "content-type" && value.eq_ignore_ascii_case("text/event-stream")
            }));
        }
        other => panic!("expected SSE head, got {other:?}"),
    }
}

fn body_items(items: &[StreamItem]) -> Vec<Vec<u8>> {
    items
        .iter()
        .filter_map(|item| match item {
            StreamItem::Body(body) => Some(body.clone()),
            _ => None,
        })
        .collect()
}

async fn spawn_fixed_response(
    status: &'static str,
    body: &'static [u8],
) -> (ObserverClient, JoinHandle<Vec<u8>>) {
    let (cert, key) = self_signed();
    let pin = observer_pl::ca::sha256(cert.as_ref())[..16].to_vec();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_one_response(listener, acceptor, status, body));
    let client = ObserverClient::new(observer_credential(pin, port))
        .unwrap()
        .with_observer_key(Some("obs-handle".into()));
    (client, server)
}

async fn spawn_sse(mode: SseMode) -> (ObserverClient, JoinHandle<Vec<u8>>) {
    let (cert, key) = self_signed();
    let pin = observer_pl::ca::sha256(cert.as_ref())[..16].to_vec();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_sse_stream(listener, acceptor, mode));
    let client = ObserverClient::new(observer_credential(pin, port))
        .unwrap()
        .with_observer_key(Some("obs-handle".into()));
    (client, server)
}

static NEXT_TEMP_PATH: AtomicUsize = AtomicUsize::new(0);

fn temp_state_path(name: &str) -> PathBuf {
    let n = NEXT_TEMP_PATH.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "journal-bridge-test-{}-{name}-{n}.json",
        std::process::id()
    ))
}

fn paired_state(credential: Credential) -> PairedState {
    PairedState {
        credential: Some(credential),
        observer_key: Some("obs-handle".to_string()),
        observer_name: None,
    }
}

fn capability_from(handle: &journal_bridge::JournalBridgeHandle) -> String {
    handle
        .bootstrap_url()
        .split_once("cap=")
        .map(|(_, cap)| cap.to_string())
        .unwrap()
}

async fn start_bridge_with_response(
    status: &'static str,
    body: &'static [u8],
) -> (journal_bridge::JournalBridgeHandle, JoinHandle<Vec<u8>>) {
    let (cert, key) = self_signed();
    let pin = observer_pl::ca::sha256(cert.as_ref())[..16].to_vec();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_one_response(listener, acceptor, status, body));
    let paired = paired_state(observer_credential(pin, upstream_port));
    let handle = journal_bridge::start(&paired, temp_state_path("response"))
        .await
        .unwrap();
    (handle, server)
}

async fn start_bridge_with_response_content_length(
    status: &'static str,
    body: &'static [u8],
    content_length: usize,
) -> (journal_bridge::JournalBridgeHandle, JoinHandle<Vec<u8>>) {
    let (cert, key) = self_signed();
    let pin = observer_pl::ca::sha256(cert.as_ref())[..16].to_vec();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_one_response_with_content_length(
        listener,
        acceptor,
        status,
        body,
        content_length,
    ));
    let paired = paired_state(observer_credential(pin, upstream_port));
    let handle = journal_bridge::start(&paired, temp_state_path("response-length"))
        .await
        .unwrap();
    (handle, server)
}

async fn start_bridge_with_sse(
    mode: SseMode,
) -> (journal_bridge::JournalBridgeHandle, JoinHandle<Vec<u8>>) {
    let (cert, key) = self_signed();
    let pin = observer_pl::ca::sha256(cert.as_ref())[..16].to_vec();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_sse_stream(listener, acceptor, mode));
    let paired = paired_state(observer_credential(pin, upstream_port));
    let handle = journal_bridge::start(&paired, temp_state_path("sse"))
        .await
        .unwrap();
    (handle, server)
}

async fn start_bridge_with_counting_upstream() -> (
    journal_bridge::JournalBridgeHandle,
    Arc<AtomicUsize>,
    JoinHandle<()>,
) {
    let (cert, key) = self_signed();
    let pin = observer_pl::ca::sha256(cert.as_ref())[..16].to_vec();
    let _acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_port = listener.local_addr().unwrap().port();
    let accepts = Arc::new(AtomicUsize::new(0));
    let task = tokio::spawn({
        let accepts = accepts.clone();
        async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    break;
                };
                accepts.fetch_add(1, Ordering::SeqCst);
                drop(tcp);
            }
        }
    });
    let paired = paired_state(observer_credential(pin, upstream_port));
    let handle = journal_bridge::start(&paired, temp_state_path("counting"))
        .await
        .unwrap();
    (handle, accepts, task)
}

async fn raw_bridge_request(
    port: u16,
    method: &str,
    target: &str,
    host: Option<String>,
    cookie: Option<String>,
    extra_headers: &[(&str, &str)],
    body: &[u8],
) -> Vec<u8> {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let mut request = format!("{method} {target} HTTP/1.1\r\n");
    if let Some(host) = host {
        request.push_str("Host: ");
        request.push_str(&host);
        request.push_str("\r\n");
    }
    if let Some(cookie) = cookie {
        request.push_str("Cookie: ");
        request.push_str(&cookie);
        request.push_str("\r\n");
    }
    for (name, value) in extra_headers {
        request.push_str(name);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    if !body.is_empty() {
        request.push_str("Content-Length: ");
        request.push_str(&body.len().to_string());
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    if !body.is_empty() {
        stream.write_all(body).await.unwrap();
    }
    stream.flush().await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    response
}

fn loopback_host(port: u16) -> String {
    format!("127.0.0.1:{port}")
}

fn cap_cookie(cap: &str) -> String {
    format!("{}={cap}", observer_pl::bridge::CAP_COOKIE_NAME)
}

fn response_text(response: &[u8]) -> String {
    String::from_utf8_lossy(response).into_owned()
}

fn response_status(response: &[u8]) -> u16 {
    response_text(response)
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap()
}

fn response_body(response: &[u8]) -> String {
    let text = response_text(response);
    text.split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap()
}

fn response_head(response: &[u8]) -> String {
    let text = response_text(response);
    text.split_once("\r\n\r\n")
        .map(|(head, _)| head.to_ascii_lowercase())
        .unwrap()
}

#[derive(Clone)]
struct CapturingSubscriber {
    lines: Arc<Mutex<Vec<String>>>,
}

impl tracing::Subscriber for CapturingSubscriber {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.target() == "journal_bridge"
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        if !self.enabled(event.metadata()) {
            return;
        }
        let mut visitor = LogVisitor::default();
        event.record(&mut visitor);
        self.lines.lock().unwrap().push(visitor.line);
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

#[derive(Default)]
struct LogVisitor {
    line: String,
}

impl LogVisitor {
    fn field(&mut self, name: &str, value: impl std::fmt::Display) {
        if !self.line.is_empty() {
            self.line.push(' ');
        }
        self.line.push_str(name);
        self.line.push('=');
        self.line.push_str(&value.to_string());
    }
}

impl tracing::field::Visit for LogVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.field(field.name(), format!("{value:?}"));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.field(field.name(), value);
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.field(field.name(), value);
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.field(field.name(), value);
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.field(field.name(), value);
    }
}

async fn spawn_counting_relay() -> (String, Arc<AtomicUsize>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let accepts = Arc::new(AtomicUsize::new(0));
    let task = tokio::spawn({
        let accepts = accepts.clone();
        async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    break;
                };
                accepts.fetch_add(1, Ordering::SeqCst);
                drop(tcp);
            }
        }
    });
    (origin, accepts, task)
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
        "/app/network/pair?token=abc123",
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
    assert!(received_text.starts_with("POST /app/network/pair?token=abc123 HTTP/1.1\r\n"));
    assert!(received_text.contains("host: spl.local\r\n"));
    assert!(received_text.contains("Content-Type: application/json\r\n"));
    assert!(received_text.ends_with("{\"csr\":\"PEM\",\"device_label\":\"win\"}"));
}

#[tokio::test]
async fn buffered_proxy_request_forwards_auth_accept_and_body() {
    let (client, server) = spawn_fixed_response("200 OK", b"<html>ok</html>").await;

    let response = client
        .request(
            "GET",
            "/journal",
            &[("accept".to_string(), "text/html".to_string())],
            b"",
        )
        .await
        .unwrap();

    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"<html>ok</html>");
    let request = server.await.unwrap();
    let request = String::from_utf8_lossy(&request);
    assert!(request.starts_with("GET /journal HTTP/1.1\r\n"));
    assert!(request.contains("X-Solstone-Observer: obs-handle\r\n"));
    assert!(request.contains("Authorization: Bearer obs-handle\r\n"));
    assert!(request.contains("X-Solstone-Protocol-Version: 2\r\n"));
    assert!(request.contains("accept: text/html\r\n"));
}

#[tokio::test]
async fn buffered_proxy_request_preserves_401_as_response() {
    let (client, server) = spawn_fixed_response("401 Unauthorized", b"{\"error\":\"auth\"}").await;

    let response = client.request("GET", "/journal", &[], b"").await.unwrap();

    assert_eq!(response.status, 401);
    assert_eq!(response.body, b"{\"error\":\"auth\"}");
    let _ = server.await.unwrap();
}

#[tokio::test]
async fn buffered_proxy_request_strips_caller_auth_headers() {
    let (client, server) = spawn_fixed_response("200 OK", b"{\"status\":\"ok\"}").await;

    client
        .request(
            "GET",
            "/journal",
            &[
                ("Authorization".to_string(), "Bearer attacker".to_string()),
                ("X-Solstone-Observer".to_string(), "attacker".to_string()),
                ("X-Solstone-Protocol-Version".to_string(), "999".to_string()),
                ("accept".to_string(), "text/html".to_string()),
            ],
            b"",
        )
        .await
        .unwrap();

    let request = server.await.unwrap();
    let request = String::from_utf8_lossy(&request);
    assert!(request.contains("X-Solstone-Observer: obs-handle\r\n"));
    assert!(request.contains("Authorization: Bearer obs-handle\r\n"));
    assert!(request.contains("X-Solstone-Protocol-Version: 2\r\n"));
    assert!(!request.contains("attacker"));
    assert!(!request.contains("999"));
}

#[tokio::test]
async fn sse_streams_head_first_body_items_then_close() {
    let (client, server) = spawn_sse(SseMode::Close).await;
    let (tx, rx) = mpsc::channel(16);

    let result = client
        .request_stream("GET", "/sse/events", &[], b"", &tx)
        .await;
    drop(tx);
    let items = collect_stream_items(rx).await;

    result.unwrap();
    let _ = server.await.unwrap();
    assert_sse_head(&items[0]);
    assert_eq!(
        body_items(&items),
        vec![b"data: 1\n\n".to_vec(), b"data: 2\n\n".to_vec()]
    );
    assert_eq!(items.last(), Some(&StreamItem::End(StreamEnd::Close)));
}

#[tokio::test]
async fn sse_chunked_body_is_decoded_incrementally() {
    let (client, server) = spawn_sse(SseMode::ChunkedClose).await;
    let (tx, rx) = mpsc::channel(16);

    let result = client
        .request_stream("GET", "/sse/events", &[], b"", &tx)
        .await;
    drop(tx);
    let items = collect_stream_items(rx).await;

    result.unwrap();
    let _ = server.await.unwrap();
    assert!(matches!(items.first(), Some(StreamItem::Head(_))));
    assert_eq!(
        body_items(&items),
        vec![b"data: 1\n\n".to_vec(), b"data: 2\n\n".to_vec()]
    );
    assert_eq!(items.last(), Some(&StreamItem::End(StreamEnd::Close)));
}

#[tokio::test]
async fn sse_reset_after_head_returns_ok_with_reset_end() {
    let (client, server) = spawn_sse(SseMode::ResetAfterFirst).await;
    let (tx, rx) = mpsc::channel(16);

    let result = client
        .request_stream("GET", "/sse/events", &[], b"", &tx)
        .await;
    drop(tx);
    let items = collect_stream_items(rx).await;

    result.unwrap();
    let _ = server.await.unwrap();
    assert_sse_head(&items[0]);
    assert_eq!(body_items(&items), vec![b"data: 1\n\n".to_vec()]);
    assert_eq!(items.last(), Some(&StreamItem::End(StreamEnd::Reset)));
}

#[tokio::test]
async fn sse_eof_before_head_returns_error_without_head_item() {
    let (client, server) = spawn_sse(SseMode::EofBeforeHead).await;
    let (tx, rx) = mpsc::channel(16);

    let result = client
        .request_stream("GET", "/sse/events", &[], b"", &tx)
        .await;
    drop(tx);
    let items = collect_stream_items(rx).await;

    assert!(result.is_err());
    let _ = server.await.unwrap();
    assert!(!items.iter().any(|item| matches!(item, StreamItem::Head(_))));
}

#[tokio::test]
async fn journal_bridge_bootstrap_sets_cookie_and_rejects_wrong_cap() {
    let (handle, _accepts, upstream) = start_bridge_with_counting_upstream().await;
    let port = handle.port();
    let cap = capability_from(&handle);

    let ok = raw_bridge_request(
        port,
        "GET",
        &format!("{}?cap={cap}", observer_pl::bridge::BOOTSTRAP_ROUTE),
        Some(loopback_host(port)),
        None,
        &[],
        b"",
    )
    .await;
    let ok_text = response_text(&ok);
    assert_eq!(response_status(&ok), 302);
    assert!(ok_text.contains(&format!(
        "Set-Cookie: {}={cap}; Path=/; HttpOnly; SameSite=Strict",
        observer_pl::bridge::CAP_COOKIE_NAME
    )));
    assert!(ok_text.contains("Location: /\r\n"));

    let bad = raw_bridge_request(
        port,
        "GET",
        &format!("{}?cap=wrong", observer_pl::bridge::BOOTSTRAP_ROUTE),
        Some(loopback_host(port)),
        None,
        &[],
        b"",
    )
    .await;
    assert_eq!(response_status(&bad), 403);
    assert!(!response_text(&bad).contains("Set-Cookie:"));

    let wrong_method = raw_bridge_request(
        port,
        "POST",
        &format!("{}?cap={cap}", observer_pl::bridge::BOOTSTRAP_ROUTE),
        Some(loopback_host(port)),
        None,
        &[],
        b"",
    )
    .await;
    assert_eq!(response_status(&wrong_method), 405);
    assert!(!response_text(&wrong_method).contains("Set-Cookie:"));

    let caller_auth = raw_bridge_request(
        port,
        "GET",
        &format!("{}?cap={cap}", observer_pl::bridge::BOOTSTRAP_ROUTE),
        Some(loopback_host(port)),
        None,
        &[("Authorization", "Bearer caller")],
        b"",
    )
    .await;
    assert_eq!(response_status(&caller_auth), 403);
    assert!(!response_text(&caller_auth).contains("Set-Cookie:"));

    handle.shutdown_and_wait().await;
    upstream.abort();
}

#[tokio::test]
async fn journal_bridge_authority_rejects_before_upstream() {
    let (handle, accepts, upstream) = start_bridge_with_counting_upstream().await;
    let port = handle.port();
    let cap = capability_from(&handle);

    let cases = [
        (
            "GET",
            "/journal",
            Some(loopback_host(port)),
            None,
            vec![],
            403,
        ),
        (
            "GET",
            "/journal",
            Some(loopback_host(port)),
            Some(cap_cookie("wrong")),
            vec![],
            403,
        ),
        (
            "GET",
            "/journal",
            Some(loopback_host(port + 1)),
            Some(cap_cookie(&cap)),
            vec![],
            403,
        ),
        (
            "OPTIONS",
            "/journal",
            Some(loopback_host(port)),
            Some(cap_cookie(&cap)),
            vec![],
            405,
        ),
        (
            "GET",
            "/journal",
            Some(loopback_host(port)),
            Some(cap_cookie(&cap)),
            vec![("Authorization", "Bearer x")],
            403,
        ),
    ];

    for (method, target, host, cookie, headers, expected) in cases {
        let response = raw_bridge_request(port, method, target, host, cookie, &headers, b"").await;
        assert_eq!(response_status(&response), expected);
    }

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(accepts.load(Ordering::SeqCst), 0);
    handle.shutdown_and_wait().await;
    upstream.abort();
}

#[tokio::test]
async fn journal_bridge_buffered_pass_through_injects_auth_and_strips_local_headers() {
    let (handle, upstream) = start_bridge_with_response("200 OK", b"bridge ok").await;
    let port = handle.port();
    let cap = capability_from(&handle);

    let response = raw_bridge_request(
        port,
        "GET",
        "/journal",
        Some(loopback_host(port)),
        Some(cap_cookie(&cap)),
        &[("Accept", "text/html")],
        b"",
    )
    .await;

    assert_eq!(response_status(&response), 200);
    assert_eq!(response_body(&response), "bridge ok");
    let head = response_head(&response);
    assert!(head.contains("content-length: 9"));
    assert!(head.contains("connection: close"));

    let request = upstream.await.unwrap();
    let request = String::from_utf8_lossy(&request);
    assert!(request.contains("X-Solstone-Observer: obs-handle\r\n"));
    assert!(request.contains("Authorization: Bearer obs-handle\r\n"));
    assert!(request.contains("X-Solstone-Protocol-Version: 2\r\n"));
    assert!(request.contains("accept: text/html\r\n"));
    let lower = request.to_ascii_lowercase();
    assert!(!lower.contains(observer_pl::bridge::CAP_COOKIE_NAME));
    assert!(!lower.contains("cookie:"));
    assert!(!lower.contains("host: 127.0.0.1"));

    handle.shutdown_and_wait().await;
}

#[tokio::test]
async fn journal_bridge_head_preserves_upstream_content_length_without_body() {
    let (handle, upstream) = start_bridge_with_response_content_length("200 OK", b"", 42).await;
    let port = handle.port();
    let cap = capability_from(&handle);

    let response = raw_bridge_request(
        port,
        "HEAD",
        "/journal",
        Some(loopback_host(port)),
        Some(cap_cookie(&cap)),
        &[],
        b"",
    )
    .await;

    assert_eq!(response_status(&response), 200);
    assert_eq!(response_body(&response), "");
    let head = response_head(&response);
    assert!(head.contains("content-length: 42"));
    assert!(head.contains("connection: close"));

    let request = upstream.await.unwrap();
    let request = String::from_utf8_lossy(&request);
    assert!(request.starts_with("HEAD /journal HTTP/1.1\r\n"));
    handle.shutdown_and_wait().await;
}

#[tokio::test]
async fn journal_bridge_forwards_journal_401_without_masking() {
    let (handle, upstream) = start_bridge_with_response("401 Unauthorized", b"auth").await;
    let port = handle.port();
    let cap = capability_from(&handle);

    let response = raw_bridge_request(
        port,
        "GET",
        "/journal",
        Some(loopback_host(port)),
        Some(cap_cookie(&cap)),
        &[],
        b"",
    )
    .await;

    assert_eq!(response_status(&response), 401);
    assert_eq!(response_body(&response), "auth");
    let _ = upstream.await.unwrap();
    handle.shutdown_and_wait().await;
}

#[tokio::test]
async fn journal_bridge_sse_streams_without_local_framing_headers() {
    let (handle, upstream) = start_bridge_with_sse(SseMode::Close).await;
    let port = handle.port();
    let cap = capability_from(&handle);

    let response = raw_bridge_request(
        port,
        "GET",
        "/sse/events",
        Some(loopback_host(port)),
        Some(cap_cookie(&cap)),
        &[],
        b"",
    )
    .await;

    assert_eq!(response_status(&response), 200);
    let head = response_head(&response);
    assert!(head.contains("content-type: text/event-stream"));
    assert!(head.contains("connection: close"));
    assert!(!head.contains("content-length"));
    assert!(!head.contains("transfer-encoding"));
    let body = response_body(&response);
    assert!(body.contains("data: 1\n\n"));
    assert!(body.contains("data: 2\n\n"));

    let _ = upstream.await.unwrap();
    handle.shutdown_and_wait().await;
}

#[tokio::test]
async fn journal_bridge_sse_fail_before_head_returns_502() {
    let (handle, upstream) = start_bridge_with_sse(SseMode::EofBeforeHead).await;
    let port = handle.port();
    let cap = capability_from(&handle);

    let response = raw_bridge_request(
        port,
        "GET",
        "/sse/events",
        Some(loopback_host(port)),
        Some(cap_cookie(&cap)),
        &[],
        b"",
    )
    .await;

    assert_eq!(response_status(&response), 502);
    assert!(!response_text(&response).starts_with("HTTP/1.1 200"));
    let _ = upstream.await.unwrap();
    handle.shutdown_and_wait().await;
}

#[tokio::test]
async fn journal_bridge_binds_loopback_and_serves_on_reported_port() {
    let (handle, _accepts, upstream) = start_bridge_with_counting_upstream().await;
    let port = handle.port();
    let cap = capability_from(&handle);

    let response = raw_bridge_request(
        port,
        "GET",
        &format!("{}?cap={cap}", observer_pl::bridge::BOOTSTRAP_ROUTE),
        Some(loopback_host(port)),
        None,
        &[],
        b"",
    )
    .await;

    assert_eq!(response_status(&response), 302);
    handle.shutdown_and_wait().await;
    upstream.abort();
}

#[tokio::test]
async fn journal_bridge_shutdown_frees_port() {
    let (handle, _accepts, upstream) = start_bridge_with_counting_upstream().await;
    let port = handle.port();

    handle.shutdown_and_wait().await;
    assert!(tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .is_err());
    upstream.abort();
}

#[tokio::test]
async fn journal_bridge_logs_redacted_failure_categories_only() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let closed_port = listener.local_addr().unwrap().port();
    drop(listener);
    let paired = paired_state(observer_credential(vec![0; 16], closed_port));
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let subscriber = CapturingSubscriber {
        lines: lines.clone(),
    };
    tracing::dispatcher::set_global_default(tracing::Dispatch::new(subscriber))
        .expect("install journal bridge log capture subscriber");
    let handle = journal_bridge::start(&paired, temp_state_path("redaction"))
        .await
        .unwrap();
    let port = handle.port();
    let cap = capability_from(&handle);

    let _ = raw_bridge_request(
        port,
        "GET",
        "/secret/path?token=owner-secret",
        Some(loopback_host(port)),
        Some(cap_cookie("wrong-capability")),
        &[],
        b"body-secret",
    )
    .await;
    let _ = raw_bridge_request(
        port,
        "GET",
        "/journal?query=owner-secret",
        Some(loopback_host(port)),
        Some(cap_cookie(&cap)),
        &[],
        b"",
    )
    .await;

    handle.shutdown_and_wait().await;
    let logs = lines.lock().unwrap().join("\n");
    assert!(logs.contains("category=local_capability_reject"));
    assert!(logs.contains("reason=bad_capability"));
    assert!(logs.contains("category=upstream_unreachable"));
    assert!(logs.contains("code=io"));
    assert!(!logs.contains(&cap));
    assert!(!logs.contains("wrong-capability"));
    assert!(!logs.contains(observer_pl::bridge::CAP_COOKIE_NAME));
    assert!(!logs.contains("/secret/path"));
    assert!(!logs.contains("owner-secret"));
    assert!(!logs.contains("body-secret"));
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

#[tokio::test]
async fn heartbeat_immediate_post_success_sets_heartbeat_ok_and_sends_safe_payload() {
    let (cert, key) = self_signed();
    let pin = observer_pl::ca::sha256(cert.as_ref())[..16].to_vec();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_one(listener, acceptor));

    let client = Arc::new(
        ObserverClient::new(observer_credential(pin, port))
            .unwrap()
            .with_observer_key(Some("observer-key".into())),
    );
    let health = Arc::new(Mutex::new(test_health(AppPhase::Idle)));
    let mut upload = UploadStatus {
        pending_segments: 7,
        last_successful_sync: Some(1_700_000_000_000),
        last_error: Some("SECRET https://x/y?token=abc C:\\Users\\me\\seg.mp4".into()),
        ..UploadStatus::default()
    };
    upload.record_failure("http_503");
    let sync = Arc::new(Mutex::new(SyncSnapshot {
        pairing: PairingState {
            phase: PairingPhase::Paired,
            journal_label: Some("Home".into()),
            observer_name: Some("fedora".into()),
            detail: None,
        },
        upload,
    }));
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(run_heartbeat(
        client,
        health,
        sync.clone(),
        "desktop".into(),
        "0.3.1".into(),
        shutdown_rx,
    ));

    let request = server.await.unwrap();
    let _ = shutdown_tx.send(());
    task.await.unwrap();

    assert!(sync.lock().unwrap().upload.heartbeat_ok);
    let body = request_body(&request);
    let object = body.as_object().unwrap();
    let expected = [
        "tract",
        "event",
        "paused",
        "name",
        "stream_type",
        "version",
        "uptime",
        "last_successful_sync",
        "pending_queue_depth",
        "recent_error_count",
        "last_error_reason",
    ];
    assert_eq!(object.len(), expected.len());
    for key in expected {
        assert!(object.contains_key(key), "missing key {key}");
    }
    assert_eq!(object["tract"], "observe");
    assert_eq!(object["event"], "status");
    assert_eq!(object["paused"], false);
    assert_eq!(object["name"], "fedora");
    assert_eq!(object["stream_type"], "desktop");
    assert_eq!(object["version"], "0.3.1");
    assert_eq!(object["last_successful_sync"], 1_700_000_000_000u64);
    assert_eq!(object["pending_queue_depth"], 7);
    assert_eq!(object["recent_error_count"], 1);
    assert_eq!(object["last_error_reason"], "http_503");
    assert!(
        object["last_error_reason"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= LAST_ERROR_REASON_MAX_LEN
    );

    let body_text = body.to_string();
    assert!(!object.contains_key("last_error"));
    assert!(!body_text.contains("SECRET"));
    assert!(!body_text.contains("token"));
    assert!(!body_text.contains("Users"));
    assert!(!body_text.contains("https://"));
}

#[tokio::test]
async fn heartbeat_immediate_post_rejection_is_non_fatal_and_sets_heartbeat_not_ok() {
    let (cert, key) = self_signed();
    let pin = observer_pl::ca::sha256(cert.as_ref())[..16].to_vec();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_one_response(
        listener,
        acceptor,
        "503 Service Unavailable",
        b"SECRET https://x/y?token=abc C:\\Users\\me\\seg.mp4",
    ));

    let client = Arc::new(
        ObserverClient::new(observer_credential(pin, port))
            .unwrap()
            .with_observer_key(Some("observer-key".into())),
    );
    let health = Arc::new(Mutex::new(test_health(AppPhase::Paused)));
    let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(run_heartbeat(
        client,
        health,
        sync.clone(),
        "desktop".into(),
        "0.3.1".into(),
        shutdown_rx,
    ));

    let request = server.await.unwrap();
    let _ = shutdown_tx.send(());
    task.await.unwrap();

    assert!(!sync.lock().unwrap().upload.heartbeat_ok);
    let body = request_body(&request);
    assert_eq!(body["tract"], "observe");
    assert_eq!(body["event"], "status");
    assert_eq!(body["paused"], true);
}

#[tokio::test]
async fn reachable_lan_success_never_dials_relay() {
    let (cert, key) = self_signed();
    let pin = observer_pl::ca::sha256(cert.as_ref())[..16].to_vec();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_one(listener, acceptor));
    let (origin, relay_accepts, relay_task) = spawn_counting_relay().await;
    let client = ObserverClient::new(observer_relay_credential(pin, port, origin, "old-token"))
        .unwrap()
        .with_observer_key(Some("observer-key".into()));

    client
        .heartbeat(&observer_pl::wire::HeartbeatEvent::status(false))
        .await
        .unwrap();

    let _ = server.await.unwrap();
    assert_eq!(relay_accepts.load(Ordering::SeqCst), 0);
    relay_task.abort();
}

#[tokio::test]
async fn reachable_lan_rejection_never_dials_relay() {
    let (cert, key) = self_signed();
    let pin = observer_pl::ca::sha256(cert.as_ref())[..16].to_vec();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_one_response(
        listener,
        acceptor,
        "503 Service Unavailable",
        b"{\"error\":\"busy\"}",
    ));
    let (origin, relay_accepts, relay_task) = spawn_counting_relay().await;
    let client = ObserverClient::new(observer_relay_credential(pin, port, origin, "old-token"))
        .unwrap()
        .with_observer_key(Some("observer-key".into()));

    let err = client
        .heartbeat(&observer_pl::wire::HeartbeatEvent::status(false))
        .await
        .unwrap_err();

    assert!(matches!(err, TransportError::Rejected { status: 503, .. }));
    let _ = server.await.unwrap();
    assert_eq!(relay_accepts.load(Ordering::SeqCst), 0);
    relay_task.abort();
}

#[tokio::test]
async fn lan_only_no_endpoint_still_returns_no_endpoint() {
    let mut credential = observer_credential(vec![0; 16], 7657);
    credential.endpoints.clear();
    let client = ObserverClient::new(credential)
        .unwrap()
        .with_observer_key(Some("observer-key".into()));

    let err = client
        .heartbeat(&observer_pl::wire::HeartbeatEvent::status(false))
        .await
        .unwrap_err();

    assert!(matches!(err, TransportError::NoEndpoint));
}

#[tokio::test]
async fn transient_lan_fault_then_success_absorbed_before_relay() {
    let (cert, key) = self_signed();
    let pin = observer_pl::ca::sha256(cert.as_ref())[..16].to_vec();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_drop_then_one(listener, acceptor));
    let (origin, relay_accepts, relay_task) = spawn_counting_relay().await;
    let client = ObserverClient::new(observer_relay_credential(pin, port, origin, "old-token"))
        .unwrap()
        .with_observer_key(Some("observer-key".into()));

    client
        .heartbeat(&observer_pl::wire::HeartbeatEvent::status(false))
        .await
        .unwrap();

    let _ = server.await.unwrap();
    assert_eq!(relay_accepts.load(Ordering::SeqCst), 0);
    relay_task.abort();
}
