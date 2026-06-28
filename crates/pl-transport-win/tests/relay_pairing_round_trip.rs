// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use observer_pl::frame::{Frame, FrameDecoder, FLAG_CLOSE, FLAG_DATA};
use observer_pl::pairlink::RelayPairLink;
use observer_pl::wire::PairRequest;
use pl_transport_win::credential::EndpointAddr;
use pl_transport_win::relay_pairing::pair_over_relay;
use pl_transport_win::relay_token::{refresh_device_token, RefreshOutcome};
use pl_transport_win::{transport_error_code, RelayControlEndpoint, TransportError};
use rcgen::{
    BasicConstraints, CertificateParams, CertificateSigningRequestParams, ExtendedKeyUsagePurpose,
    IsCa, KeyPair, KeyUsagePurpose, PKCS_ECDSA_P256_SHA256,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::ServerConfig;
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{accept_async, WebSocketStream};

const INSTANCE_ID: &str = "12345678-1234-5678-1234-567812345678";
const NONCE_HEX: &str = "0123456789abcdef0123456789abcdef";
const CURRENT_TOKEN: &str = "e30.eyJpYXQiOjEwMCwiZXhwIjoyMDB9.sig";
const NEW_TOKEN: &str = "e30.eyJpYXQiOjMwMCwiZXhwIjo0MDB9.sig";
const ENROLL_TOKEN: &str = "e30.eyJpYXQiOjEwMCwiZXhwIjo5OTk5OTk5OTk5fQ.sig";

struct TestCa {
    cert: rcgen::Certificate,
    key: KeyPair,
}

impl TestCa {
    fn new() -> Self {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages.push(KeyUsagePurpose::DigitalSignature);
        params.key_usages.push(KeyUsagePurpose::KeyCertSign);
        params.key_usages.push(KeyUsagePurpose::CrlSign);
        let cert = params.self_signed(&key).unwrap();
        Self { cert, key }
    }

    fn spki_pin(&self) -> Vec<u8> {
        let spki = observer_pl::ca::extract_spki_der(self.cert.der()).unwrap();
        observer_pl::ca::sha256(&spki)[..16].to_vec()
    }

    fn cert_der_pin(&self) -> Vec<u8> {
        observer_pl::ca::sha256(self.cert.der())[..16].to_vec()
    }
}

fn leaf_config(signer: &TestCa) -> ServerConfig {
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
    let mut params = CertificateParams::new(vec!["spl.local".to_string()]).unwrap();
    params.is_ca = IsCa::NoCa;
    params
        .extended_key_usages
        .push(ExtendedKeyUsagePurpose::ServerAuth);
    let cert = params.signed_by(&key, &signer.cert, &signer.key).unwrap();
    ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(cert.der().to_vec())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
        )
        .unwrap()
}

#[derive(Clone)]
enum HomeMode {
    Ok,
    InnerGone,
    MissingHomeAttestation,
}

struct MockState {
    json_ca: Arc<TestCa>,
    tls_signer: Arc<TestCa>,
    home_mode: HomeMode,
    enroll_status: Mutex<Option<u16>>,
    refresh_status: Mutex<Option<u16>>,
    refresh_hits: AtomicUsize,
}

impl MockState {
    fn normal() -> Self {
        let ca = Arc::new(TestCa::new());
        Self {
            json_ca: ca,
            tls_signer: Arc::new(TestCa::new()),
            home_mode: HomeMode::Ok,
            enroll_status: Mutex::new(None),
            refresh_status: Mutex::new(None),
            refresh_hits: AtomicUsize::new(0),
        }
    }

    fn with_same_tls_ca(mut self) -> Self {
        self.tls_signer = self.json_ca.clone();
        self
    }
}

fn relay_link(origin: String, ca_fp_spki: Vec<u8>) -> RelayPairLink {
    RelayPairLink {
        instance_id: INSTANCE_ID.to_string(),
        totp: "123456".to_string(),
        nonce_hex: NONCE_HEX.to_string(),
        ca_fp_spki,
        relay_origin: origin,
    }
}

async fn spawn_mock_relay(state: Arc<MockState>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                break;
            };
            let state = state.clone();
            tokio::spawn(async move {
                let _ = handle_connection(tcp, state).await;
            });
        }
    });
    origin
}

async fn handle_connection(tcp: TcpStream, state: Arc<MockState>) -> io::Result<()> {
    let mut peek = [0u8; 512];
    let n = tcp.peek(&mut peek).await?;
    if String::from_utf8_lossy(&peek[..n]).starts_with("GET ") {
        handle_ws(tcp, state).await
    } else {
        handle_http(tcp, state).await
    }
}

async fn handle_ws(tcp: TcpStream, state: Arc<MockState>) -> io::Result<()> {
    let ws = accept_async(tcp).await.map_err(io::Error::other)?;
    let (relay_side, home_side) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        let _ = pump_ws(ws, relay_side).await;
    });
    serve_home_pair(home_side, state).await
}

async fn pump_ws(ws: WebSocketStream<TcpStream>, relay_side: DuplexStream) -> io::Result<()> {
    let (mut ws_sink, mut ws_stream) = ws.split();
    let (mut relay_read, mut relay_write) = tokio::io::split(relay_side);

    let to_inner = async move {
        while let Some(message) = ws_stream.next().await {
            match message.map_err(io::Error::other)? {
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
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "bad ws message"));
                }
            }
        }
        Ok(())
    };

    let to_ws = async move {
        let mut buf = [0u8; 4096];
        loop {
            let n = relay_read.read(&mut buf).await?;
            if n == 0 {
                let _ = ws_sink.close().await;
                return Ok(());
            }
            ws_sink
                .send(Message::Binary(buf[..n].to_vec().into()))
                .await
                .map_err(io::Error::other)?;
        }
    };

    tokio::select! {
        result = to_inner => result,
        result = to_ws => result,
    }
}

