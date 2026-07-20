// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

mod support;

use std::sync::{Arc, Mutex};

use observer_model::TransportPath;
use observer_pl::frame::{Frame, FrameDecoder, FLAG_CLOSE, FLAG_DATA};
use observer_pl::multipart::FilePart;
use pl_transport_win::client::ObserverClient;
use pl_transport_win::credential::{Credential, EndpointAddr};
use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::ServerConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use support::observer_contract::fixture as authority_fixture;

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

async fn serve_one_ingest(listener: TcpListener, acceptor: TlsAcceptor) -> Vec<u8> {
    let (tcp, _) = listener.accept().await.unwrap();
    let mut tls = acceptor.accept(tcp).await.unwrap();
    let (stream_id, request) = read_framed_request(&mut tls).await;
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

#[derive(Clone)]
struct CapturingSubscriber {
    lines: Arc<Mutex<Vec<String>>>,
}

impl tracing::Subscriber for CapturingSubscriber {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.target() == "pl_transport"
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

#[tokio::test]
async fn lan_ingest_lifecycle_logs_direct_path_without_secret_material() {
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let subscriber = CapturingSubscriber {
        lines: lines.clone(),
    };
    tracing::dispatcher::set_global_default(tracing::Dispatch::new(subscriber))
        .expect("install PL transport log capture subscriber");

    let (cert, key) = self_signed();
    let pin = observer_pl::ca::sha256(cert.as_ref())[..16].to_vec();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_one_ingest(listener, acceptor));
    let client = ObserverClient::new(observer_credential(pin, port))
        .unwrap()
        .with_observer_key(Some("observer-key".into()));
    let files = [FilePart {
        filename: "display_1_screen.mp4".into(),
        content_type: "video/mp4".into(),
        bytes: b"segment bytes".to_vec(),
    }];

    let (_response, metadata) = client
        .ingest("120000_300", "20260702", "windows", &files)
        .await
        .unwrap();
    let _request = server.await.unwrap();

    assert_eq!(metadata.path, TransportPath::Direct);
    assert_eq!(metadata.attempts, 1);
    let logs = lines.lock().unwrap().join("\n");
    assert!(logs.contains("dial success"));
    assert!(logs.contains("path=direct"));
    assert!(!logs.contains("127.0.0.1"));
    assert!(!logs.contains(&format!("127.0.0.1:{port}")));
    assert!(!logs.contains("test-instance"));
    assert!(!logs.contains("observer-key"));
    assert!(!logs.contains("token"));
    assert!(!logs.contains("relay"));
}

#[tokio::test]
async fn observer_contract_authority_upload_reuses_ingest_capture_seam() {
    let fixture =
        authority_fixture("example.observer.ingestUpload.request.body.multipart-form-data.default");
    let payload = &fixture["payload"];
    let (cert, key) = self_signed();
    let pin = observer_pl::ca::sha256(cert.as_ref())[..16].to_vec();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_one_ingest(listener, acceptor));
    let client = ObserverClient::new(observer_credential(pin, port))
        .unwrap()
        .with_observer_key(Some("authority-observer".into()));
    let files: Vec<FilePart> = payload["files"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(index, filename)| FilePart {
            filename: filename.as_str().unwrap().to_owned(),
            content_type: "application/octet-stream".to_owned(),
            bytes: format!("test-owned-{index}").into_bytes(),
        })
        .collect();

    client
        .ingest(
            payload["segment"].as_str().unwrap(),
            payload["day"].as_str().unwrap(),
            payload["platform"].as_str().unwrap(),
            &files,
        )
        .await
        .unwrap();
    let request = String::from_utf8(server.await.unwrap()).unwrap();
    assert!(request.starts_with("POST /app/observer/ingest HTTP/1.1\r\n"));
    assert!(request.contains("X-Solstone-Observer: authority-observer\r\n"));
    assert!(request.contains("Authorization: Bearer authority-observer\r\n"));
    assert!(request.contains(&format!(
        "{}: {}\r\n",
        observer_pl::PROTOCOL_VERSION_HEADER,
        observer_pl::OBSERVER_PROTOCOL_VERSION
    )));
    for filename in payload["files"].as_array().unwrap() {
        assert!(request.contains(&format!(
            "name=\"files\"; filename=\"{}\"",
            filename.as_str().unwrap()
        )));
    }
}
