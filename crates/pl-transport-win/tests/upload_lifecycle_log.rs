// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

mod support;

use std::sync::Arc;

use observer_model::TransportPath;
use observer_pl::frame::{Frame, FLAG_CLOSE, FLAG_DATA};
use observer_pl::multipart::FilePart;
use pl_transport_win::client::ObserverClient;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use support::journal_fake::{direct_credential, read_framed_request, self_signed, server_config};
use support::log_capture::CapturingSubscriber;
use support::observer_contract::fixture as authority_fixture;

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

#[tokio::test]
async fn lan_ingest_lifecycle_logs_direct_path_without_secret_material() {
    let subscriber = CapturingSubscriber::for_target("pl_transport");
    subscriber.install();

    let (cert, key) = self_signed();
    let pin = observer_pl::ca::sha256(cert.as_ref())[..16].to_vec();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(serve_one_ingest(listener, acceptor));
    let client = ObserverClient::new(direct_credential(pin, port))
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
    let logs = subscriber.joined();
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
    let client = ObserverClient::new(direct_credential(pin, port))
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
