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
            handoff: None,
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
        handoff: None,
    };
    let first = client.save_import_file(file()).await;
    assert!(first.is_err(), "refused-device result: {first:?}");
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
                handoff: None,
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
            handoff: None,
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
            retryable: false,
            clear_saved: false,
            reason_code: None,
            kind: None,
        }
    );
    server.await.unwrap();
    assert_eq!(std::fs::metadata(path).unwrap().len(), 128 * 1024 * 1024);
}

#[tokio::test]
async fn import_status_reconciles_over_real_framed_mtls_route() {
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
        let mut decoder = FrameDecoder::new();
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        let stream_id = 'req: loop {
            let n = tls.read(&mut buffer).await.unwrap();
            assert_ne!(n, 0);
            decoder.feed(&buffer[..n]);
            for frame in decoder.drain().unwrap() {
                if frame.flags & FLAG_DATA != 0 {
                    bytes.extend_from_slice(&frame.payload);
                }
                if frame.flags & FLAG_CLOSE != 0 {
                    break 'req frame.stream_id;
                }
            }
        };
        let body = r#"{"status":"running","saved_start":{"active":true,"state":"consumed"}}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
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
        String::from_utf8_lossy(&bytes).into_owned()
    });
    let client = ObserverClient::new(
        direct_credential(pin, port),
        Arc::new(AtomicBool::new(true)),
    )
    .unwrap();
    let reply = client.import_status("20261122_143000").await.unwrap();
    assert_eq!(
        reply,
        pl_transport_win::import_client::ReconcileReply::Confirmed
    );
    let head = server.await.unwrap();
    assert!(
        head.starts_with("GET /app/import/api/20261122_143000 "),
        "{head}"
    );
}