async fn handle_http(mut tcp: TcpStream, state: Arc<MockState>) -> io::Result<()> {
    let raw = read_http_request(&mut tcp).await?;
    let text = String::from_utf8_lossy(&raw);
    let path = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");

    if path.starts_with("/session/pair-ticket") {
        write_json(&mut tcp, 200, json!({"pair_ticket":"pair-ticket"})).await?;
    } else if path == "/enroll/device" {
        let status = *state.enroll_status.lock().unwrap();
        match status {
            Some(status) => write_json(&mut tcp, status, json!({"error":"rejected"})).await?,
            None => write_json(&mut tcp, 200, json!({"device_token":ENROLL_TOKEN})).await?,
        }
    } else if path == "/token/refresh" {
        state.refresh_hits.fetch_add(1, Ordering::SeqCst);
        let status = *state.refresh_status.lock().unwrap();
        match status {
            Some(401) => write_json(&mut tcp, 401, json!({"reason":"expired"})).await?,
            Some(status) => write_json(&mut tcp, status, json!({"error":"rejected"})).await?,
            None => write_json(&mut tcp, 200, json!({"device_token":NEW_TOKEN})).await?,
        }
    } else {
        write_json(&mut tcp, 404, json!({"error":"not_found"})).await?;
    }
    let _ = tcp.shutdown().await;
    Ok(())
}

async fn read_http_request<S>(stream: &mut S) -> io::Result<Vec<u8>>
where
    S: AsyncRead + Unpin,
{
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            return Ok(raw);
        }
        raw.extend_from_slice(&buf[..n]);
        if request_complete(&raw) {
            return Ok(raw);
        }
    }
}

fn request_complete(raw: &[u8]) -> bool {
    let Some(split) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
        return false;
    };
    let head = String::from_utf8_lossy(&raw[..split]);
    let len = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    raw.len() >= split + 4 + len
}

async fn write_json<S>(stream: &mut S, status: u16, body: serde_json::Value) -> io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    let body = body.to_string();
    let reason = if status == 200 { "OK" } else { "ERR" };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await
}

async fn serve_home_pair(stream: DuplexStream, state: Arc<MockState>) -> io::Result<()> {
    let acceptor = TlsAcceptor::from(Arc::new(leaf_config(state.tls_signer.as_ref())));
    let mut tls = acceptor.accept(stream).await.map_err(io::Error::other)?;
    let request = read_pl_request(&mut tls).await?;

    if matches!(state.home_mode, HomeMode::InnerGone) {
        write_pl_response(&mut tls, 410, json!({"error":"gone"})).await?;
        return Ok(());
    }

    let request_text = String::from_utf8_lossy(&request);
    assert!(request_text.starts_with("POST /app/network/pair?token="));
    let body = request
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|split| &request[split + 4..])
        .unwrap();
    let pair_request: PairRequest = serde_json::from_slice(body).unwrap();
    let csr = CertificateSigningRequestParams::from_pem(&pair_request.csr).unwrap();
    let client_cert = csr
        .signed_by(&state.json_ca.cert, &state.json_ca.key)
        .unwrap();
    let fingerprint = format!("sha256:{}", observer_pl::ca::sha256_hex(client_cert.der()));

    let mut response = json!({
        "client_cert": client_cert.pem(),
        "ca_chain": [state.json_ca.cert.pem()],
        "instance_id": INSTANCE_ID,
        "home_label": "Home",
        "fingerprint": fingerprint,
        "local_endpoints": [{"ip":"10.0.0.2","port":7657,"scope":"lan"}]
    });
    if !matches!(state.home_mode, HomeMode::MissingHomeAttestation) {
        response["home_attestation"] = json!("attestation");
    }
    write_pl_response(&mut tls, 200, response).await
}

async fn read_pl_request<S>(tls: &mut S) -> io::Result<Vec<u8>>
where
    S: AsyncRead + Unpin,
{
    let mut decoder = FrameDecoder::new();
    let mut request = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = tls.read(&mut buf).await?;
        if n == 0 {
            return Ok(request);
        }
        decoder.feed(&buf[..n]);
        for frame in decoder.drain().unwrap() {
            if frame.flags & FLAG_DATA != 0 {
                request.extend_from_slice(&frame.payload);
            }
            if frame.flags & FLAG_CLOSE != 0 {
                return Ok(request);
            }
        }
    }
}

async fn write_pl_response<S>(tls: &mut S, status: u16, body: serde_json::Value) -> io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    let body = body.to_string();
    let status_text = if status == 200 { "OK" } else { "ERR" };
    let response = format!(
        "HTTP/1.1 {status} {status_text}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    );
    let frame = Frame::new(1, FLAG_DATA | FLAG_CLOSE, response.into_bytes());
    tls.write_all(&frame.encode().unwrap()).await?;
    tls.flush().await?;
    let _ = tls.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn relay_pairing_full_ceremony_populates_credential() {
    let state = Arc::new(MockState::normal().with_same_tls_ca());
    let origin = spawn_mock_relay(state.clone()).await;
    let link = relay_link(origin.clone(), state.json_ca.spki_pin());

    let credential = pair_over_relay(&link, "win-test").await.unwrap();

    assert_eq!(credential.relay_origin.as_deref(), Some(origin.as_str()));
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
