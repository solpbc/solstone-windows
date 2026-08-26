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

mod support;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use observer_model::{LocalOffset, LocalOffsetError, SyncSnapshot};
use observer_pl::frame::{
    Frame, FrameDecoder, FLAG_CLOSE, FLAG_DATA, FLAG_RESET, FLAG_WINDOW, RESET_CANCEL,
};
use observer_pl::ingest::{FilePart, IngestStatus};
use observer_pl::mux::INITIAL_WINDOW;
use observer_pl::PROTOCOL_VERSION_HEADER;
use observer_retention::RetentionConfig;
use pl_transport_win::client::ObserverClient;
use pl_transport_win::connection::request_once;
use pl_transport_win::credential::{Credential, EndpointAddr, PairedState};
use pl_transport_win::service::{self, SyncConfig};
use pl_transport_win::tls::pairing_config;
use pl_transport_win::{journal_bridge, TransportError};
use rcgen::{
    BasicConstraints, CertificateParams, CertificateSigningRequestParams, IsCa, KeyPair,
    KeyUsagePurpose, PKCS_ECDSA_P256_SHA256,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::ServerConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

use support::observer_contract::{
    fixture as authority_fixture, v3_read_capture_matches, v3_upload_capture_matches,
    vector as authority_vector,
};

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

fn request_body(request: &[u8]) -> serde_json::Value {
    let request = String::from_utf8_lossy(request);
    let (_, body) = request.split_once("\r\n\r\n").unwrap();
    serde_json::from_str(body).unwrap()
}

fn pair_capture_matches(request: &[u8], nonce: &str, label: &str) -> bool {
    let text = String::from_utf8_lossy(request);
    let body = request_body(request);
    text.starts_with(&format!(
        "POST /app/network/pair?token={nonce} HTTP/1.1\r\n"
    )) && text.contains("Content-Type: application/json\r\n")
        && !text.contains("X-Solstone-Observer:")
        && !text.contains("Authorization:")
        && !text.contains("X-Solstone-Protocol-Version:")
        && body["device_label"] == label
        && body["csr"]
            .as_str()
            .is_some_and(|csr| csr.contains("BEGIN CERTIFICATE REQUEST"))
        && body.get("nonce").is_none()
        && body.get("sender_instance_id").is_none()
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
    serve_one_response_with_header(listener, acceptor, status, body, content_length, None).await
}

async fn serve_one_response_with_header(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    status: &str,
    body: &'static [u8],
    content_length: usize,
    extra_header: Option<&str>,
) -> Vec<u8> {
    let (tcp, _) = listener.accept().await.unwrap();
    let mut tls = acceptor.accept(tcp).await.unwrap();

    let (stream_id, request) = read_framed_request(&mut tls).await;

    let extra_header = extra_header
        .map(|header| format!("{header}\r\n"))
        .unwrap_or_default();
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {content_length}\r\n{extra_header}\r\n{}",
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

async fn serve_empty_segments(listener: TcpListener, acceptor: TlsAcceptor) -> Vec<u8> {
    serve_one_response(
        listener,
        acceptor,
        "200 OK",
        br#"{"items":[],"total":0,"protocol_version":3}"#,
    )
    .await
}

#[derive(Clone, Copy)]
enum PairCertificateMode {
    SubmittedCsr,
    UnrelatedKey,
}

async fn serve_one_pair_response(
    listener: Arc<TcpListener>,
    acceptor: TlsAcceptor,
    signing_cert: rcgen::Certificate,
    signing_key: KeyPair,
    mode: PairCertificateMode,
) -> Vec<u8> {
    let (tcp, _) = listener.accept().await.unwrap();
    let mut tls = acceptor.accept(tcp).await.unwrap();
    let (stream_id, request) = read_framed_request(&mut tls).await;
    let pair_request: observer_pl::wire::PairRequest =
        serde_json::from_value(request_body(&request)).unwrap();
    let client_cert = match mode {
        PairCertificateMode::SubmittedCsr => {
            CertificateSigningRequestParams::from_pem(&pair_request.csr)
                .unwrap()
                .signed_by(&signing_cert, &signing_key)
                .unwrap()
        }
        PairCertificateMode::UnrelatedKey => {
            let unrelated_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
            CertificateParams::new(Vec::<String>::new())
                .unwrap()
                .signed_by(&unrelated_key, &signing_cert, &signing_key)
                .unwrap()
        }
    };
    // Upstream follow-up: v9 no longer projects pairing; use the committed local fixture.
    let fixture = authority_fixture("pair_response");
    let payload = &fixture["payload"];
    let response_body = serde_json::to_vec(&serde_json::json!({
        "client_cert": client_cert.pem(),
        "ca_chain": [signing_cert.pem()],
        "instance_id": payload["instance_id"],
        "home_label": payload["home_label"],
        "fingerprint": format!("sha256:{}", observer_pl::ca::sha256_hex(client_cert.der())),
        "home_attestation": payload["home_attestation"],
        "local_endpoints": payload["local_endpoints"],
    }))
    .unwrap();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        response_body.len(),
        String::from_utf8_lossy(&response_body)
    );
    let frame = Frame::new(stream_id, FLAG_DATA | FLAG_CLOSE, response.into_bytes());
    tls.write_all(&frame.encode().unwrap()).await.unwrap();
    tls.flush().await.unwrap();
    let _ = tls.shutdown().await;
    request
}

fn signing_ca() -> (rcgen::Certificate, KeyPair) {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages.push(KeyUsagePurpose::DigitalSignature);
    params.key_usages.push(KeyUsagePurpose::KeyCertSign);
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
    let cert = params.self_signed(&key).unwrap();
    (cert, key)
}

#[derive(Debug)]
struct TestOffset;

impl LocalOffset for TestOffset {
    fn local_offset_secs(&self, _epoch_secs: u64) -> Result<i64, LocalOffsetError> {
        Ok(0)
    }
}

fn direct_pair_link(port: u16, ca_fp_prefix: &[u8]) -> String {
    let mut blob = vec![0x04, 0x01, 127, 0, 0, 1];
    blob.extend_from_slice(&port.to_be_bytes());
    blob.extend(0u8..16);
    blob.extend_from_slice(ca_fp_prefix);
    format!(
        "https://go.solstone.app/p#{}",
        observer_pl::crockford::encode(&blob)
    )
}

fn service_config(state_path: PathBuf) -> SyncConfig {
    SyncConfig {
        device_label: "service-pair-test".into(),
        period_secs: 300,
        segments_root: state_path.with_extension("segments"),
        state_path,
        retention: Arc::new(RwLock::new(RetentionConfig::default())),
        local_offset: Arc::new(TestOffset),
    }
}

async fn serve_drop_then_empty_segments(listener: TcpListener, acceptor: TlsAcceptor) -> Vec<u8> {
    let (tcp, _) = listener.accept().await.unwrap();
    drop(tcp);
    serve_empty_segments(listener, acceptor).await
}

#[derive(Clone, Copy)]
enum SseMode {
    Close,
    EofBeforeHead,
    EofAfterHeadAndPartialBody,
    Authority(&'static [u8]),
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

    write_response_frame(
        &mut tls,
        stream_id,
        FLAG_DATA,
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n",
    )
    .await;
    if matches!(mode, SseMode::EofAfterHeadAndPartialBody) {
        write_response_frame(&mut tls, stream_id, FLAG_DATA, b"data: partial").await;
        return request;
    }
    if let SseMode::Authority(body) = mode {
        write_response_frame(&mut tls, stream_id, FLAG_DATA, body).await;
        write_response_frame(&mut tls, stream_id, FLAG_CLOSE, b"").await;
        let _ = tls.shutdown().await;
        return request;
    }
    write_response_frame(&mut tls, stream_id, FLAG_DATA, b"data: 1\n\n").await;
    write_response_frame(&mut tls, stream_id, FLAG_DATA, b"data: 2\n\n").await;
    write_response_frame(&mut tls, stream_id, FLAG_CLOSE, b"").await;

    let _ = tls.shutdown().await;
    request
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

async fn start_client_with_response(
    status: &'static str,
    body: &'static [u8],
) -> (ObserverClient, JoinHandle<Vec<u8>>) {
    let (cert, key) = self_signed();
    let pin = observer_pl::ca::sha256(cert.as_ref())[..16].to_vec();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_one_response(listener, acceptor, status, body));
    let client = ObserverClient::new(observer_credential(pin, port)).unwrap();
    (client, server)
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

struct PersistentRequest {
    carrier_index: usize,
    stream_id: u32,
    bytes: Vec<u8>,
}

enum PersistentWrite {
    Frame(Vec<u8>),
    CloseCarrier,
}

struct PersistentBridgeServer {
    accepts: Arc<AtomicUsize>,
    requests: mpsc::Receiver<PersistentRequest>,
    writes: mpsc::UnboundedSender<PersistentWrite>,
    task: JoinHandle<()>,
}

impl PersistentBridgeServer {
    async fn next_request(&mut self) -> PersistentRequest {
        tokio::time::timeout(std::time::Duration::from_secs(3), self.requests.recv())
            .await
            .expect("timed out waiting for upstream mux request")
            .expect("persistent upstream closed before request")
    }

    fn send_http(&self, stream_id: u32, status: &str, body: &[u8]) {
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            String::from_utf8_lossy(body)
        );
        self.send_frame(stream_id, FLAG_DATA | FLAG_CLOSE, response.as_bytes());
    }

    fn send_sse_head(&self, stream_id: u32) {
        self.send_frame(
            stream_id,
            FLAG_DATA,
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n",
        );
    }

    fn send_body(&self, stream_id: u32, body: &[u8]) {
        self.send_frame(stream_id, FLAG_DATA, body);
    }

    fn close_stream(&self, stream_id: u32) {
        self.send_frame(stream_id, FLAG_CLOSE, b"");
    }

    fn reset_stream(&self, stream_id: u32) {
        self.send_frame(stream_id, FLAG_RESET, &[RESET_CANCEL]);
    }

    fn send_frame(&self, stream_id: u32, flags: u8, payload: &[u8]) {
        let frame = Frame::new(stream_id, flags, payload.to_vec())
            .encode()
            .unwrap();
        self.writes.send(PersistentWrite::Frame(frame)).unwrap();
    }

    fn close_current_carrier(&self) {
        self.writes.send(PersistentWrite::CloseCarrier).unwrap();
    }

    fn accepted_carriers(&self) -> usize {
        self.accepts.load(Ordering::SeqCst)
    }

    fn abort(self) {
        self.task.abort();
    }
}

async fn start_bridge_with_persistent_server(
) -> (journal_bridge::JournalBridgeHandle, PersistentBridgeServer) {
    let (cert, key) = self_signed();
    let pin = observer_pl::ca::sha256(cert.as_ref())[..16].to_vec();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_port = listener.local_addr().unwrap().port();
    let accepts = Arc::new(AtomicUsize::new(0));
    let (request_tx, request_rx) = mpsc::channel(16);
    let (write_tx, mut write_rx) = mpsc::unbounded_channel::<PersistentWrite>();
    let task = tokio::spawn({
        let accepts = accepts.clone();
        async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    break;
                };
                let carrier_index = accepts.fetch_add(1, Ordering::SeqCst) + 1;
                let tls = acceptor.accept(tcp).await.unwrap();
                let (mut read, mut write) = tokio::io::split(tls);
                let mut decoder = FrameDecoder::new();
                let mut requests: HashMap<u32, Vec<u8>> = HashMap::new();
                let mut buf = [0u8; 4096];
                loop {
                    tokio::select! {
                        read_result = read.read(&mut buf) => {
                            let Ok(n) = read_result else {
                                break;
                            };
                            if n == 0 {
                                break;
                            }
                            decoder.feed(&buf[..n]);
                            for frame in decoder.drain().unwrap() {
                                if let Some(pong) = frame.control_pong() {
                                    let bytes = pong.encode().unwrap();
                                    write.write_all(&bytes).await.unwrap();
                                    write.flush().await.unwrap();
                                    continue;
                                }
                                if frame.flags & FLAG_DATA != 0 {
                                    requests
                                        .entry(frame.stream_id)
                                        .or_default()
                                        .extend_from_slice(&frame.payload);
                                }
                                if frame.flags & FLAG_CLOSE != 0 {
                                    let bytes = requests.remove(&frame.stream_id).unwrap_or_default();
                                    request_tx
                                        .send(PersistentRequest {
                                            carrier_index,
                                            stream_id: frame.stream_id,
                                            bytes,
                                        })
                                        .await
                                        .unwrap();
                                }
                            }
                        }
                        write_command = write_rx.recv() => {
                            match write_command {
                                Some(PersistentWrite::Frame(bytes)) => {
                                    if write.write_all(&bytes).await.is_err() || write.flush().await.is_err() {
                                        break;
                                    }
                                }
                                Some(PersistentWrite::CloseCarrier) => {
                                    let _ = write.shutdown().await;
                                    break;
                                }
                                None => return,
                            }
                        }
                    }
                }
            }
        }
    });
    let paired = paired_state(observer_credential(pin, upstream_port));
    let handle = journal_bridge::start(&paired, temp_state_path("persistent"))
        .await
        .unwrap();
    (
        handle,
        PersistentBridgeServer {
            accepts,
            requests: request_rx,
            writes: write_tx,
            task,
        },
    )
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
async fn observer_contract_authority_direct_pairing_uses_real_crypto_and_request_path() {
    // Upstream follow-up: v9 no longer projects pairing; use the committed local fixture.
    let request_fixture = authority_fixture("pair_request");
    let response_fixture = authority_fixture("pair_response");
    let (signing_cert, signing_key) = signing_ca();

    let (server_cert, server_key) = self_signed();
    let server_pin = observer_pl::ca::sha256(server_cert.as_ref())[..16].to_vec();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(server_cert, server_key)));
    let listener = Arc::new(TcpListener::bind("127.0.0.1:0").await.unwrap());
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_one_pair_response(
        listener,
        acceptor,
        signing_cert,
        signing_key,
        PairCertificateMode::SubmittedCsr,
    ));
    let nonce = request_fixture["payload"]["nonce"].as_str().unwrap();
    let label = request_fixture["payload"]["device_label"].as_str().unwrap();
    let endpoints = [observer_pl::pairlink::Endpoint {
        host: "127.0.0.1".to_owned(),
        port,
    }];

    let credential = pl_transport_win::pairing::pair(&endpoints, nonce, &server_pin, label)
        .await
        .unwrap();
    assert_eq!(
        credential.instance_id,
        response_fixture["payload"]["instance_id"]
    );
    assert_eq!(
        credential.home_label,
        response_fixture["payload"]["home_label"]
    );
    assert!(credential.client_cert_pem.contains("BEGIN CERTIFICATE"));
    let credential_key = KeyPair::from_pem(&credential.client_key_pem).unwrap();
    let credential_leaf = pl_transport_win::tls::parse_certs(&credential.client_cert_pem)
        .unwrap()
        .remove(0);
    assert_eq!(
        credential_key.public_key_der(),
        observer_pl::ca::extract_spki_der(credential_leaf.as_ref()).unwrap()
    );
    let request = server.await.unwrap();
    assert!(pair_capture_matches(&request, nonce, label));
    let mutated = String::from_utf8(request.clone()).unwrap().replacen(
        &format!("token={nonce}"),
        "token=wrong",
        1,
    );
    assert!(!pair_capture_matches(mutated.as_bytes(), nonce, label));
}