#[tokio::test]
async fn window_bridge_one_time_token_redirects_directly_to_target_and_serves_offline_page() {
    use std::sync::Mutex;

    #[derive(Debug)]
    struct ZeroOffset;
    impl observer_model::LocalOffset for ZeroOffset {
        fn local_zone(
            &self,
            _epoch_secs: u64,
        ) -> Result<observer_model::LocalZone, observer_model::LocalOffsetError> {
            Ok(observer_model::LocalZone {
                utc_offset_seconds: 0,
                tz: None,
            })
        }
    }

    let (cert, _key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let cred = direct_credential(pin, 1);
    let binding = pl_transport_win::ack::JournalIdentity::from_credential(&cred).client_cert_sha256;
    let paired = pl_transport_win::credential::PairedState {
        credential: Some(cred),
        ..Default::default()
    };
    let dir = tempfile::tempdir().unwrap();
    let state_path = dir.path().join("pairing.json");
    let journal_version = Arc::new(pl_transport_win::JournalVersionController::new(
        dir.path().join("jv.json"),
    ));
    let sync = Arc::new(Mutex::new(observer_model::SyncSnapshot::default()));
    let cfg = pl_transport_win::service::SyncConfig {
        device_label: "window-bridge-test".into(),
        period_secs: 300,
        segments_root: dir.path().join("segments"),
        state_path,
        local_offset: Arc::new(ZeroOffset),
        journal_version,
        on_new_confirmation: Arc::new(|| {}),
        facts_fn: Arc::new(pl_transport_win::RawDeviceFacts::default),
        confirmation: Arc::new(Mutex::new(binding)),
        tombstone: Arc::new(Mutex::new(None)),
        liveness: Default::default(),
        #[cfg(feature = "awaiting-hold")]
        awaiting_hold: None,
    };
    let access = pl_transport_win::CredentialAccess::bind(&paired, &cfg, sync, None).unwrap();
    let handle = pl_transport_win::journal_bridge::start_for_window(
        access,
        Arc::new(|target| format!("<html>offline:{target}</html>")),
    )
    .await
    .unwrap();

    // 1. One-time token bootstrap URL redirects directly to `/app/import/20261122_143000` (no `/` flash).
    let bootstrap = handle
        .issue_url_for_path("/app/import/20261122_143000")
        .unwrap();
    let (_, req_target) = bootstrap
        .split_once(&format!("127.0.0.1:{}", handle.port()))
        .unwrap();
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", handle.port()))
        .await
        .unwrap();
    stream
        .write_all(
            format!(
                "GET {req_target} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
                handle.port()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).await.unwrap();
    let resp_str = String::from_utf8_lossy(&resp);
    assert!(resp_str.starts_with("HTTP/1.1 302 Found\r\n"), "{resp_str}");
    assert!(
        resp_str.contains("\r\nLocation: /app/import/20261122_143000\r\n"),
        "{resp_str}"
    );
    let cookie_line = resp_str
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("set-cookie:"))
        .unwrap();
    let cookie_pair = cookie_line
        .split_once(':')
        .unwrap()
        .1
        .trim()
        .split(';')
        .next()
        .unwrap();

    // 2. Same-origin `/_solstone/offline` serves calm HTML with 200 OK instead of 502.
    let mut offline_stream = tokio::net::TcpStream::connect(("127.0.0.1", handle.port()))
        .await
        .unwrap();
    offline_stream
        .write_all(
            format!(
                "GET /_solstone/offline?next=%2Fapp%2Fimport%2F20261122_143000 HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: {cookie_pair}\r\nConnection: close\r\n\r\n",
                handle.port()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut offline_resp = Vec::new();
    offline_stream.read_to_end(&mut offline_resp).await.unwrap();
    let offline_str = String::from_utf8_lossy(&offline_resp);
    assert!(
        offline_str.starts_with("HTTP/1.1 200 OK\r\n"),
        "{offline_str}"
    );
    assert!(
        offline_str.contains(
            "<html>offline:/_solstone/offline?next=%2Fapp%2Fimport%2F20261122_143000</html>"
        ),
        "{offline_str}"
    );

    handle.shutdown_and_wait().await;
}

#[tokio::test]
async fn resumable_upload_session_reuses_one_carrier_and_enforces_chunk_framing() {
    use pl_transport_win::import_client::{
        sha256_hex, ResumableUploadSession, UploadChunkReply, UploadCommitReply, UploadCreateReply,
        UploadStatusReply,
    };
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let (cert, key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepts = Arc::new(AtomicUsize::new(0));
    let accepts_server = accepts.clone();

    let total_payload: Vec<u8> = (0..(5 * 1024 * 1024 + 123))
        .map(|i| ((i * 31 + 7) % 251) as u8)
        .collect();
    let total_sha = sha256_hex(&total_payload);
    let expected_payload = total_payload.clone();
    let expected_sha = total_sha.clone();

    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        accepts_server.fetch_add(1, Ordering::SeqCst);
        let mut tls = TlsAcceptor::from(Arc::new(server_config(cert, key)))
            .accept(tcp)
            .await
            .unwrap();
        let mut decoder = FrameDecoder::new();
        let mut streams: HashMap<u32, Vec<u8>> = HashMap::new();
        let mut assembled = Vec::new();
        let mut handled = 0usize;
        let mut buffer = [0; 16384];

        while handled < 5 {
            let n = tls.read(&mut buffer).await.unwrap();
            assert_ne!(n, 0);
            decoder.feed(&buffer[..n]);
            for frame in decoder.drain().unwrap() {
                if frame.flags & FLAG_DATA != 0 {
                    streams
                        .entry(frame.stream_id)
                        .or_default()
                        .extend_from_slice(&frame.payload);
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
                    let raw = streams.remove(&frame.stream_id).unwrap_or_default();
                    let head_end = raw.windows(4).position(|s| s == b"\r\n\r\n").unwrap();
                    let head = String::from_utf8_lossy(&raw[..head_end]).into_owned();
                    let body = &raw[head_end + 4..];
                    let lower_head = head.to_ascii_lowercase();
                    assert!(!lower_head.contains("transfer-encoding: chunked"), "{head}");
                    assert!(!lower_head.contains("expect: 100-continue"), "{head}");

                    let first_line = head.lines().next().unwrap();
                    let (status, resp_body) = if first_line
                        .starts_with("POST /app/import/api/uploads ")
                    {
                        let req: serde_json::Value = serde_json::from_slice(body).unwrap();
                        assert_eq!(req["sha256"], expected_sha);
                        assert_eq!(req["device_noun"], "PC");
                        assert_eq!(req["imported_via"], "windows");
                        assert_eq!(req["total_bytes"], expected_payload.len() as u64);
                        (
                            "201 Created",
                            format!(
                                r#"{{"upload_id":"550e8400-e29b-41d4-a716-446655440000","client_item_id":"stable","status":"open","next_offset":0,"total_bytes":{},"chunk_bytes":4194304,"expires_at":"2026-12-01T00:00:00Z"}}"#,
                                expected_payload.len()
                            ),
                        )
                    } else if first_line.starts_with(
                        "PUT /app/import/api/uploads/550e8400-e29b-41d4-a716-446655440000?offset=",
                    ) {
                        let offset_str = first_line
                            .split("?offset=")
                            .nth(1)
                            .unwrap()
                            .split(' ')
                            .next()
                            .unwrap();
                        let offset: usize = offset_str.parse().unwrap();
                        assert_eq!(offset, assembled.len());
                        let expected_cl = format!("content-length: {}", body.len());
                        assert!(lower_head.contains(&expected_cl), "{head}");
                        let chunk_sha = head
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("x-chunk-sha256")
                                    .then(|| value.trim().to_owned())
                            })
                            .expect("missing X-Chunk-Sha256");
                        assert_eq!(chunk_sha, sha256_hex(body));
                        assembled.extend_from_slice(body);
                        let complete = assembled.len() == expected_payload.len();
                        (
                            "200 OK",
                            format!(
                                r#"{{"upload_id":"550e8400-e29b-41d4-a716-446655440000","next_offset":{},"total_bytes":{},"complete":{complete},"expires_at":"2026-12-01T00:00:00Z"}}"#,
                                assembled.len(),
                                expected_payload.len()
                            ),
                        )
                    } else if first_line.starts_with(
                        "GET /app/import/api/uploads/550e8400-e29b-41d4-a716-446655440000 ",
                    ) {
                        (
                            "200 OK",
                            format!(
                                r#"{{"upload_id":"550e8400-e29b-41d4-a716-446655440000","client_item_id":"stable","filename":"big.bin","status":"open","phase":"sending","next_offset":{},"total_bytes":{},"chunk_bytes":4194304,"imported_via":"windows","device_noun":"PC","source_hint":null,"created_at":"2026-11-25T00:00:00Z","last_activity_at":"2026-11-25T00:00:01Z","expires_at":"2026-12-01T00:00:00Z"}}"#,
                                assembled.len(),
                                expected_payload.len()
                            ),
                        )
                    } else if first_line.starts_with(
                        "POST /app/import/api/uploads/550e8400-e29b-41d4-a716-446655440000/commit ",
                    ) {
                        assert_eq!(assembled, expected_payload);
                        let req: serde_json::Value = serde_json::from_slice(body).unwrap();
                        assert_eq!(req["sha256"], expected_sha);
                        (
                            "200 OK",
                            r#"{"status":"staged","path":"/imports/20261125_120000/big.bin","timestamp":"20261125_120000"}"#.to_owned(),
                        )
                    } else {
                        panic!("unexpected request: {first_line}");
                    };

                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{resp_body}",
                        resp_body.len()
                    );
                    tls.write_all(
                        &Frame::new(
                            frame.stream_id,
                            FLAG_DATA | FLAG_CLOSE,
                            response.into_bytes(),
                        )
                        .encode()
                        .unwrap(),
                    )
                    .await
                    .unwrap();
                    tls.flush().await.unwrap();
                    handled += 1;
                }
            }
        }
    });

    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("big.bin");
    std::fs::write(&path, &total_payload).unwrap();
    let client = Arc::new(
        ObserverClient::new(
            direct_credential(pin, port),
            Arc::new(AtomicBool::new(true)),
        )
        .unwrap(),
    );
    let mut session = ResumableUploadSession::open(client).await.unwrap();
    let file = ImportFile {
        id: "stable",
        name: "big.bin",
        content_type: "application/octet-stream",
        path: &path,
        handoff: None,
    };
    let created = session
        .create_upload(&file, total_payload.len() as u64, &total_sha)
        .await
        .unwrap();
    let UploadCreateReply::Open(open) = created else {
        panic!("expected Open, got {created:?}");
    };
    assert_eq!(open.next_offset, 0);
    assert_eq!(open.chunk_bytes, 4 * 1024 * 1024);

    let chunk1 = session
        .put_chunk(&open.upload_id, 0, &total_payload[..4 * 1024 * 1024])
        .await
        .unwrap();
    assert_eq!(
        chunk1,
        UploadChunkReply::Accepted {
            next_offset: 4 * 1024 * 1024,
            total_bytes: total_payload.len() as u64,
            complete: false,
            expires_at: Some("2026-12-01T00:00:00Z".into()),
        }
    );

    let status = session.get_status(&open.upload_id).await.unwrap();
    assert!(matches!(
        status,
        UploadStatusReply::Open {
            next_offset: 4_194_304,
            ..
        }
    ));

    let chunk2 = session
        .put_chunk(
            &open.upload_id,
            4 * 1024 * 1024,
            &total_payload[4 * 1024 * 1024..],
        )
        .await
        .unwrap();
    assert!(matches!(
        chunk2,
        UploadChunkReply::Accepted { complete: true, .. }
    ));

    let committed = session
        .commit_upload(&open.upload_id, &total_sha)
        .await
        .unwrap();
    assert!(matches!(committed, UploadCommitReply::Saved(_)));
    session.close().await;
    server.await.unwrap();
    assert_eq!(
        accepts.load(Ordering::SeqCst),
        1,
        "must reuse one mTLS carrier"
    );
}

