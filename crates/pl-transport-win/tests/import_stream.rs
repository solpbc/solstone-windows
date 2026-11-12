// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

mod support;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use pl_transport_win::import_client::{ImportFile, ImportReply};
use pl_transport_win::ObserverClient;
use spl_core::frame::{Frame, FrameDecoder, FLAG_CLOSE, FLAG_DATA};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use support::journal_fake::{
    direct_credential, refusing_server_config, self_signed, server_config,
};

#[tokio::test]
async fn multi_megabyte_save_preserves_bytes_and_sends_the_device_headers() {
    let (cert, key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut tls = TlsAcceptor::from(Arc::new(server_config(cert, key)))
            .accept(tcp)
            .await
            .unwrap();
        let mut decoder = FrameDecoder::new();
        let mut bytes = Vec::new();
        let mut buffer = [0; 8192];
        let stream_id = 'request: loop {
            let n = tls.read(&mut buffer).await.unwrap();
            assert_ne!(n, 0);
            decoder.feed(&buffer[..n]);
            for frame in decoder.drain().unwrap() {
                if frame.flags & FLAG_DATA != 0 {
                    bytes.extend_from_slice(&frame.payload);
                    tls.write_all(
                        &Frame::window(frame.stream_id, frame.payload.len() as u32)
                            .encode()
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                    tls.flush().await.unwrap();
                }
                if frame.flags & FLAG_CLOSE != 0 {
                    break 'request frame.stream_id;
                }
            }
        };
        let body = r#"{"status":"staged","path":"/imports/item/document.txt","timestamp":"item"}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        tls.write_all(
            &Frame::new(stream_id, FLAG_DATA | FLAG_CLOSE, response.into_bytes())
                .encode()
                .unwrap(),
        )
        .await
        .unwrap();
        tls.flush().await.unwrap();
        let _ = tls.read(&mut buffer).await;
        bytes
    });
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("payload");
    let bytes: Vec<u8> = (0..3 * 1024 * 1024 + 17).map(|i| (i % 251) as u8).collect();
    std::fs::write(&path, &bytes).unwrap();
    let client = Arc::new(
        ObserverClient::new(
            direct_credential(pin, port),
            Arc::new(AtomicBool::new(true)),
        )
        .unwrap(),
    );
    let reply = tokio::time::timeout(
        Duration::from_secs(15),
        client.save_import_file(ImportFile {
            id: "stable",
            name: "document.txt",
            content_type: "text/plain",
            path: &path,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(reply, ImportReply::Saved(_)), "{reply:?}");
    let received = task.await.unwrap();
    let head_end = received.windows(4).position(|s| s == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&received[..head_end]);
    assert!(head.starts_with("POST /app/import/api/save "));
    assert!(head
        .to_ascii_lowercase()
        .contains(&observer_pl::PROTOCOL_VERSION_HEADER.to_ascii_lowercase()));
    let file_start = received
        .windows(bytes[..4096].len())
        .position(|s| s == &bytes[..4096])
        .unwrap();
    assert_eq!(&received[file_start..file_start + bytes.len()], &bytes);
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[tokio::test]
async fn a_refused_device_stops_future_file_and_start_dials() {
    let (cert, key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let config = refusing_server_config(
            cert,
            key,
            rustls::CertificateError::ApplicationVerificationFailure,
        );
        assert!(TlsAcceptor::from(Arc::new(config))
            .accept(tcp)
            .await
            .is_err());
        assert!(
            tokio::time::timeout(Duration::from_millis(500), listener.accept())
                .await
                .is_err()
        );
    });
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("payload");
    std::fs::write(&path, b"held").unwrap();
    let client = Arc::new(
        ObserverClient::new(
            direct_credential(pin, port),
            Arc::new(AtomicBool::new(true)),
        )
        .unwrap(),
    );
    let file = || ImportFile {
        id: "stable",
        name: "file.txt",
        content_type: "text/plain",
        path: &path,
    };
    assert!(client.save_import_file(file()).await.is_err());
    assert!(client.refusal_stop().is_some());
    assert!(client.save_import_file(file()).await.is_err());
    assert!(client
        .start_import(&pl_transport_win::import_client::SavedImport {
            path: "p".into(),
            timestamp: "t".into()
        })
        .await
        .is_err());
    assert_eq!(std::fs::read(path).unwrap(), b"held");
    task.await.unwrap();
}

#[tokio::test]
async fn cancellation_closes_the_carrier_and_keeps_the_held_file() {
    let (cert, key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (started, ready) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut tls = TlsAcceptor::from(Arc::new(server_config(cert, key)))
            .accept(tcp)
            .await
            .unwrap();
        let mut buffer = [0; 8192];
        assert!(tls.read(&mut buffer).await.unwrap() > 0);
        started.send(()).unwrap();
        // No credits: the source must stop rather than read the entire file.
        tokio::time::timeout(Duration::from_secs(3), async {
            while tls.read(&mut buffer).await.unwrap_or(0) != 0 {}
        })
        .await
        .unwrap();
    });
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("payload");
    std::fs::File::create(&path)
        .unwrap()
        .set_len(128 * 1024 * 1024)
        .unwrap();
    let client = Arc::new(
        ObserverClient::new(
            direct_credential(pin, port),
            Arc::new(AtomicBool::new(true)),
        )
        .unwrap(),
    );
    let upload_path = path.clone();
    let upload = tokio::spawn(async move {
        client
            .save_import_file(ImportFile {
                id: "stable",
                name: "file.txt",
                content_type: "text/plain",
                path: &upload_path,
            })
            .await
    });
    ready.await.unwrap();
    upload.abort();
    assert!(upload.await.unwrap_err().is_cancelled());
    server.await.unwrap();
    assert_eq!(std::fs::metadata(path).unwrap().len(), 128 * 1024 * 1024);
}