#[tokio::test]
async fn direct_pairing_key_mismatch_is_terminal_after_first_written_request() {
    let (signing_cert, signing_key) = signing_ca();

    let (server_cert, server_key) = self_signed();
    let server_pin = observer_pl::ca::sha256(server_cert.as_ref())[..16].to_vec();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(server_cert, server_key)));
    let first_listener = Arc::new(TcpListener::bind("127.0.0.1:0").await.unwrap());
    let first_port = first_listener.local_addr().unwrap().port();
    let first_server = tokio::spawn(serve_one_pair_response(
        first_listener,
        acceptor,
        signing_cert,
        signing_key,
        PairCertificateMode::UnrelatedKey,
    ));

    let later_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let later_port = later_listener.local_addr().unwrap().port();
    let later_accepts = Arc::new(AtomicUsize::new(0));
    let later_server = tokio::spawn({
        let later_accepts = later_accepts.clone();
        async move {
            if let Ok(Ok((tcp, _))) = tokio::time::timeout(
                std::time::Duration::from_millis(250),
                later_listener.accept(),
            )
            .await
            {
                later_accepts.fetch_add(1, Ordering::SeqCst);
                drop(tcp);
            }
        }
    });
    let endpoints = [
        observer_pl::pairlink::Endpoint {
            host: "127.0.0.1".into(),
            port: first_port,
        },
        observer_pl::pairlink::Endpoint {
            host: "127.0.0.1".into(),
            port: later_port,
        },
    ];

    let error = pl_transport_win::pairing::pair(
        &endpoints,
        "00112233445566778899aabbccddeeff",
        &server_pin,
        "win-test",
    )
    .await
    .unwrap_err();

    assert!(matches!(
        error,
        TransportError::Pairing(message)
            if message == "client certificate public key does not match generated key"
    ));
    let request = first_server.await.unwrap();
    assert!(String::from_utf8_lossy(&request)
        .starts_with("POST /app/network/pair?token=00112233445566778899aabbccddeeff HTTP/1.1"));
    later_server.await.unwrap();
    assert_eq!(later_accepts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn service_pair_persists_credential_without_register_request() {
    let (signing_cert, signing_key) = signing_ca();
    let (server_cert, server_key) = self_signed();
    let server_pin = observer_pl::ca::sha256(server_cert.as_ref())[..16].to_vec();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(server_cert, server_key)));
    let listener = Arc::new(TcpListener::bind("127.0.0.1:0").await.unwrap());
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_one_pair_response(
        listener.clone(),
        acceptor,
        signing_cert,
        signing_key,
        PairCertificateMode::SubmittedCsr,
    ));
    let state_path = temp_state_path("service-pair");
    let cfg = service_config(state_path.clone());
    let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
    let link = direct_pair_link(port, &server_pin);

    let paired = service::pair(&link, &cfg, sync.clone())
        .await
        .expect("pair-only journal should persist a credential");

    assert!(paired.credential.is_some());
    assert!(PairedState::load(&state_path).unwrap().credential.is_some());
    assert_eq!(
        sync.lock().unwrap().pairing.phase,
        observer_model::PairingPhase::Paired
    );

    let requests = [server.await.unwrap()];
    assert!(
        requests.iter().all(|request| {
            let text = String::from_utf8_lossy(request);
            !text.contains("/app/devices/register")
        }),
        "pair-only fixture received a register-shaped request"
    );
    assert!(pair_capture_matches(
        &requests[0],
        "000102030405060708090a0b0c0d0e0f",
        "service-pair-test"
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err(),
        "service::pair must not make a follow-up registration connection"
    );

    let _ = std::fs::remove_file(state_path);
}