#[tokio::test]
async fn resumable_upload_session_stops_dials_after_tls_refusal() {
    use pl_transport_win::import_client::{sha256_hex, ResumableUploadSession};

    let (cert, key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
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
    let path = root.path().join("big.bin");
    std::fs::write(&path, b"payload").unwrap();
    let client = Arc::new(
        ObserverClient::new(
            direct_credential(pin, port),
            Arc::new(AtomicBool::new(true)),
        )
        .unwrap(),
    );
    let mut session = ResumableUploadSession::open(client.clone()).await.unwrap();
    let file = ImportFile {
        id: "stable",
        name: "big.bin",
        content_type: "application/octet-stream",
        path: &path,
        handoff: None,
    };
    let res = session
        .create_upload(&file, 7, &sha256_hex(b"payload"))
        .await;
    assert!(res.is_err(), "expected refusal error, got {res:?}");
    assert!(client.refusal_stop().is_some());
    assert!(!session.is_alive());
    assert!(ResumableUploadSession::open(client).await.is_err());
    server.await.unwrap();
}

#[tokio::test]
async fn probe_resumable_import_and_abort_upload_work_over_real_mtls_routes() {
    use pl_transport_win::import_client::{ResumableConfig, ResumableProbe, UploadAbortReply};

    let (cert, key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
        for expected in [
            (
                "GET /api/system/status ",
                r#"{"status":"ok","capabilities":["resumable_import_uploads"]}"#,
            ),
            (
                "GET /app/import/api/sources ",
                r#"{"sources":[],"resumable":{"threshold_bytes":16777216,"chunk_bytes":4194304,"max_file_bytes":4294967296}}"#,
            ),
            (
                "DELETE /app/import/api/uploads/550e8400-e29b-41d4-a716-446655440000 ",
                r#"{"deleted":true,"upload_id":"550e8400-e29b-41d4-a716-446655440000"}"#,
            ),
        ] {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut tls = acceptor.accept(tcp).await.unwrap();
            let mut decoder = FrameDecoder::new();
            let mut bytes = Vec::new();
            let mut buffer = [0; 4096];
            let stream_id = loop {
                let n = tls.read(&mut buffer).await.unwrap();
                assert_ne!(n, 0);
                decoder.feed(&buffer[..n]);
                let mut closed = None;
                for frame in decoder.drain().unwrap() {
                    if frame.flags & FLAG_DATA != 0 {
                        bytes.extend_from_slice(&frame.payload);
                    }
                    if frame.flags & FLAG_CLOSE != 0 {
                        closed = Some(frame.stream_id);
                    }
                }
                if let Some(id) = closed {
                    break id;
                }
            };
            let req = String::from_utf8_lossy(&bytes);
            assert!(req.starts_with(expected.0), "got {req}");
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                expected.1.len(),
                expected.1
            );
            tls.write_all(
                &Frame::new(stream_id, FLAG_DATA | FLAG_CLOSE, resp.into_bytes())
                    .encode()
                    .unwrap(),
            )
            .await
            .unwrap();
            tls.flush().await.unwrap();
        }
    });

    let client = ObserverClient::new(
        direct_credential(pin, port),
        Arc::new(AtomicBool::new(true)),
    )
    .unwrap();
    let probe = client.probe_resumable_import().await.unwrap();
    assert_eq!(
        probe,
        ResumableProbe::Supported(ResumableConfig {
            threshold_bytes: 16_777_216,
            chunk_bytes: 4_194_304,
            max_file_bytes: 4_294_967_296,
        })
    );
    let aborted = client
        .abort_upload("550e8400-e29b-41d4-a716-446655440000")
        .await
        .unwrap();
    assert_eq!(aborted, UploadAbortReply::Deleted);
    server.await.unwrap();
}
