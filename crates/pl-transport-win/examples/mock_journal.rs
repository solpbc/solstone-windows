// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Mock journal for the native journal-window live validation.
//!
//! Operator-facing harness only: it emits a paired state for the app, accepts real
//! PL/TLS carriers, serves a tiny dashboard, and records request provenance.

use std::collections::HashMap;
use std::error::Error;
use std::fs::{File, OpenOptions};
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use observer_pl::frame::{Frame, FrameDecoder, FLAG_CLOSE, FLAG_DATA, FLAG_WINDOW};
use observer_pl::mux::INITIAL_WINDOW;
use pl_transport_win::credential::{Credential, EndpointAddr, PairedState};
use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::ServerConfig;
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio_rustls::TlsAcceptor;

const DEFAULT_MARKER: &str = "SOLSTONE_JOURNAL_WINDOW_LIVE_MARKER";
const OBSERVER_KEY: &str = "mock-observer-key";

#[derive(Debug)]
struct Args {
    pairing_out: PathBuf,
    transcript: PathBuf,
    ready_file: PathBuf,
    marker: String,
}

#[derive(Debug)]
struct RequestHead {
    method: String,
    path: String,
    has_observer_header: bool,
    has_authorization: bool,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse()?;
    ensure_parent(&args.pairing_out)?;
    ensure_parent(&args.transcript)?;
    ensure_parent(&args.ready_file)?;

    let transcript = Arc::new(Mutex::new(
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&args.transcript)?,
    ));

    let (cert, key) = self_signed()?;
    let pin = observer_pl::ca::sha256(cert.as_ref())[..16].to_vec();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)?));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();

    let paired = PairedState {
        credential: Some(observer_credential(pin, port)?),
        observer_key: Some(OBSERVER_KEY.to_string()),
        observer_name: Some("mock observer".to_string()),
    };
    paired.save(&args.pairing_out)?;
    write_ready_file(&args.ready_file, port, &args.marker)?;
    println!(
        "MOCK_JOURNAL_READY port={} pairing={} transcript={}",
        port,
        args.pairing_out.display(),
        args.transcript.display()
    );
    std::io::stdout().flush()?;

    let carrier_index = Arc::new(AtomicUsize::new(0));
    loop {
        let (tcp, _) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let transcript = transcript.clone();
        let marker = args.marker.clone();
        let index = carrier_index.fetch_add(1, Ordering::SeqCst) + 1;
        tokio::spawn(async move {
            if let Err(error) = serve_carrier(index, tcp, acceptor, transcript, marker).await {
                eprintln!("mock carrier {index} exited: {error}");
            }
        });
    }
}

async fn serve_carrier(
    carrier_index: usize,
    tcp: tokio::net::TcpStream,
    acceptor: TlsAcceptor,
    transcript: Arc<Mutex<File>>,
    marker: String,
) -> Result<(), Box<dyn Error>> {
    let tls = acceptor.accept(tcp).await?;
    let (mut read, mut write) = tokio::io::split(tls);
    let mut decoder = FrameDecoder::new();
    let mut requests: HashMap<u32, Vec<u8>> = HashMap::new();
    let mut buf = [0u8; 4096];

    loop {
        let n = read.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        decoder.feed(&buf[..n]);
        for frame in decoder.drain()? {
            if let Some(pong) = frame.control_pong() {
                write.write_all(&pong.encode()?).await?;
                write.flush().await?;
                continue;
            }

            if frame.flags & FLAG_DATA != 0 {
                requests
                    .entry(frame.stream_id)
                    .or_default()
                    .extend_from_slice(&frame.payload);
                write_window(&mut write, frame.stream_id).await?;
            }

            if frame.flags & FLAG_CLOSE != 0 {
                let bytes = requests.remove(&frame.stream_id).unwrap_or_default();
                let request = parse_request(&bytes);
                append_transcript(&transcript, carrier_index, frame.stream_id, &request).await?;
                write_http_response(&mut write, frame.stream_id, &request.path, &marker).await?;
            }
        }
    }

    Ok(())
}

async fn write_window<W>(write: &mut W, stream_id: u32) -> Result<(), Box<dyn Error>>
where
    W: AsyncWrite + Unpin,
{
    let frame = Frame::new(
        stream_id,
        FLAG_WINDOW,
        (INITIAL_WINDOW as u32).to_be_bytes().to_vec(),
    );
    write.write_all(&frame.encode()?).await?;
    write.flush().await?;
    Ok(())
}

async fn write_http_response<W>(
    write: &mut W,
    stream_id: u32,
    path: &str,
    marker: &str,
) -> Result<(), Box<dyn Error>>
where
    W: AsyncWrite + Unpin,
{
    let (content_type, body) = response_body(path, marker);
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{body}",
        body.len()
    );
    let frame = Frame::new(stream_id, FLAG_DATA | FLAG_CLOSE, response.into_bytes());
    write.write_all(&frame.encode()?).await?;
    write.flush().await?;
    Ok(())
}

