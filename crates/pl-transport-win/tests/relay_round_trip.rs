// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! End-to-end SPL relay carrier tests over a real loopback WebSocket relay.
//!
//! The loopback relay intentionally uses `ws://127.0.0.1` rather than WSS, so it
//! does not exercise the production outer WebPKI leg. It is otherwise a dumb
//! opaque pump: WS binary frames are bridged to an in-memory byte stream whose
//! far side runs the same pinned inner TLS + PL framing server used by the LAN
//! round-trip tests.

mod support;

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use observer_model::TransportPath;
use observer_pl::frame::{
    Frame, FrameDecoder, FLAG_CLOSE, FLAG_DATA, FLAG_RESET, FLAG_WINDOW, RECOMMENDED_CHUNK,
};
use observer_pl::http::{self, HttpResponse};
use observer_pl::ingest::{FilePart, IngestStatus};
use observer_pl::mux::INITIAL_WINDOW;
use pl_transport_win::client::ObserverClient;
use pl_transport_win::credential::{Credential, EndpointAddr, PairedState};
use pl_transport_win::journal_bridge;
use pl_transport_win::observe::{DialCounts, OperationObserver};
use pl_transport_win::relay::{dial_relay_ws, request_once_over_ws, request_once_relay};
use pl_transport_win::tls::pairing_config;
use pl_transport_win::{transport_error_code, RelayError, TransportError};
use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};
use rustls::ClientConfig;
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{accept_async, accept_hdr_async, WebSocketStream};

use support::journal_fake::{self_signed, server_config};
use support::log_capture::CapturingSubscriber;
use support::observer_contract::{
    fixture as authority_fixture, v3_read_capture_matches, v3_upload_capture_matches,
    vector as authority_vector,
};

const RELAY_TOKEN: &str = "test-device-token";
const INSTANCE_ID: &str = "12345678-1234-5678-1234-567812345678";
const CARRIER_READ_BUF_BYTES: usize = 64 * 1024;
const LARGE_WS_FRAME_BYTES: usize = 256 * 1024;
const LARGE_RESPONSE_BYTES: usize = 512 * 1024 + 137;
const OBSERVER_CLIENT_PRODUCTION_DIAL_ATTEMPTS: u64 = 6;
static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
struct RelayRequestArtifacts {
    request_bytes: Vec<u8>,
    head_boundary_found: bool,
    saw_request_close: bool,
    sent_terminal_response: bool,
    tcp_accepts: usize,
    ws_dials: usize,
    inner_requests: usize,
}

fn client_config(pin: &[u8]) -> Arc<ClientConfig> {
    Arc::new(pairing_config(pin).unwrap())
}

fn unused_outer_config() -> Arc<ClientConfig> {
    client_config(&[0xAA; 16])
}