#[tokio::test]
async fn protocol_v3_list_segments_is_strict_and_mtls_only() {
    let day = "20260729";
    let (client, server) =
        start_client_with_response("200 OK", br#"{"items":[],"total":0,"protocol_version":3}"#)
            .await;
    let (response, _) = client.list_segments(day).await.unwrap();
    assert_eq!(response.protocol_version, 3);
    let request = String::from_utf8(server.await.unwrap()).unwrap();
    assert!(request.starts_with(&format!(
        "GET /app/devices/ingest/segments/{day} HTTP/1.1\r\n"
    )));
    assert!(request.contains(&format!("{PROTOCOL_VERSION_HEADER}: 3\r\n")));
    assert!(!request.contains("Authorization:"));
    assert!(!request.contains("X-Solstone-Observer:"));
}

#[tokio::test]
async fn protocol_v3_manifest_reads_do_not_require_an_observer_handle() {
    let day = "20260730";
    let (client, root_server) =
        start_client_with_response("200 OK", br#"{"days":{"20260730":{"segments":1}}}"#).await;
    let (manifest, _) = client.ingest_manifest().await.unwrap();
    assert_eq!(manifest.days[day].segments, 1);
    let root_request = String::from_utf8(root_server.await.unwrap()).unwrap();
    assert!(root_request.starts_with("GET /app/devices/ingest/manifest HTTP/1.1\r\n"));
    assert!(!root_request.contains("Authorization:"));

    let (client, day_server) =
        start_client_with_response("200 OK", br#"{"version":1,"day":"20260730","segments":{}}"#)
            .await;
    let (day_manifest, _) = client.ingest_manifest_day(day).await.unwrap();
    assert_eq!(day_manifest.day, day);
    let day_request = String::from_utf8(day_server.await.unwrap()).unwrap();
    assert!(day_request.starts_with(&format!(
        "GET /app/devices/ingest/manifest/{day} HTTP/1.1\r\n"
    )));
    assert!(!day_request.contains("X-Solstone-Observer:"));
}

#[tokio::test]
async fn protocol_v3_ingest_captures_envelope_and_conflict_status() {
    let day = "20260729";
    let segment = "080000_600";
    let filenames = ["screen-unique.mp4", "audio-unique.flac"];
    let files = filenames
        .iter()
        .map(|filename| FilePart {
            filename: (*filename).into(),
            content_type: "application/octet-stream".into(),
            bytes: filename.as_bytes().to_vec(),
        })
        .collect();
    let (client, server) =
        start_client_with_response("200 OK", br#"{"status":"ok","segment":"080000_600"}"#).await;
    let (response, _) = client.ingest(segment, day, files).await.unwrap();
    assert_eq!(response.status, IngestStatus::Ok);
    let request = server.await.unwrap();
    assert!(v3_upload_capture_matches(
        &request, day, segment, &filenames
    ));

    let (client, server) =
        start_client_with_response("409 Conflict", br#"{"status":"conflict"}"#).await;
    let (response, _) = client
        .ingest(
            segment,
            day,
            vec![FilePart {
                filename: "conflict.wav".into(),
                content_type: "audio/wav".into(),
                bytes: vec![9],
            }],
        )
        .await
        .unwrap();
    assert_eq!(response.status, IngestStatus::Conflict);
    let request = String::from_utf8(server.await.unwrap()).unwrap();
    assert!(!request.contains("Authorization:"));
}

#[tokio::test]
async fn observer_contract_authority_direct_v3_operations_have_identical_mtls_only_policy() {
    let day = "20260820";
    let segment = "080000_600";
    let filenames = ["screen.mp4", "audio.flac"];

    let (client, server) =
        start_client_with_response("200 OK", br#"{"status":"ok","segment":"080000_600"}"#).await;
    client
        .ingest(
            segment,
            day,
            filenames
                .iter()
                .map(|filename| FilePart {
                    filename: (*filename).to_owned(),
                    content_type: "application/octet-stream".to_owned(),
                    bytes: filename.as_bytes().to_vec(),
                })
                .collect(),
        )
        .await
        .unwrap();
    let upload = server.await.unwrap();
    assert!(v3_upload_capture_matches(&upload, day, segment, &filenames));
    let without_protocol_header = String::from_utf8(upload.clone()).unwrap().replacen(
        "X-Solstone-Protocol-Version: 3\r\n",
        "",
        1,
    );
    assert!(
        !v3_upload_capture_matches(without_protocol_header.as_bytes(), day, segment, &filenames),
        "removing the v3 protocol header must invalidate the v3 upload assertion"
    );
    for (from, to) in [
        (
            "X-Solstone-Protocol-Version: 3",
            "X-Solstone-Protocol-Version: 2",
        ),
        (
            "\"submitted\":\"screen.mp4\"",
            "\"submitted\":\"wrong.mp4\"",
        ),
        ("name=\"files\"", "name=\"segment\""),
        ("name=\"files\"", "name=\"day\""),
        ("name=\"files\"", "name=\"platform\""),
    ] {
        let mutated = String::from_utf8(upload.clone())
            .unwrap()
            .replacen(from, to, 1);
        assert!(
            !v3_upload_capture_matches(mutated.as_bytes(), day, segment, &filenames),
            "mutation must invalidate the v3 upload assertion: {from}"
        );
    }

    let (client, server) =
        start_client_with_response("200 OK", br#"{"days":{"20260820":{"segments":1}}}"#).await;
    client.ingest_manifest().await.unwrap();
    assert!(v3_read_capture_matches(
        &server.await.unwrap(),
        "GET",
        "/app/devices/ingest/manifest"
    ));

    let (client, server) =
        start_client_with_response("200 OK", br#"{"version":1,"day":"20260820","segments":{}}"#)
            .await;
    client.ingest_manifest_day(day).await.unwrap();
    assert!(v3_read_capture_matches(
        &server.await.unwrap(),
        "GET",
        "/app/devices/ingest/manifest/20260820"
    ));

    let (client, server) =
        start_client_with_response("200 OK", br#"{"items":[],"total":0,"protocol_version":3}"#)
            .await;
    client.list_segments(day).await.unwrap();
    assert!(v3_read_capture_matches(
        &server.await.unwrap(),
        "GET",
        "/app/devices/ingest/segments/20260820"
    ));
}

#[tokio::test]
async fn observer_contract_authority_direct_status_vectors_use_documented_http_mapping() {
    for vector_id in xtask::observer_contract::VECTOR_IDS {
        let vector = authority_vector(vector_id);
        let fixture = authority_fixture(vector["fixture_id"].as_str().unwrap());
        let status = vector["decision"]["http_status"].as_u64().unwrap();
        let reason = match status {
            200 => "OK",
            409 => "Conflict",
            500 => "Internal Server Error",
            _ => unreachable!("pinned status"),
        };
        let body: &'static [u8] = Box::leak(
            serde_json::to_vec(&fixture["payload"])
                .unwrap()
                .into_boxed_slice(),
        );
        let (client, server) = start_client_with_response(
            Box::leak(format!("{status} {reason}").into_boxed_str()),
            body,
        )
        .await;
        let (response, _) = client
            .ingest(
                "080000_600",
                "20260820",
                vec![FilePart {
                    filename: "status.wav".into(),
                    content_type: "audio/wav".into(),
                    bytes: vec![1],
                }],
            )
            .await
            .expect("documented status response parses");
        assert_eq!(
            response.status.is_accepted(),
            vector["decision"]["accepted"].as_bool().unwrap(),
            "{vector_id}"
        );
        let request = server.await.unwrap();
        assert!(v3_upload_capture_matches(
            &request,
            "20260820",
            "080000_600",
            &["status.wav"]
        ));
    }
}

#[tokio::test]
async fn observer_contract_authority_direct_v3_reads_fail_closed_on_non_success() {
    for status in [403, 500] {
        let text: &'static str = Box::leak(format!("{status} Rejected").into_boxed_str());
        let (client, server) = start_client_with_response(text, br#"{}"#).await;
        assert!(matches!(
            client.ingest_manifest().await,
            Err(TransportError::Rejected { status: actual, .. }) if actual == status
        ));
        assert!(v3_read_capture_matches(
            &server.await.unwrap(),
            "GET",
            "/app/devices/ingest/manifest"
        ));

        let (client, server) = start_client_with_response(text, br#"{}"#).await;
        assert!(matches!(
            client.ingest_manifest_day("20260820").await,
            Err(TransportError::Rejected { status: actual, .. }) if actual == status
        ));
        assert!(v3_read_capture_matches(
            &server.await.unwrap(),
            "GET",
            "/app/devices/ingest/manifest/20260820"
        ));

        let (client, server) = start_client_with_response(text, br#"{}"#).await;
        assert!(matches!(
            client.list_segments("20260820").await,
            Err(TransportError::Rejected { status: actual, .. }) if actual == status
        ));
        assert!(v3_read_capture_matches(
            &server.await.unwrap(),
            "GET",
            "/app/devices/ingest/segments/20260820"
        ));
    }
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
async fn journal_bridge_buffered_pass_through_adds_v3_headers_and_strips_local_headers() {
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
    assert!(request.contains("X-Solstone-Protocol-Version: 3\r\n"));
    assert!(!request.contains("X-Solstone-Observer:"));
    assert!(!request.contains("Authorization:"));
    assert!(!request.contains("X-Solstone-Protocol-Version: 2"));
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
async fn journal_bridge_sse_fail_after_head_does_not_emit_502() {
    let (handle, upstream) = start_bridge_with_sse(SseMode::EofAfterHeadAndPartialBody).await;
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

    let text = response_text(&response);
    assert_eq!(text.matches("HTTP/1.1").count(), 1);
    assert!(text.starts_with("HTTP/1.1 200"));
    assert!(response_body(&response).contains("data: partial"));
    assert!(!text.contains("502"));
    assert!(!text.contains("journal unreachable"));
    let _ = upstream.await.unwrap();
    handle.shutdown_and_wait().await;
}

#[tokio::test]
async fn observer_contract_authority_root_sse_preserves_data_and_heartbeat_bytes() {
    // Upstream follow-up: v9 no longer projects callosum; use the committed local fixture.
    let fixture = authority_fixture("root_sse");
    for payload in [
        fixture["payload"]["default"].clone(),
        fixture["payload"]["unknown"].clone(),
        fixture["payload"]["heartbeat"].clone(),
    ] {
        let expected = if payload.is_string() {
            payload.as_str().unwrap().as_bytes().to_vec()
        } else {
            format!("data: {}\n\n", serde_json::to_string(&payload).unwrap()).into_bytes()
        };
        let expected: &'static [u8] = Box::leak(expected.into_boxed_slice());
        let (handle, upstream) = start_bridge_with_sse(SseMode::Authority(expected)).await;
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
        assert_eq!(response_body(&response).as_bytes(), expected);
        let head = response_head(&response);
        assert!(head.contains("content-type: text/event-stream"));
        assert!(!head.contains("content-length"));
        assert!(!head.contains("transfer-encoding"));
        let request = upstream.await.unwrap();
        assert!(String::from_utf8_lossy(&request)
            .starts_with(&format!("{} {} HTTP/1.1\r\n", "GET", "/sse/events")));
        handle.shutdown_and_wait().await;
    }
}

#[tokio::test]
async fn journal_bridge_reuses_one_carrier_for_sequential_requests() {
    let (handle, mut server) = start_bridge_with_persistent_server().await;
    let port = handle.port();
    let cap = capability_from(&handle);

    let first_cap = cap.clone();
    let first = tokio::spawn(async move {
        raw_bridge_request(
            port,
            "GET",
            "/first",
            Some(loopback_host(port)),
            Some(cap_cookie(&first_cap)),
            &[],
            b"",
        )
        .await
    });
    let first_request = server.next_request().await;
    assert!(String::from_utf8_lossy(&first_request.bytes).starts_with("GET /first HTTP/1.1\r\n"));
    server.send_http(first_request.stream_id, "200 OK", b"first");
    let first_response = first.await.unwrap();
    assert_eq!(response_status(&first_response), 200);
    assert_eq!(response_body(&first_response), "first");

    let second_cap = cap.clone();
    let second = tokio::spawn(async move {
        raw_bridge_request(
            port,
            "GET",
            "/second",
            Some(loopback_host(port)),
            Some(cap_cookie(&second_cap)),
            &[],
            b"",
        )
        .await
    });
    let second_request = server.next_request().await;
    assert!(String::from_utf8_lossy(&second_request.bytes).starts_with("GET /second HTTP/1.1\r\n"));
    server.send_http(second_request.stream_id, "200 OK", b"second");
    let second_response = second.await.unwrap();
    assert_eq!(response_status(&second_response), 200);
    assert_eq!(response_body(&second_response), "second");

    assert_eq!(server.accepted_carriers(), 1);
    handle.shutdown_and_wait().await;
    server.abort();
}

#[tokio::test]
async fn journal_bridge_first_load_concurrent_requests_coalesce_one_carrier() {
    let (handle, mut server) = start_bridge_with_persistent_server().await;
    let port = handle.port();
    let cap = capability_from(&handle);

    let first_cap = cap.clone();
    let first = tokio::spawn(async move {
        raw_bridge_request(
            port,
            "GET",
            "/first-load-a",
            Some(loopback_host(port)),
            Some(cap_cookie(&first_cap)),
            &[],
            b"",
        )
        .await
    });
    let second_cap = cap.clone();
    let second = tokio::spawn(async move {
        raw_bridge_request(
            port,
            "GET",
            "/first-load-b",
            Some(loopback_host(port)),
            Some(cap_cookie(&second_cap)),
            &[],
            b"",
        )
        .await
    });

    let req_a = server.next_request().await;
    let req_b = server.next_request().await;
    assert_eq!(req_a.carrier_index, 1);
    assert_eq!(req_b.carrier_index, 1);
    let mut stream_ids = [req_a.stream_id, req_b.stream_id];
    stream_ids.sort();
    assert_eq!(stream_ids, [1, 3]);
    server.send_http(req_a.stream_id, "200 OK", b"a");
    server.send_http(req_b.stream_id, "200 OK", b"b");

    assert_eq!(response_status(&first.await.unwrap()), 200);
    assert_eq!(response_status(&second.await.unwrap()), 200);
    assert_eq!(server.accepted_carriers(), 1);

    handle.shutdown_and_wait().await;
    server.abort();
}

#[tokio::test]
async fn journal_bridge_two_handles_use_separate_caps_and_carriers() {
    let (handle1, mut server1) = start_bridge_with_persistent_server().await;
    let (handle2, mut server2) = start_bridge_with_persistent_server().await;
    let port1 = handle1.port();
    let port2 = handle2.port();
    let cap1 = capability_from(&handle1);
    let cap2 = capability_from(&handle2);
    assert_ne!(cap1, cap2);

    let cap1_for_request = cap1.clone();
    let one = tokio::spawn(async move {
        raw_bridge_request(
            port1,
            "GET",
            "/one",
            Some(loopback_host(port1)),
            Some(cap_cookie(&cap1_for_request)),
            &[],
            b"",
        )
        .await
    });
    let cap2_for_request = cap2.clone();
    let two = tokio::spawn(async move {
        raw_bridge_request(
            port2,
            "GET",
            "/two",
            Some(loopback_host(port2)),
            Some(cap_cookie(&cap2_for_request)),
            &[],
            b"",
        )
        .await
    });

    let req1 = server1.next_request().await;
    let req2 = server2.next_request().await;
    assert!(String::from_utf8_lossy(&req1.bytes).starts_with("GET /one HTTP/1.1\r\n"));
    assert!(String::from_utf8_lossy(&req2.bytes).starts_with("GET /two HTTP/1.1\r\n"));
    server1.send_http(req1.stream_id, "200 OK", b"one");
    server2.send_http(req2.stream_id, "200 OK", b"two");

    assert_eq!(response_body(&one.await.unwrap()), "one");
    assert_eq!(response_body(&two.await.unwrap()), "two");
    assert_eq!(server1.accepted_carriers(), 1);
    assert_eq!(server2.accepted_carriers(), 1);

    handle1.shutdown_and_wait().await;
    handle2.shutdown_and_wait().await;
    server1.abort();
    server2.abort();
}

#[tokio::test]
async fn journal_bridge_interleaves_streams_to_correct_clients_on_one_carrier() {
    let (handle, mut server) = start_bridge_with_persistent_server().await;
    let port = handle.port();
    let cap = capability_from(&handle);

    let first_cap = cap.clone();
    let first = tokio::spawn(async move {
        raw_bridge_request(
            port,
            "GET",
            "/first",
            Some(loopback_host(port)),
            Some(cap_cookie(&first_cap)),
            &[],
            b"",
        )
        .await
    });
    let second_cap = cap.clone();
    let second = tokio::spawn(async move {
        raw_bridge_request(
            port,
            "GET",
            "/second",
            Some(loopback_host(port)),
            Some(cap_cookie(&second_cap)),
            &[],
            b"",
        )
        .await
    });

    let req_a = server.next_request().await;
    let req_b = server.next_request().await;
    let text_a = String::from_utf8_lossy(&req_a.bytes);
    let (first_req, second_req) = if text_a.starts_with("GET /first ") {
        (req_a, req_b)
    } else {
        (req_b, req_a)
    };
    assert!(String::from_utf8_lossy(&first_req.bytes).starts_with("GET /first HTTP/1.1\r\n"));
    assert!(String::from_utf8_lossy(&second_req.bytes).starts_with("GET /second HTTP/1.1\r\n"));

    server.send_http(second_req.stream_id, "200 OK", b"second");
    server.send_http(first_req.stream_id, "200 OK", b"first");

    let first_response = first.await.unwrap();
    let second_response = second.await.unwrap();
    assert_eq!(response_body(&first_response), "first");
    assert_eq!(response_body(&second_response), "second");
    assert_eq!(server.accepted_carriers(), 1);

    handle.shutdown_and_wait().await;
    server.abort();
}

#[tokio::test]
async fn journal_bridge_reset_isolates_one_stream_on_shared_carrier() {
    let (handle, mut server) = start_bridge_with_persistent_server().await;
    let port = handle.port();
    let cap = capability_from(&handle);

    let reset_cap = cap.clone();
    let reset_client = tokio::spawn(async move {
        raw_bridge_request(
            port,
            "GET",
            "/reset-me",
            Some(loopback_host(port)),
            Some(cap_cookie(&reset_cap)),
            &[],
            b"",
        )
        .await
    });
    let ok_cap = cap.clone();
    let ok_client = tokio::spawn(async move {
        raw_bridge_request(
            port,
            "GET",
            "/still-ok",
            Some(loopback_host(port)),
            Some(cap_cookie(&ok_cap)),
            &[],
            b"",
        )
        .await
    });

    let req_a = server.next_request().await;
    let req_b = server.next_request().await;
    let text_a = String::from_utf8_lossy(&req_a.bytes);
    let (reset_req, ok_req) = if text_a.starts_with("GET /reset-me ") {
        (req_a, req_b)
    } else {
        (req_b, req_a)
    };
    assert!(String::from_utf8_lossy(&reset_req.bytes).starts_with("GET /reset-me HTTP/1.1\r\n"));
    assert!(String::from_utf8_lossy(&ok_req.bytes).starts_with("GET /still-ok HTTP/1.1\r\n"));

    server.reset_stream(reset_req.stream_id);
    server.send_http(ok_req.stream_id, "200 OK", b"survived");

    let reset_response = reset_client.await.unwrap();
    let ok_response = ok_client.await.unwrap();
    assert_eq!(response_status(&reset_response), 502);
    assert_eq!(response_status(&ok_response), 200);
    assert_eq!(response_body(&ok_response), "survived");
    assert_eq!(server.accepted_carriers(), 1);

    handle.shutdown_and_wait().await;
    server.abort();
}

#[tokio::test]
async fn journal_bridge_sse_does_not_block_second_get_on_same_carrier() {
    let (handle, mut server) = start_bridge_with_persistent_server().await;
    let port = handle.port();
    let cap = capability_from(&handle);

    let sse_cap = cap.clone();
    let sse = tokio::spawn(async move {
        raw_bridge_request(
            port,
            "GET",
            "/sse/events",
            Some(loopback_host(port)),
            Some(cap_cookie(&sse_cap)),
            &[],
            b"",
        )
        .await
    });
    let sse_request = server.next_request().await;
    assert!(String::from_utf8_lossy(&sse_request.bytes).starts_with("GET /sse/events HTTP/1.1\r\n"));
    server.send_sse_head(sse_request.stream_id);
    server.send_body(sse_request.stream_id, b"data: 1\n\n");

    let get_cap = cap.clone();
    let get = tokio::spawn(async move {
        raw_bridge_request(
            port,
            "GET",
            "/journal",
            Some(loopback_host(port)),
            Some(cap_cookie(&get_cap)),
            &[],
            b"",
        )
        .await
    });
    let get_request = server.next_request().await;
    assert!(String::from_utf8_lossy(&get_request.bytes).starts_with("GET /journal HTTP/1.1\r\n"));
    server.send_http(get_request.stream_id, "200 OK", b"ok while sse open");

    let get_response = tokio::time::timeout(std::time::Duration::from_millis(500), get)
        .await
        .expect("second GET should not wait for SSE to close")
        .unwrap();
    assert_eq!(response_status(&get_response), 200);
    assert_eq!(response_body(&get_response), "ok while sse open");

    server.send_body(sse_request.stream_id, b"data: 2\n\n");
    server.close_stream(sse_request.stream_id);
    let sse_response = tokio::time::timeout(std::time::Duration::from_secs(1), sse)
        .await
        .expect("SSE should close after upstream close")
        .unwrap();
    assert_eq!(response_status(&sse_response), 200);
    let sse_body = response_body(&sse_response);
    assert!(sse_body.contains("data: 1\n\n"));
    assert!(sse_body.contains("data: 2\n\n"));
    assert_eq!(server.accepted_carriers(), 1);

    handle.shutdown_and_wait().await;
    server.abort();
}

#[tokio::test]
async fn journal_bridge_shutdown_closes_active_carrier_and_streams() {
    let (handle, mut server) = start_bridge_with_persistent_server().await;
    let port = handle.port();
    let cap = capability_from(&handle);

    let mut local = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let request = format!(
        "GET /sse/events HTTP/1.1\r\nHost: {}\r\nCookie: {}\r\n\r\n",
        loopback_host(port),
        cap_cookie(&cap)
    );
    local.write_all(request.as_bytes()).await.unwrap();
    local.flush().await.unwrap();

    let sse_request = server.next_request().await;
    server.send_sse_head(sse_request.stream_id);
    server.send_body(sse_request.stream_id, b"data: one\n\n");

    let mut sse_response = Vec::new();
    let mut buf = [0u8; 256];
    while !response_text(&sse_response).contains("data: one\n\n") {
        let n = tokio::time::timeout(std::time::Duration::from_secs(1), local.read(&mut buf))
            .await
            .expect("SSE bytes should arrive before shutdown")
            .unwrap();
        assert!(n > 0, "SSE closed before first body item");
        sse_response.extend_from_slice(&buf[..n]);
    }

    handle.shutdown_and_wait().await;
    let mut tail = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        local.read_to_end(&mut tail),
    )
    .await
    .expect("shutdown should close active SSE")
    .unwrap();
    sse_response.extend_from_slice(&tail);
    assert_eq!(response_status(&sse_response), 200);
    assert!(response_body(&sse_response).contains("data: one\n\n"));
    assert!(tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .is_err());
    server.abort();
}

#[tokio::test]
async fn journal_bridge_carrier_death_redials_without_replaying_failed_stream() {
    let (handle, mut server) = start_bridge_with_persistent_server().await;
    let port = handle.port();
    let cap = capability_from(&handle);

    let ok_cap = cap.clone();
    let ok = tokio::spawn(async move {
        raw_bridge_request(
            port,
            "GET",
            "/ok",
            Some(loopback_host(port)),
            Some(cap_cookie(&ok_cap)),
            &[],
            b"",
        )
        .await
    });
    let ok_request = server.next_request().await;
    assert_eq!(ok_request.carrier_index, 1);
    assert!(String::from_utf8_lossy(&ok_request.bytes).starts_with("GET /ok HTTP/1.1\r\n"));
    server.send_http(ok_request.stream_id, "200 OK", b"ok");
    assert_eq!(response_body(&ok.await.unwrap()), "ok");

    let dying_cap = cap.clone();
    let dying = tokio::spawn(async move {
        raw_bridge_request(
            port,
            "GET",
            "/dies",
            Some(loopback_host(port)),
            Some(cap_cookie(&dying_cap)),
            &[],
            b"",
        )
        .await
    });
    let dying_request = server.next_request().await;
    assert_eq!(dying_request.carrier_index, 1);
    assert!(String::from_utf8_lossy(&dying_request.bytes).starts_with("GET /dies HTTP/1.1\r\n"));
    server.close_current_carrier();
    let dying_response = tokio::time::timeout(std::time::Duration::from_secs(1), dying)
        .await
        .expect("dead carrier should fail in-flight local request")
        .unwrap();
    assert_eq!(response_status(&dying_response), 502);

    let after_cap = cap.clone();
    let after = tokio::spawn(async move {
        raw_bridge_request(
            port,
            "GET",
            "/after",
            Some(loopback_host(port)),
            Some(cap_cookie(&after_cap)),
            &[],
            b"",
        )
        .await
    });
    let after_request = server.next_request().await;
    assert_eq!(after_request.carrier_index, 2);
    assert!(String::from_utf8_lossy(&after_request.bytes).starts_with("GET /after HTTP/1.1\r\n"));
    assert!(
        !String::from_utf8_lossy(&after_request.bytes).starts_with("GET /dies HTTP/1.1\r\n"),
        "failed in-flight request must not be replayed on the new carrier"
    );
    server.send_http(after_request.stream_id, "200 OK", b"after");
    assert_eq!(response_body(&after.await.unwrap()), "after");
    assert_eq!(server.accepted_carriers(), 2);

    handle.shutdown_and_wait().await;
    server.abort();
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
        "/app/devices/ingest",
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
    assert!(received_text.starts_with("POST /app/devices/ingest HTTP/1.1\r\n"));
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
async fn reachable_lan_success_never_dials_relay() {
    let (cert, key) = self_signed();
    let pin = observer_pl::ca::sha256(cert.as_ref())[..16].to_vec();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_empty_segments(listener, acceptor));
    let (origin, relay_accepts, relay_task) = spawn_counting_relay().await;
    let client =
        ObserverClient::new(observer_relay_credential(pin, port, origin, "old-token")).unwrap();

    client.list_segments("20260729").await.unwrap();

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
    let client =
        ObserverClient::new(observer_relay_credential(pin, port, origin, "old-token")).unwrap();

    let err = client.list_segments("20260729").await.unwrap_err();

    assert!(matches!(err, TransportError::Rejected { status: 503, .. }));
    let _ = server.await.unwrap();
    assert_eq!(relay_accepts.load(Ordering::SeqCst), 0);
    relay_task.abort();
}

#[tokio::test]
async fn lan_only_no_endpoint_still_returns_no_endpoint() {
    let mut credential = observer_credential(vec![0; 16], 7657);
    credential.endpoints.clear();
    let client = ObserverClient::new(credential).unwrap();

    let err = client.list_segments("20260729").await.unwrap_err();

    assert!(matches!(err, TransportError::NoEndpoint));
}

#[tokio::test]
async fn transient_lan_fault_then_success_absorbed_before_relay() {
    let (cert, key) = self_signed();
    let pin = observer_pl::ca::sha256(cert.as_ref())[..16].to_vec();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_drop_then_empty_segments(listener, acceptor));
    let (origin, relay_accepts, relay_task) = spawn_counting_relay().await;
    let client =
        ObserverClient::new(observer_relay_credential(pin, port, origin, "old-token")).unwrap();

    client.list_segments("20260729").await.unwrap();

    let _ = server.await.unwrap();
    assert_eq!(relay_accepts.load(Ordering::SeqCst), 0);
    relay_task.abort();
}