fn response_body(path: &str, marker: &str) -> (&'static str, String) {
    match path {
        "/" => (
            "text/html; charset=utf-8",
            format!(
                "<!doctype html><meta charset=\"utf-8\"><title>mock journal</title><link rel=\"stylesheet\" href=\"/asset-a\"><script src=\"/asset-b\"></script><main style=\"font: 28px sans-serif; padding: 48px\">{marker}</main>"
            ),
        ),
        "/asset-a" => (
            "text/css; charset=utf-8",
            format!("body::after {{ content: \"{marker}\"; display: none; }}"),
        ),
        "/asset-b" => (
            "application/javascript; charset=utf-8",
            format!("window.__SOLSTONE_MOCK_MARKER = {:?};", marker),
        ),
        observer_pl::paths::INGEST_EVENT => ("text/plain; charset=utf-8", "ok".to_string()),
        _ => ("text/plain; charset=utf-8", "ok".to_string()),
    }
}

fn parse_request(bytes: &[u8]) -> RequestHead {
    let text = String::from_utf8_lossy(bytes);
    let head = text.split("\r\n\r\n").next().unwrap_or_default();
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let path = target
        .split_once('?')
        .map(|(path, _)| path)
        .unwrap_or(target.as_str())
        .to_string();
    let mut has_observer_header = false;
    let mut has_authorization = false;

    for line in lines {
        let Some((name, _)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.eq_ignore_ascii_case(observer_pl::OBSERVER_HANDLE_HEADER) {
            has_observer_header = true;
        }
        if name.eq_ignore_ascii_case("Authorization") {
            has_authorization = true;
        }
    }

    RequestHead {
        method,
        path,
        has_observer_header,
        has_authorization,
    }
}

async fn append_transcript(
    transcript: &Arc<Mutex<File>>,
    carrier_index: usize,
    stream_id: u32,
    request: &RequestHead,
) -> Result<(), Box<dyn Error>> {
    let line = serde_json::json!({
        "carrier_index": carrier_index,
        "stream_id": stream_id,
        "method": request.method,
        "path": request.path,
        "has_observer_header": request.has_observer_header,
        "has_authorization": request.has_authorization,
    });
    let mut file = transcript.lock().await;
    writeln!(file, "{line}")?;
    file.flush()?;
    Ok(())
}

fn self_signed() -> Result<(CertificateDer<'static>, PrivateKeyDer<'static>), Box<dyn Error>> {
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;
    let params = CertificateParams::new(vec!["spl.local".to_string()])?;
    let cert = params.self_signed(&key)?;
    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
    Ok((cert_der, key_der))
}

fn server_config(
    cert: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
) -> Result<ServerConfig, Box<dyn Error>> {
    let config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)?;
    Ok(config)
}

fn observer_credential(pin: Vec<u8>, port: u16) -> Result<Credential, Box<dyn Error>> {
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;
    let params = CertificateParams::new(vec!["observer.test".to_string()])?;
    let cert = params.self_signed(&key)?;
    Ok(Credential {
        client_key_pem: key.serialize_pem(),
        client_cert_pem: cert.pem(),
        ca_chain_pem: vec![cert.pem()],
        ca_fp_prefix: pin,
        instance_id: "mock-instance".into(),
        home_label: "mock journal".into(),
        endpoints: vec![EndpointAddr {
            host: "127.0.0.1".into(),
            port,
        }],
        relay_origin: None,
        device_token: None,
        device_token_expires_at: None,
    })
}

fn write_ready_file(path: &Path, port: u16, marker: &str) -> Result<(), Box<dyn Error>> {
    let ready = serde_json::json!({
        "port": port,
        "marker": marker,
    });
    std::fs::write(path, serde_json::to_vec_pretty(&ready)?)?;
    Ok(())
}

fn ensure_parent(path: &Path) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

impl Args {
    fn parse() -> Result<Self, Box<dyn Error>> {
        let mut pairing_out = None;
        let mut transcript = None;
        let mut ready_file = None;
        let mut marker = DEFAULT_MARKER.to_string();
        let mut args = std::env::args().skip(1);

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--pairing-out" => pairing_out = Some(next_path(&mut args, "--pairing-out")?),
                "--transcript" => transcript = Some(next_path(&mut args, "--transcript")?),
                "--ready-file" => ready_file = Some(next_path(&mut args, "--ready-file")?),
                "--marker" => marker = next_value(&mut args, "--marker")?,
                "--help" | "-h" => return Err(invalid_input(usage()).into()),
                _ => return Err(invalid_input(format!("unknown arg {arg}\n{}", usage())).into()),
            }
        }

        Ok(Self {
            pairing_out: pairing_out.ok_or_else(|| invalid_input(usage()))?,
            transcript: transcript.ok_or_else(|| invalid_input(usage()))?,
            ready_file: ready_file.ok_or_else(|| invalid_input(usage()))?,
            marker,
        })
    }
}

fn next_path(
    args: &mut impl Iterator<Item = String>,
    name: &str,
) -> Result<PathBuf, Box<dyn Error>> {
    Ok(PathBuf::from(next_value(args, name)?))
}

fn next_value(
    args: &mut impl Iterator<Item = String>,
    name: &str,
) -> Result<String, Box<dyn Error>> {
    args.next()
        .ok_or_else(|| invalid_input(format!("missing value for {name}\n{}", usage())).into())
}

fn usage() -> String {
    "usage: mock_journal --pairing-out <path> --transcript <path> --ready-file <path> [--marker <string>]".to_string()
}

fn invalid_input(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