fn epoch_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn base64url_no_pad(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    let mut index = 0;
    while index + 3 <= input.len() {
        let chunk = ((input[index] as u32) << 16)
            | ((input[index + 1] as u32) << 8)
            | input[index + 2] as u32;
        out.push(TABLE[((chunk >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((chunk >> 12) & 0x3F) as usize] as char);
        out.push(TABLE[((chunk >> 6) & 0x3F) as usize] as char);
        out.push(TABLE[(chunk & 0x3F) as usize] as char);
        index += 3;
    }
    match input.len() - index {
        1 => {
            let chunk = (input[index] as u32) << 16;
            out.push(TABLE[((chunk >> 18) & 0x3F) as usize] as char);
            out.push(TABLE[((chunk >> 12) & 0x3F) as usize] as char);
        }
        2 => {
            let chunk = ((input[index] as u32) << 16) | ((input[index + 1] as u32) << 8);
            out.push(TABLE[((chunk >> 18) & 0x3F) as usize] as char);
            out.push(TABLE[((chunk >> 12) & 0x3F) as usize] as char);
            out.push(TABLE[((chunk >> 6) & 0x3F) as usize] as char);
        }
        _ => {}
    }
    out
}

fn mint_jwt(iat: i64, exp: i64) -> String {
    let payload = format!(r#"{{"iat":{iat},"exp":{exp}}}"#);
    format!(
        "{}.{}.sig",
        base64url_no_pad(b"{}"),
        base64url_no_pad(payload.as_bytes())
    )
}

fn observer_relay_credential(
    pin: Vec<u8>,
    lan_port: u16,
    relay_origin: String,
    token: String,
) -> Credential {
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
    let params = CertificateParams::new(vec!["observer.test".to_string()]).unwrap();
    let cert = params.self_signed(&key).unwrap();
    let expires_at = observer_pl::jwt::decode_unverified_claims(&token).map(|claims| claims.exp);
    Credential {
        client_key_pem: key.serialize_pem(),
        client_cert_pem: cert.pem(),
        ca_chain_pem: vec![cert.pem()],
        ca_fp_prefix: pin,
        instance_id: INSTANCE_ID.into(),
        home_label: "Home".into(),
        endpoints: vec![EndpointAddr {
            host: "127.0.0.1".into(),
            port: lan_port,
        }],
        relay_origin: Some(relay_origin),
        device_token: Some(token),
        device_token_expires_at: expires_at,
    }
}

fn temp_pairing_path(name: &str) -> PathBuf {
    let unique = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!("plw-{name}-{}-{unique}.json", std::process::id()))
}

fn relay_client(credential: Credential) -> ObserverClient {
    ObserverClient::new(credential).unwrap()
}

async fn relay_probe(client: &ObserverClient) -> Result<(), TransportError> {
    client
        .ingest(
            "143000_300",
            "20260729",
            vec![FilePart {
                filename: "probe.bin".into(),
                content_type: "application/octet-stream".into(),
                bytes: vec![1],
            }],
        )
        .await
        .map(|_| ())
}

fn relay_bridge_state(credential: Credential) -> PairedState {
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

fn loopback_host(port: u16) -> String {
    format!("127.0.0.1:{port}")
}

fn cap_cookie(cap: &str) -> String {
    format!("{}={cap}", observer_pl::bridge::CAP_COOKIE_NAME)
}

async fn raw_bridge_request(port: u16, target: &str, cap: &str) -> Vec<u8> {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let request = format!(
        "GET {target} HTTP/1.1\r\nHost: {}\r\nCookie: {}\r\n\r\n",
        loopback_host(port),
        cap_cookie(cap)
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    stream.flush().await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    response
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
    response_text(response)
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap()
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

/// Pump one opaque relay stream until the inner server requests a terminal close.
///
/// The trigger and acknowledgement are both load-bearing: the inner server keeps
/// its TLS/duplex alive until the pump confirms that close 4402 was flushed. If it
/// dropped the duplex first, an EOF could win the race, map to `Abnormal`, and make
/// the client retry five times against this deliberately one-shot fixture.
async fn pump_ws_with_terminal_close(
    ws: WebSocketStream<TcpStream>,
    relay_side: DuplexStream,
    mut close_trigger: oneshot::Receiver<u16>,
    close_sent: oneshot::Sender<()>,
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

    let to_ws_or_close = async move {
        let mut buf = [0u8; 512];
        loop {
            tokio::select! {
                read = relay_read.read(&mut buf) => {
                    let n = read?;
                    if n == 0 {
                        let _ = ws_sink.close().await;
                        return Ok(());
                    }
                    ws_sink
                        .send(Message::Binary(buf[..n].to_vec().into()))
                        .await
                        .map_err(|_| {
                            io::Error::new(
                                io::ErrorKind::BrokenPipe,
                                "relay websocket send failed",
                            )
                        })?;
                }
                code = &mut close_trigger => {
                    let code = code.map_err(|_| {
                        io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "relay terminal-close trigger dropped",
                        )
                    })?;
                    ws_sink
                        .send(Message::Close(Some(CloseFrame {
                            code: CloseCode::from(code),
                            reason: "".into(),
                        })))
                        .await
                        .map_err(io::Error::other)?;
                    let _ = close_sent.send(());
                    return Ok(());
                }
            }
        }
    };

    tokio::select! {
        result = to_inner => result,
        result = to_ws_or_close => result,
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

async fn accept_counted_relay_stream(
    listener: TcpListener,
    duplex_capacity: usize,
) -> (DuplexStream, usize, usize) {
    let (tcp, _) = listener.accept().await.unwrap();
    let tcp_accepts = 1;
    let ws = accept_async(tcp).await.unwrap();
    let ws_dials = 1;
    let (relay_side, server_side) = tokio::io::duplex(duplex_capacity);
    tokio::spawn(async move {
        let _ = pump_ws(ws, relay_side, None).await;
    });
    (server_side, tcp_accepts, ws_dials)
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

async fn serve_stream_with_flow_control<S>(
    stream: S,
    acceptor: TlsAcceptor,
) -> RelayRequestArtifacts
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
                    return RelayRequestArtifacts {
                        head_boundary_found: request_head_len(&request).is_some(),
                        request_bytes: request,
                        saw_request_close: closed,
                        sent_terminal_response: false,
                        tcp_accepts: 0,
                        ws_dials: 0,
                        inner_requests: 1,
                    };
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

    let body = b"{\"status\":\"ok\"}";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        String::from_utf8_lossy(body)
    );
    let frame = Frame::new(stream_id, FLAG_DATA | FLAG_CLOSE, response.into_bytes());
    tls.write_all(&frame.encode().unwrap()).await.unwrap();
    tls.flush().await.unwrap();
    let sent_terminal_response = true;
    let _ = tls.shutdown().await;
    RelayRequestArtifacts {
        head_boundary_found: request_head_len(&request).is_some(),
        request_bytes: request,
        saw_request_close: closed,
        sent_terminal_response,
        tcp_accepts: 0,
        ws_dials: 0,
        inner_requests: 1,
    }
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

async fn spawn_flow_relay(acceptor: TlsAcceptor) -> (String, JoinHandle<RelayRequestArtifacts>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (stream, tcp_accepts, ws_dials) = accept_counted_relay_stream(listener, 1024).await;
        let mut artifacts = serve_stream_with_flow_control(stream, acceptor).await;
        artifacts.tcp_accepts = tcp_accepts;
        artifacts.ws_dials = ws_dials;
        artifacts
    });
    (origin, task)
}

async fn serve_stream_until_terminal_close<S>(
    stream: S,
    acceptor: TlsAcceptor,
    close_trigger: oneshot::Sender<u16>,
    close_sent: oneshot::Receiver<()>,
) -> RelayRequestArtifacts
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut tls = acceptor.accept(stream).await.unwrap();
    let mut decoder = FrameDecoder::new();
    let mut request = Vec::new();
    let mut saw_request_close = false;
    let mut buf = [0u8; 16 * 1024];

    while request.len() < INITIAL_WINDOW && !saw_request_close {
        let n = tls.read(&mut buf).await.unwrap();
        if n == 0 {
            break;
        }
        decoder.feed(&buf[..n]);
        for frame in decoder.drain().unwrap() {
            if frame.flags & FLAG_DATA != 0 {
                request.extend_from_slice(&frame.payload);
            }
            if frame.flags & FLAG_CLOSE != 0 {
                saw_request_close = true;
            }
        }
    }

    let head_len = request_head_len(&request);
    assert_eq!(
        request.len(),
        INITIAL_WINDOW,
        "client must consume exactly its initial send window before interruption"
    );
    assert!(
        head_len.is_some_and(|len| request.len() > len),
        "fixture must receive at least one request-body byte before interruption"
    );
    assert!(
        !saw_request_close,
        "fixture must interrupt before the request CLOSE"
    );

    close_trigger.send(4402).unwrap();
    close_sent
        .await
        .expect("relay pump must flush close 4402 before inner TLS drops");

    RelayRequestArtifacts {
        request_bytes: request,
        head_boundary_found: head_len.is_some(),
        saw_request_close,
        sent_terminal_response: false,
        tcp_accepts: 0,
        ws_dials: 0,
        inner_requests: 1,
    }
}

async fn spawn_interrupted_flow_relay(
    acceptor: TlsAcceptor,
) -> (String, JoinHandle<RelayRequestArtifacts>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let tcp_accepts = 1;
        let ws = accept_async(tcp).await.unwrap();
        let ws_dials = 1;
        let (relay_side, server_side) = tokio::io::duplex(1024);
        let (close_trigger_tx, close_trigger_rx) = oneshot::channel();
        let (close_sent_tx, close_sent_rx) = oneshot::channel();
        let pump = tokio::spawn(async move {
            pump_ws_with_terminal_close(ws, relay_side, close_trigger_rx, close_sent_tx).await
        });

        let mut artifacts = serve_stream_until_terminal_close(
            server_side,
            acceptor,
            close_trigger_tx,
            close_sent_rx,
        )
        .await;
        artifacts.tcp_accepts = tcp_accepts;
        artifacts.ws_dials = ws_dials;
        pump.await.unwrap().unwrap();
        artifacts
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

fn tls_pair_with_pin() -> (Vec<u8>, TlsAcceptor) {
    let (cert, key) = self_signed();
    let pin = observer_pl::ca::sha256(cert.as_ref())[..16].to_vec();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    (pin, acceptor)
}

fn tls_pair() -> (Arc<ClientConfig>, TlsAcceptor) {
    let (pin, acceptor) = tls_pair_with_pin();
    (client_config(&pin), acceptor)
}

fn assert_relay_error<T>(result: Result<T, TransportError>, expected: RelayError) {
    match result {
        Err(TransportError::Relay(actual)) => assert_eq!(actual, expected),
        Err(other) => panic!("expected relay error {expected:?}, got {other:?}"),
        Ok(_) => panic!("expected relay error {expected:?}, got success"),
    }
}

#[derive(Clone, Copy)]
enum CombinedWsMode {
    AcceptAny,
    FreshOnly,
    AlwaysUnauthorized,
    Close(u16),
    UpgradeReject(u16),
}

struct CombinedRelayState {
    acceptor: TlsAcceptor,
    mode: CombinedWsMode,
    fresh_token: String,
    tcp_accepts: AtomicUsize,
    ws_dials: AtomicUsize,
    refreshes: AtomicUsize,
    auth_headers: Mutex<Vec<String>>,
    inner_requests: Mutex<Vec<Vec<u8>>>,
}

struct CombinedRelay {
    origin: String,
    state: Arc<CombinedRelayState>,
    task: JoinHandle<()>,
}

impl CombinedRelay {
    fn abort(&self) {
        self.task.abort();
    }
}

async fn spawn_combined_relay(
    acceptor: TlsAcceptor,
    mode: CombinedWsMode,
    fresh_token: String,
) -> CombinedRelay {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let state = Arc::new(CombinedRelayState {
        acceptor,
        mode,
        fresh_token,
        tcp_accepts: AtomicUsize::new(0),
        ws_dials: AtomicUsize::new(0),
        refreshes: AtomicUsize::new(0),
        auth_headers: Mutex::new(Vec::new()),
        inner_requests: Mutex::new(Vec::new()),
    });
    let task = tokio::spawn({
        let state = state.clone();
        async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    break;
                };
                state.tcp_accepts.fetch_add(1, Ordering::SeqCst);
                let state = state.clone();
                tokio::spawn(async move {
                    let _ = handle_combined_connection(tcp, state).await;
                });
            }
        }
    });
    CombinedRelay {
        origin,
        state,
        task,
    }
}

async fn handle_combined_connection(
    tcp: TcpStream,
    state: Arc<CombinedRelayState>,
) -> io::Result<()> {
    let mut peek = [0u8; 512];
    let n = tcp.peek(&mut peek).await?;
    if String::from_utf8_lossy(&peek[..n]).starts_with("GET ") {
        handle_combined_ws(tcp, state).await
    } else {
        handle_combined_http(tcp, state).await
    }
}

#[allow(clippy::result_large_err)]
async fn handle_combined_ws(tcp: TcpStream, state: Arc<CombinedRelayState>) -> io::Result<()> {
    let seen_auth = Arc::new(Mutex::new(String::new()));
    let seen_auth_for_cb = seen_auth.clone();
    let state_for_cb = state.clone();
    let mode = state.mode;
    let result = accept_hdr_async(tcp, move |request: &Request, response: Response| {
        state_for_cb.ws_dials.fetch_add(1, Ordering::SeqCst);
        let path_ok = request.uri().path() == "/session/dial"
            && request
                .uri()
                .query()
                .map(|query| query.contains(INSTANCE_ID))
                .unwrap_or(false);
        let auth = request
            .headers()
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        *seen_auth_for_cb.lock().unwrap() = auth.clone();
        state_for_cb.auth_headers.lock().unwrap().push(auth);
        if !path_ok {
            let response: ErrorResponse = tokio_tungstenite::tungstenite::http::Response::builder()
                .status(400)
                .body(Some("bad path".to_string()))
                .unwrap();
            return Err(response);
        }
        if let CombinedWsMode::UpgradeReject(status) = mode {
            let response: ErrorResponse = tokio_tungstenite::tungstenite::http::Response::builder()
                .status(status)
                .body(Some("rejected".to_string()))
                .unwrap();
            return Err(response);
        }
        Ok(response)
    })
    .await;

    let mut ws = match result {
        Ok(ws) => ws,
        Err(_) => return Ok(()),
    };
    match mode {
        CombinedWsMode::AcceptAny => {}
        CombinedWsMode::FreshOnly => {
            let expected = format!("Bearer {}", state.fresh_token);
            if *seen_auth.lock().unwrap() != expected {
                ws.send(Message::Close(Some(CloseFrame {
                    code: CloseCode::from(4401),
                    reason: "".into(),
                })))
                .await
                .map_err(io::Error::other)?;
                return Ok(());
            }
        }
        CombinedWsMode::AlwaysUnauthorized => {
            ws.send(Message::Close(Some(CloseFrame {
                code: CloseCode::from(4401),
                reason: "".into(),
            })))
            .await
            .map_err(io::Error::other)?;
            return Ok(());
        }
        CombinedWsMode::Close(code) => {
            ws.send(Message::Close(Some(CloseFrame {
                code: CloseCode::from(code),
                reason: "".into(),
            })))
            .await
            .map_err(io::Error::other)?;
            return Ok(());
        }
        CombinedWsMode::UpgradeReject(_) => return Ok(()),
    }

    let (relay_side, server_side) = tokio::io::duplex(4096);
    tokio::spawn(async move {
        let _ = pump_ws(ws, relay_side, None).await;
    });
    let request = serve_stream_response(
        server_side,
        state.acceptor.clone(),
        "200 OK",
        b"{\"status\":\"ok\"}",
    )
    .await;
    state.inner_requests.lock().unwrap().push(request);
    Ok(())
}

async fn handle_combined_http(
    mut tcp: TcpStream,
    state: Arc<CombinedRelayState>,
) -> io::Result<()> {
    let raw = read_http_request(&mut tcp).await?;
    let text = String::from_utf8_lossy(&raw);
    let path = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    if path == "/token/refresh" {
        state.refreshes.fetch_add(1, Ordering::SeqCst);
        write_json(
            &mut tcp,
            200,
            json!({
                "device_token": state.fresh_token,
            }),
        )
        .await?;
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

fn request_head_len(raw: &[u8]) -> Option<usize> {
    raw.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
}

fn request_complete(raw: &[u8]) -> bool {
    let Some(head_len) = request_head_len(raw) else {
        return false;
    };
    let head = String::from_utf8_lossy(&raw[..head_len - 4]);
    let len = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    raw.len() >= head_len + len
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

fn rebuild_captured_request(raw: &[u8]) -> Vec<u8> {
    let head_len = request_head_len(raw).expect("captured request must contain an HTTP head");
    let head = std::str::from_utf8(&raw[..head_len - 4]).unwrap();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap();
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().unwrap();
    let path = request_parts.next().unwrap();
    assert_eq!(request_parts.next(), Some("HTTP/1.1"));
    assert_eq!(request_parts.next(), None);
    let headers = lines
        .map(|line| {
            let (name, value) = line.split_once(':').unwrap();
            (name.to_string(), value.trim_start().to_string())
        })
        .collect::<Vec<_>>();
    http::build_request(method, path, &headers, &raw[head_len..])
}

fn full_request_len_from_captured_head(raw: &[u8]) -> usize {
    let head_len = request_head_len(raw).expect("captured request must contain an HTTP head");
    let head = std::str::from_utf8(&raw[..head_len - 4]).unwrap();
    let content_length = head
        .split("\r\n")
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .expect("captured request must contain content-length");
    head_len + content_length
}

#[derive(Debug, PartialEq, Eq)]
struct CompleteClientOutcome {
    status: IngestStatus,
    path: TransportPath,
    attempts: u32,
}

#[derive(Debug, PartialEq, Eq)]
enum InterruptedClientOutcome {
    RelayUnpaid,
}

fn production_ingest_files() -> Vec<FilePart> {
    vec![FilePart {
        filename: "display_1_screen.mp4".into(),
        content_type: "video/mp4".into(),
        bytes: deterministic_bytes(INITIAL_WINDOW + RECOMMENDED_CHUNK + 17),
    }]
}

async fn relay_client_with_response(
    status: &'static str,
    body: &'static [u8],
) -> (ObserverClient, JoinHandle<Vec<u8>>) {
    let (pin, acceptor) = tls_pair_with_pin();
    let (origin, server) = spawn_response_relay(acceptor, status, body, None, 4096).await;
    let token = mint_jwt(epoch_secs(), epoch_secs() + 10_000);
    let client = ObserverClient::new(observer_relay_credential(pin, 9, origin, token)).unwrap();
    (client, server)
}

async fn run_complete_observer_ingest(
    observer: Option<Arc<OperationObserver>>,
    token: String,
) -> (RelayRequestArtifacts, CompleteClientOutcome) {
    let (pin, acceptor) = tls_pair_with_pin();
    let (origin, server) = spawn_flow_relay(acceptor).await;
    let client =
        relay_client(observer_relay_credential(pin, 9, origin, token)).with_observer(observer);
    let (response, metadata) = client
        .ingest("143000_300", "20260729", production_ingest_files())
        .await
        .unwrap();
    let artifacts = server.await.unwrap();
    (
        artifacts,
        CompleteClientOutcome {
            status: response.status,
            path: metadata.path,
            attempts: metadata.attempts,
        },
    )
}

async fn run_interrupted_observer_ingest(
    observer: Option<Arc<OperationObserver>>,
    token: String,
) -> (RelayRequestArtifacts, InterruptedClientOutcome) {
    let (pin, acceptor) = tls_pair_with_pin();
    let (origin, server) = spawn_interrupted_flow_relay(acceptor).await;
    let client =
        relay_client(observer_relay_credential(pin, 9, origin, token)).with_observer(observer);
    let error = client
        .ingest("143000_300", "20260729", production_ingest_files())
        .await
        .unwrap_err();
    let outcome = match error {
        TransportError::Relay(RelayError::Unpaid) => InterruptedClientOutcome::RelayUnpaid,
        other => panic!("expected the terminal relay unpaid error, got {other:?}"),
    };
    let artifacts = server.await.unwrap();
    (artifacts, outcome)
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
        "/transport/small",
        &[("Content-Type".to_string(), "application/json".to_string())],
        b"{\"device\":\"win\"}",
    )
    .await
    .expect("relay request should round-trip");

    assert_eq!(response.status, 200);
    assert_eq!(response.body_text(), "{\"status\":\"ok\"}");
    let received = server.await.unwrap();
    let received_text = String::from_utf8_lossy(&received);
    assert!(received_text.starts_with("POST /transport/small HTTP/1.1\r\n"));
    assert!(received_text.ends_with("{\"device\":\"win\"}"));
}

#[tokio::test]
async fn observer_contract_authority_relay_v3_operations_have_identical_mtls_only_policy() {
    let day = "20260820";
    let segment = "080000_600";
    let filenames = ["screen.mp4", "audio.flac"];
    let (client, server) =
        relay_client_with_response("200 OK", br#"{"status":"ok","segment":"080000_600"}"#).await;
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

    let (client, server) =
        relay_client_with_response("200 OK", br#"{"days":{"20260820":{"segments":1}}}"#).await;
    client.ingest_manifest().await.unwrap();
    assert!(v3_read_capture_matches(
        &server.await.unwrap(),
        "GET",
        "/app/devices/ingest/manifest"
    ));

    let (client, server) =
        relay_client_with_response("200 OK", br#"{"version":1,"day":"20260820","segments":{}}"#)
            .await;
    client.ingest_manifest_day(day).await.unwrap();
    assert!(v3_read_capture_matches(
        &server.await.unwrap(),
        "GET",
        "/app/devices/ingest/manifest/20260820"
    ));

    let (client, server) =
        relay_client_with_response("200 OK", br#"{"items":[],"total":0,"protocol_version":3}"#)
            .await;
    client.list_segments(day).await.unwrap();
    assert!(v3_read_capture_matches(
        &server.await.unwrap(),
        "GET",
        "/app/devices/ingest/segments/20260820"
    ));
}

#[tokio::test]
async fn observer_contract_authority_relay_status_vectors_use_documented_http_mapping() {
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
        let (client, server) = relay_client_with_response(
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
        assert!(v3_upload_capture_matches(
            &server.await.unwrap(),
            "20260820",
            "080000_600",
            &["status.wav"]
        ));
    }
}

#[tokio::test]
async fn observer_contract_authority_relay_v3_reads_fail_closed_on_non_success() {
    for status in [403, 500] {
        let text: &'static str = Box::leak(format!("{status} Rejected").into_boxed_str());
        let (client, server) = relay_client_with_response(text, br#"{}"#).await;
        assert!(matches!(
            client.ingest_manifest().await,
            Err(TransportError::Rejected { status: actual, .. }) if actual == status
        ));
        assert!(v3_read_capture_matches(
            &server.await.unwrap(),
            "GET",
            "/app/devices/ingest/manifest"
        ));

        let (client, server) = relay_client_with_response(text, br#"{}"#).await;
        assert!(matches!(
            client.ingest_manifest_day("20260820").await,
            Err(TransportError::Rejected { status: actual, .. }) if actual == status
        ));
        assert!(v3_read_capture_matches(
            &server.await.unwrap(),
            "GET",
            "/app/devices/ingest/manifest/20260820"
        ));

        let (client, server) = relay_client_with_response(text, br#"{}"#).await;
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
        "/app/devices/ingest",
        &[(
            "Content-Type".to_string(),
            "application/octet-stream".to_string(),
        )],
        &big_body,
    )
    .await
    .expect("relay must preserve the windowed upload over partial byte reads");

    assert_eq!(response.status, 200);
    assert_eq!(response.body_text(), "{\"status\":\"ok\"}");
    let artifacts = server.await.unwrap();
    let received = artifacts.request_bytes;
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
        "/large-response",
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
    assert!(received_text.starts_with("GET /large-response HTTP/1.1\r\n"));
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
        "/transport/blind",
        &[("X-Private-Probe".to_string(), "private-value".to_string())],
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
    assert!(!contains_bytes(&captured, b"X-Private-Probe"));
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
        "/app/devices/ingest",
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

#[tokio::test]
async fn relay_fallbacks_after_lan_unreachable() {
    let (pin, acceptor) = tls_pair_with_pin();
    let now = epoch_secs();
    let token = mint_jwt(now, now + 10_000);
    let relay = spawn_combined_relay(acceptor, CombinedWsMode::AcceptAny, token.clone()).await;
    let client = relay_client(observer_relay_credential(
        pin,
        9,
        relay.origin.clone(),
        token,
    ));

    relay_probe(&client).await.unwrap();

    let requests = relay.state.inner_requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let request_text = String::from_utf8_lossy(&requests[0]);
    assert!(request_text.starts_with("POST /app/devices/ingest HTTP/1.1\r\n"));
    relay.abort();
}

/// The operation observer reports the exact production ingest request without
/// changing anything visible to the relay fixture.
#[tokio::test]
async fn relay_observer_is_inert_and_reports_every_byte_of_a_complete_production_ingest() {
    let token = mint_jwt(epoch_secs(), epoch_secs() + 10_000);
    let observed = OperationObserver::new();
    let unattached = OperationObserver::new();

    let (observed_artifacts, observed_outcome) =
        run_complete_observer_ingest(Some(observed.clone()), token.clone()).await;
    let (unobserved_artifacts, unobserved_outcome) =
        run_complete_observer_ingest(None, token).await;

    assert_eq!(observed_artifacts, unobserved_artifacts);
    assert_eq!(observed_outcome, unobserved_outcome);
    assert_eq!(observed_outcome.status, IngestStatus::Ok);
    assert_eq!(observed_outcome.path, TransportPath::Relay);
    assert_eq!(
        u64::from(observed_outcome.attempts),
        OBSERVER_CLIENT_PRODUCTION_DIAL_ATTEMPTS
    );

    assert!(observed_artifacts.head_boundary_found);
    assert!(observed_artifacts.saw_request_close);
    assert!(observed_artifacts.sent_terminal_response);
    assert_eq!(observed_artifacts.tcp_accepts, 1);
    assert_eq!(observed_artifacts.ws_dials, 1);
    assert_eq!(observed_artifacts.inner_requests, 1);

    let rebuilt = rebuild_captured_request(&observed_artifacts.request_bytes);
    assert_eq!(rebuilt, observed_artifacts.request_bytes);
    assert_eq!(observed_artifacts.request_bytes.len(), rebuilt.len());

    let counts = observed.counts();
    assert_eq!(
        counts.dial_attempts,
        OBSERVER_CLIENT_PRODUCTION_DIAL_ATTEMPTS
    );
    assert_eq!(counts.request_bytes_sent, rebuilt.len() as u64);
    assert!(counts.close_completed);
    assert_eq!(counts.direct_successes, 0);
    assert_eq!(counts.relay_successes, 1);
    assert_eq!(
        unattached.counts(),
        DialCounts {
            dial_attempts: 0,
            direct_successes: 0,
            relay_successes: 0,
            request_bytes_sent: 0,
            close_completed: false,
        }
    );
}

/// Close 4402 is selected because it is the only close code that terminates
/// after one relay attempt: Unauthorized burns a reactive refresh, while
/// Overflow and Abnormal enter the five-attempt transient ladder. This fixture
/// selects that terminal property; it does not model a billing failure.
#[tokio::test]
async fn relay_observer_is_inert_and_reports_progress_for_the_only_one_attempt_close_4402() {
    let token = mint_jwt(epoch_secs(), epoch_secs() + 10_000);
    let observed = OperationObserver::new();
    let unattached = OperationObserver::new();

    let (observed_artifacts, observed_outcome) =
        run_interrupted_observer_ingest(Some(observed.clone()), token.clone()).await;
    let (unobserved_artifacts, unobserved_outcome) =
        run_interrupted_observer_ingest(None, token).await;

    assert_eq!(observed_artifacts, unobserved_artifacts);
    assert_eq!(observed_outcome, unobserved_outcome);
    assert_eq!(observed_outcome, InterruptedClientOutcome::RelayUnpaid);

    assert!(observed_artifacts.head_boundary_found);
    assert!(!observed_artifacts.saw_request_close);
    assert!(!observed_artifacts.sent_terminal_response);
    assert_eq!(observed_artifacts.tcp_accepts, 1);
    assert_eq!(
        observed_artifacts.ws_dials, 1,
        "the fixture's WS dial count is the authoritative relay-attempt count"
    );
    assert_eq!(observed_artifacts.inner_requests, 1);

    let encoded_head_len = request_head_len(&observed_artifacts.request_bytes).unwrap();
    let full_http_request_len =
        full_request_len_from_captured_head(&observed_artifacts.request_bytes);
    let counts = observed.counts();
    assert_eq!(
        counts.dial_attempts,
        OBSERVER_CLIENT_PRODUCTION_DIAL_ATTEMPTS
    );

    assert!(
        encoded_head_len < counts.request_bytes_sent as usize,
        "at least one request-body byte must be reported before interruption"
    );
    assert!(
        (counts.request_bytes_sent as usize) < full_http_request_len,
        "the interrupted request must not report the unsent tail"
    );
    assert!(!counts.close_completed);
    assert_eq!(
        counts.request_bytes_sent as usize,
        observed_artifacts.request_bytes.len()
    );
    assert_eq!(
        observed_artifacts.request_bytes.len(),
        INITIAL_WINDOW,
        "the no-WINDOW fixture deterministically exhausts only the initial credit"
    );
    assert!(
        counts.request_bytes_sent >= RECOMMENDED_CHUNK as u64,
        "the first full 64 KiB request DATA frame must be visible"
    );
    assert_eq!(counts.direct_successes, 0);
    assert_eq!(counts.relay_successes, 0);
    assert_eq!(
        unattached.counts(),
        DialCounts {
            dial_attempts: 0,
            direct_successes: 0,
            relay_successes: 0,
            request_bytes_sent: 0,
            close_completed: false,
        }
    );
}

#[tokio::test]
async fn relay_credential_without_lan_endpoints_rejected_at_new() {
    let (pin, _acceptor) = tls_pair_with_pin();
    let mut credential =
        observer_relay_credential(pin, 7657, "http://127.0.0.1:1".into(), mint_jwt(100, 200));
    credential.endpoints.clear();

    match ObserverClient::new(credential) {
        Err(TransportError::Pairing(message)) => {
            assert_eq!(message, "relay credential has no LAN endpoints");
        }
        Err(other) => panic!("expected Pairing error, got {other:?}"),
        Ok(_) => panic!("relay credential without LAN endpoints should fail"),
    }
}

#[tokio::test]
async fn relay_proactive_refresh_before_first_dial() {
    let (pin, acceptor) = tls_pair_with_pin();
    let now = epoch_secs();
    let old_token = mint_jwt(100, 200);
    let fresh_token = mint_jwt(now, now + 10_000);
    let relay =
        spawn_combined_relay(acceptor, CombinedWsMode::FreshOnly, fresh_token.clone()).await;
    let client = relay_client(observer_relay_credential(
        pin,
        9,
        relay.origin.clone(),
        old_token,
    ));

    relay_probe(&client).await.unwrap();

    assert_eq!(relay.state.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(relay.state.ws_dials.load(Ordering::SeqCst), 1);
    assert_eq!(
        relay.state.auth_headers.lock().unwrap().as_slice(),
        [format!("Bearer {fresh_token}")]
    );
    relay.abort();
}

#[tokio::test]
async fn relay_unauthorized_refreshes_and_redials_once() {
    let (pin, acceptor) = tls_pair_with_pin();
    let now = epoch_secs();
    let old_token = mint_jwt(now, now + 10_000);
    let fresh_token = mint_jwt(now, now + 20_000);
    let relay =
        spawn_combined_relay(acceptor, CombinedWsMode::FreshOnly, fresh_token.clone()).await;
    let client = relay_client(observer_relay_credential(
        pin,
        9,
        relay.origin.clone(),
        old_token,
    ));

    relay_probe(&client).await.unwrap();

    assert_eq!(relay.state.ws_dials.load(Ordering::SeqCst), 2);
    assert_eq!(relay.state.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(
        relay.state.auth_headers.lock().unwrap().last().cloned(),
        Some(format!("Bearer {fresh_token}"))
    );
    relay.abort();
}

#[tokio::test]
async fn relay_bridge_carrier_refreshes_on_4401_then_succeeds() {
    let (pin, acceptor) = tls_pair_with_pin();
    let now = epoch_secs();
    let old_token = mint_jwt(now, now + 10_000);
    let fresh_token = mint_jwt(now, now + 20_000);
    let relay =
        spawn_combined_relay(acceptor, CombinedWsMode::FreshOnly, fresh_token.clone()).await;
    let credential = observer_relay_credential(pin, 9, relay.origin.clone(), old_token);
    let paired = relay_bridge_state(credential);
    let jv_1 = Arc::new(pl_transport_win::JournalVersionController::new(
        temp_pairing_path("bridge-refresh-jv"),
    ));
    let sync_1 = Arc::new(std::sync::Mutex::new(
        observer_model::SyncSnapshot::default(),
    ));
    let handle = journal_bridge::start(&paired, temp_pairing_path("bridge-refresh"), jv_1, sync_1)
        .await
        .unwrap();
    let cap = capability_from(&handle);

    let response = raw_bridge_request(handle.port(), "/journal", &cap).await;

    assert_eq!(response_status(&response), 200);
    assert_eq!(response_body(&response), "{\"status\":\"ok\"}");
    assert_eq!(relay.state.ws_dials.load(Ordering::SeqCst), 2);
    assert_eq!(relay.state.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(
        relay.state.auth_headers.lock().unwrap().last().cloned(),
        Some(format!("Bearer {fresh_token}"))
    );
    {
        let requests = relay.state.inner_requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(String::from_utf8_lossy(&requests[0]).starts_with("GET /journal HTTP/1.1\r\n"));
    }

    handle.shutdown_and_wait().await;
    relay.abort();
}

#[tokio::test]
async fn relay_bridge_initial_dial_failure_returns_502_and_next_request_redials() {
    let (pin, acceptor) = tls_pair_with_pin();
    let now = epoch_secs();
    let old_token = mint_jwt(now, now + 10_000);
    let fresh_token = mint_jwt(now, now + 20_000);
    let relay = spawn_combined_relay(
        acceptor,
        CombinedWsMode::AlwaysUnauthorized,
        fresh_token.clone(),
    )
    .await;
    let subscriber = CapturingSubscriber::for_target("journal_bridge");
    let _ = tracing::dispatcher::set_global_default(tracing::Dispatch::new(subscriber.clone()));
    let credential = observer_relay_credential(pin, 9, relay.origin.clone(), old_token.clone());
    let paired = relay_bridge_state(credential);
    let jv_2 = Arc::new(pl_transport_win::JournalVersionController::new(
        temp_pairing_path("bridge-relay-fail-jv"),
    ));
    let sync_2 = Arc::new(std::sync::Mutex::new(
        observer_model::SyncSnapshot::default(),
    ));
    let handle = journal_bridge::start(
        &paired,
        temp_pairing_path("bridge-relay-fail"),
        jv_2,
        sync_2,
    )
    .await
    .unwrap();
    let cap = capability_from(&handle);

    let first = raw_bridge_request(handle.port(), "/fail", &cap).await;
    assert_eq!(response_status(&first), 502);
    let first_dials = relay.state.ws_dials.load(Ordering::SeqCst);
    assert!(first_dials >= 2);

    let second = raw_bridge_request(handle.port(), "/fail-again", &cap).await;
    assert_eq!(response_status(&second), 502);
    assert!(
        relay.state.ws_dials.load(Ordering::SeqCst) > first_dials,
        "failed relay carrier must not be cached as live"
    );

    handle.shutdown_and_wait().await;
    let logs = subscriber.joined();
    assert!(logs.contains("category=upstream_unreachable"));
    assert!(logs.contains("code=relay_unauthorized"));
    assert!(!logs.contains(&old_token));
    assert!(!logs.contains(&fresh_token));
    assert!(!logs.contains(&relay.origin));
    relay.abort();
}

// Multi-stream-over-relay does not get a separate live harness here: after
// `dial_carrier` returns, LAN and relay both feed the same transport-agnostic
// `MuxCarrier` coordinator. The duplex and persistent-TLS tests cover that mux
// behavior; these relay tests cover the relay-specific dial and refresh branch.

#[tokio::test]
async fn relay_unauthorized_persists_after_refresh_is_terminal_no_storm() {
    let (pin, acceptor) = tls_pair_with_pin();
    let now = epoch_secs();
    let old_token = mint_jwt(now, now + 10_000);
    let fresh_token = mint_jwt(now, now + 20_000);
    let relay =
        spawn_combined_relay(acceptor, CombinedWsMode::AlwaysUnauthorized, fresh_token).await;
    let client = relay_client(observer_relay_credential(
        pin,
        9,
        relay.origin.clone(),
        old_token,
    ));

    let err = relay_probe(&client).await.unwrap_err();

    assert_relay_error::<()>(Err(err), RelayError::Unauthorized);
    assert_eq!(relay.state.ws_dials.load(Ordering::SeqCst), 2);
    assert_eq!(relay.state.refreshes.load(Ordering::SeqCst), 1);
    relay.abort();
}

#[tokio::test]
async fn relay_refresh_persists_token_for_restart() {
    let (pin, acceptor) = tls_pair_with_pin();
    let now = epoch_secs();
    let old_token = mint_jwt(now, now + 10_000);
    let fresh_token = mint_jwt(now, now + 20_000);
    let relay =
        spawn_combined_relay(acceptor, CombinedWsMode::FreshOnly, fresh_token.clone()).await;
    let credential = observer_relay_credential(pin, 9, relay.origin.clone(), old_token);
    let path = temp_pairing_path("refresh");
    PairedState {
        credential: Some(credential.clone()),
    }
    .save(&path)
    .unwrap();
    let client = relay_client(credential).with_state_path(path.clone());

    relay_probe(&client).await.unwrap();

    let loaded = PairedState::load(&path).unwrap();
    let loaded_credential = loaded.credential.unwrap();
    assert_eq!(
        loaded_credential.device_token.as_deref(),
        Some(fresh_token.as_str())
    );
    assert_eq!(
        loaded_credential.device_token_expires_at,
        Some(now + 20_000)
    );
    let _ = std::fs::remove_file(&path);
    relay.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_refresh_single_flight_across_concurrent_sends() {
    let (pin, acceptor) = tls_pair_with_pin();
    let now = epoch_secs();
    let old_token = mint_jwt(now, now + 10_000);
    let fresh_token = mint_jwt(now, now + 20_000);
    let relay =
        spawn_combined_relay(acceptor, CombinedWsMode::FreshOnly, fresh_token.clone()).await;
    let client = Arc::new(relay_client(observer_relay_credential(
        pin,
        9,
        relay.origin.clone(),
        old_token,
    )));

    let mut tasks = Vec::new();
    for _ in 0..3 {
        let client = client.clone();
        tasks.push(tokio::spawn(async move { relay_probe(&client).await }));
    }
    for task in tasks {
        task.await.unwrap().unwrap();
    }

    assert_eq!(relay.state.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(
        relay
            .state
            .auth_headers
            .lock()
            .unwrap()
            .iter()
            .filter(|auth| *auth == &format!("Bearer {fresh_token}"))
            .count(),
        3
    );
    relay.abort();
}

#[tokio::test]
async fn relay_unpaid_is_terminal_bounded() {
    let (pin, acceptor) = tls_pair_with_pin();
    let now = epoch_secs();
    let token = mint_jwt(now, now + 10_000);
    let relay = spawn_combined_relay(acceptor, CombinedWsMode::Close(4402), token.clone()).await;
    let client = relay_client(observer_relay_credential(
        pin,
        9,
        relay.origin.clone(),
        token,
    ));

    let err = relay_probe(&client).await.unwrap_err();

    assert_relay_error::<()>(Err(err), RelayError::Unpaid);
    assert_eq!(relay.state.ws_dials.load(Ordering::SeqCst), 1);
    assert_eq!(relay.state.refreshes.load(Ordering::SeqCst), 0);
    relay.abort();
}

#[tokio::test]
async fn relay_reasons_map_verbatim_and_redacted() {
    for (mode, expected) in [
        (CombinedWsMode::Close(4402), "relay_unpaid"),
        (CombinedWsMode::UpgradeReject(503), "relay_home_offline"),
    ] {
        let (pin, acceptor) = tls_pair_with_pin();
        let now = epoch_secs();
        let token = mint_jwt(now, now + 10_000);
        let relay = spawn_combined_relay(acceptor, mode, token.clone()).await;
        let client = relay_client(observer_relay_credential(
            pin,
            9,
            relay.origin.clone(),
            token.clone(),
        ));

        let err = relay_probe(&client).await.unwrap_err();
        let code = transport_error_code(&err);

        assert_eq!(code, expected);
        assert!(!code.contains(&token));
        assert!(!code.contains(&relay.origin));
        assert!(!code.contains("https://"));
        assert!(!code.contains(INSTANCE_ID));
        relay.abort();
    }
}