#[tokio::test]
async fn an_early_http_refusal_stops_a_large_body_and_preserves_the_copy() {
    let (cert, key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut tls = TlsAcceptor::from(Arc::new(server_config(cert, key)))
            .accept(tcp)
            .await
            .unwrap();
        let mut buffer = [0; 8192];
        let mut decoder = FrameDecoder::new();
        let id = loop {
            let n = tls.read(&mut buffer).await.unwrap();
            assert_ne!(n, 0);
            decoder.feed(&buffer[..n]);
            if let Some(frame) = decoder
                .drain()
                .unwrap()
                .into_iter()
                .find(|f| f.flags & FLAG_DATA != 0)
            {
                break frame.stream_id;
            }
        };
        let body = r#"{"error":"it's larger than your journal takes","reason_code":"multipart_part_too_large"}"#;
        let answer = format!(
            "HTTP/1.1 413 Refused\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        tls.write_all(
            &Frame::new(id, FLAG_DATA | FLAG_CLOSE, answer.into_bytes())
                .encode()
                .unwrap(),
        )
        .await
        .unwrap();
        tls.flush().await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while tls.read(&mut buffer).await.unwrap_or(0) != 0 {}
        })
        .await
        .unwrap();
    });
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("payload");
    std::fs::File::create(&path)
        .unwrap()
        .set_len(128 * 1024 * 1024)
        .unwrap();
    let client = Arc::new(
        ObserverClient::new(
            direct_credential(pin, port),
            Arc::new(AtomicBool::new(true)),
        )
        .unwrap(),
    );
    let reply = tokio::time::timeout(
        Duration::from_secs(5),
        client.save_import_file(ImportFile {
            id: "stable",
            name: "file.txt",
            content_type: "text/plain",
            path: &path,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    // The shared bridge stops a premature response at its head, so its
    // status survives while the body is deliberately absent. Retain custody
    // and use the same fallback as any refusal without a sentence.
    assert_eq!(
        reply,
        ImportReply::Refused {
            sentence: "your journal couldn't accept this.".into(),
            retryable: true
        }
    );
    server.await.unwrap();
    assert_eq!(std::fs::metadata(path).unwrap().len(), 128 * 1024 * 1024);
}
