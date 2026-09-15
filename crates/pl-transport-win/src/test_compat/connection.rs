// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::Arc;
    use std::time::Duration;

    use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use rustls::ServerConfig;
    use spl_core::ca::sha256;
    use spl_core::frame::{
        Frame, FrameDecoder, FLAG_CLOSE, FLAG_DATA, FLAG_RESET, FLAG_WINDOW,
        RESET_FLOW_CONTROL_ERROR,
    };
    use spl_core::http;
    use spl_core::mux::{MuxError, INITIAL_WINDOW};
    use spl_transport::connection::request_once;
    use spl_transport::credential::{Credential, EndpointAddr};
    use spl_transport::{
        OperationObserver, RequestError, RequestOptions, TransportClient, TransportError,
    };
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio_rustls::TlsAcceptor;

    fn fixture(port: u16) -> (ServerConfig, Arc<rustls::ClientConfig>, Credential) {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).expect("test key");
        let params = CertificateParams::new(vec!["spl.local".to_string()]).expect("test cert");
        let cert = params.self_signed(&key).expect("self-signed cert");
        let cert_der = CertificateDer::from(cert.der().to_vec());
        let server =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("TLS versions")
                .with_no_client_auth()
                .with_single_cert(
                    vec![cert_der.clone()],
                    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
                )
                .expect("server config");
        let pin = sha256(cert_der.as_ref())[..16].to_vec();
        let config = Arc::new(spl_transport::tls::pairing_config(&pin).expect("pairing config"));
        let credential = Credential {
            client_key_pem: key.serialize_pem(),
            client_cert_pem: cert.pem(),
            ca_chain_pem: vec![cert.pem()],
            ca_fp_prefix: pin,
            instance_id: "compat-instance".into(),
            home_label: "Compat".into(),
            endpoints: vec![EndpointAddr {
                host: "127.0.0.1".into(),
                port,
            }],
            home_attestation: None,
            local_endpoints: None,
            relay_origin: None,
            device_token: None,
            device_token_expires_at: None,
        };
        (server, config, credential)
    }

    async fn accept_tls(
        listener: TcpListener,
        config: ServerConfig,
    ) -> tokio_rustls::server::TlsStream<tokio::net::TcpStream> {
        let (tcp, _) = listener.accept().await.expect("TCP accept");
        TlsAcceptor::from(Arc::new(config))
            .accept(tcp)
            .await
            .expect("TLS accept")
    }

    async fn next_frame<S: AsyncRead + Unpin>(stream: &mut S, decoder: &mut FrameDecoder) -> Frame {
        loop {
            if let Some(frame) = decoder.next_frame().expect("frame decode") {
                return frame;
            }
            let mut bytes = [0u8; 16 * 1024];
            let count = stream.read(&mut bytes).await.expect("client read");
            assert_ne!(count, 0, "client closed before next frame");
            decoder.feed(&bytes[..count]);
        }
    }

    async fn send_frame<S: AsyncWrite + Unpin>(
        stream: &mut S,
        stream_id: u32,
        flags: u8,
        payload: &[u8],
    ) {
        let frame = Frame::new(stream_id, flags, payload.to_vec())
            .encode()
            .expect("response frame");
        stream.write_all(&frame).await.expect("frame write");
        stream.flush().await.expect("frame flush");
    }

    async fn read_request_close<S: AsyncRead + Unpin>(
        stream: &mut S,
        decoder: &mut FrameDecoder,
    ) -> u32 {
        loop {
            let frame = next_frame(stream, decoder).await;
            if frame.flags & FLAG_CLOSE != 0 {
                return frame.stream_id;
            }
            assert_ne!(frame.flags & FLAG_DATA, 0, "request must carry data frames");
        }
    }

    async fn wait_for_peer_close<S: AsyncRead + Unpin>(stream: &mut S) {
        let mut bytes = [0u8; 1024];
        loop {
            match stream.read(&mut bytes).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn pending_write_times_out_without_waiting() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("TLS listener");
        let port = listener.local_addr().expect("TLS address").port();
        let (server_config, client_config, _) = fixture(port);
        let (ready_tx, ready_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let _tls = accept_tls(listener, server_config).await;
            let _ = ready_tx.send(());
            std::future::pending::<()>().await;
        });
        let body = vec![b'x'; INITIAL_WINDOW * 2];
        let request = tokio::spawn(async move {
            request_once(
                client_config,
                "127.0.0.1",
                port,
                "POST",
                "/blocked-write",
                &[],
                &body,
            )
            .await
        });
        ready_rx.await.expect("peer handshake");
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(31)).await;
        let error = request
            .await
            .expect("request task")
            .expect_err("blocked peer must time out");
        match error {
            TransportError::Io(error) => {
                assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            }
            other => panic!("expected write timeout, got {other:?}"),
        }
        server.abort();
    }

    #[tokio::test]
    async fn one_shot_response_over_initial_window_replenishes_peer_credit() {
        const BODY_BYTES: usize = INITIAL_WINDOW + 600_000;
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("TLS listener");
        let port = listener.local_addr().expect("TLS address").port();
        let (server_config, client_config, _) = fixture(port);
        let server = tokio::spawn(async move {
            let mut tls = accept_tls(listener, server_config).await;
            let mut decoder = FrameDecoder::new();
            let stream_id = read_request_close(&mut tls, &mut decoder).await;
            let mut response =
                format!("HTTP/1.1 200 OK\r\nContent-Length: {BODY_BYTES}\r\n\r\n").into_bytes();
            response.extend(vec![b'x'; BODY_BYTES]);
            let mut offset = 0;
            let mut credit = INITIAL_WINDOW;
            while offset < response.len() {
                if credit == 0 {
                    loop {
                        let frame = next_frame(&mut tls, &mut decoder).await;
                        if frame.stream_id == stream_id {
                            if let Some(grant) = frame.window_credit() {
                                credit += grant as usize;
                                break;
                            }
                        }
                    }
                }
                let count = (response.len() - offset)
                    .min(spl_core::frame::RECOMMENDED_CHUNK)
                    .min(credit);
                send_frame(
                    &mut tls,
                    stream_id,
                    FLAG_DATA,
                    &response[offset..offset + count],
                )
                .await;
                offset += count;
                credit -= count;
            }
            send_frame(&mut tls, stream_id, FLAG_CLOSE, &[]).await;
            wait_for_peer_close(&mut tls).await;
        });

        let response = request_once(client_config, "127.0.0.1", port, "GET", "/large", &[], b"")
            .await
            .expect("windowed response");
        assert_eq!(response.body, vec![b'x'; BODY_BYTES]);
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn a_completed_request_records_every_byte_and_a_completed_close() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("TLS listener");
        let port = listener.local_addr().expect("TLS address").port();
        let (server_config, _, credential) = fixture(port);
        let body = vec![b'z'; 4096];
        let server = tokio::spawn(async move {
            let mut tls = accept_tls(listener, server_config).await;
            let mut decoder = FrameDecoder::new();
            let stream_id = read_request_close(&mut tls, &mut decoder).await;
            send_frame(
                &mut tls,
                stream_id,
                FLAG_DATA | FLAG_CLOSE,
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok",
            )
            .await;
        });
        let observer = OperationObserver::new();
        let client = TransportClient::new(credential, None).expect("transport client");
        let outcome = client
            .request(
                "POST",
                "/app/devices/ingest",
                &[],
                &body,
                RequestOptions {
                    observer: Some(observer.as_ref()),
                    ..RequestOptions::default()
                },
            )
            .await
            .expect("one-shot request");
        assert_eq!(outcome.response.status, 200);
        assert_eq!(outcome.path, spl_transport::SelectedPath::Direct);
        let counts = observer.snapshot();
        assert_eq!(
            counts.request_bytes_sent,
            http::build_request("POST", "/app/devices/ingest", &[], &body).len() as u64
        );
        assert!(counts.close_completed);
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn an_interrupted_request_records_progress_without_a_completed_close() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("TLS listener");
        let port = listener.local_addr().expect("TLS address").port();
        let (server_config, _, credential) = fixture(port);
        let server = tokio::spawn(async move {
            let mut tls = accept_tls(listener, server_config).await;
            let mut decoder = FrameDecoder::new();
            let mut received = 0usize;
            while received < INITIAL_WINDOW / 2 {
                let frame = next_frame(&mut tls, &mut decoder).await;
                if frame.flags & FLAG_DATA != 0 {
                    received += frame.payload.len();
                }
            }
        });
        let observer = OperationObserver::new();
        let client = TransportClient::new(credential, None).expect("transport client");
        let body = vec![b'z'; INITIAL_WINDOW * 2];
        let error = client
            .request(
                "POST",
                "/app/devices/ingest",
                &[],
                &body,
                RequestOptions {
                    observer: Some(observer.as_ref()),
                    ..RequestOptions::default()
                },
            )
            .await
            .expect_err("interrupted request");
        assert!(matches!(error, RequestError::ReplayUnsafe(_)));
        let counts = observer.snapshot();
        assert!(counts.request_bytes_sent > 0 && counts.request_bytes_sent < body.len() as u64);
        assert!(!counts.close_completed);
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn an_early_peer_rejection_reports_an_incomplete_close() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("TLS listener");
        let port = listener.local_addr().expect("TLS address").port();
        let (server_config, _, credential) = fixture(port);
        let server = tokio::spawn(async move {
            let mut tls = accept_tls(listener, server_config).await;
            let mut decoder = FrameDecoder::new();
            let (stream_id, mut received) = loop {
                let frame = next_frame(&mut tls, &mut decoder).await;
                if frame.flags & FLAG_DATA != 0 {
                    break (frame.stream_id, frame.payload.len());
                }
            };
            while received < INITIAL_WINDOW {
                let frame = next_frame(&mut tls, &mut decoder).await;
                assert_eq!(frame.stream_id, stream_id);
                assert_ne!(frame.flags & FLAG_DATA, 0);
                received += frame.payload.len();
            }
            assert_eq!(received, INITIAL_WINDOW);
            send_frame(
                &mut tls,
                stream_id,
                FLAG_DATA | FLAG_CLOSE,
                b"HTTP/1.1 413 Payload Too Large\r\nContent-Length: 3\r\n\r\nbig",
            )
            .await;
            wait_for_peer_close(&mut tls).await;
        });
        let observer = OperationObserver::new();
        let client = TransportClient::new(credential, None).expect("transport client");
        let outcome = client
            .request(
                "POST",
                "/app/devices/ingest",
                &[],
                &vec![b'z'; INITIAL_WINDOW * 2],
                RequestOptions {
                    observer: Some(observer.as_ref()),
                    ..RequestOptions::default()
                },
            )
            .await
            .expect("early HTTP rejection is a response");
        assert_eq!(outcome.response.status, 413);
        assert!(observer.request_bytes_sent() > 0);
        assert!(!observer.close_completed());
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn one_shot_over_window_writes_one_flow_control_reset_before_error() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("TLS listener");
        let port = listener.local_addr().expect("TLS address").port();
        let (server_config, client_config, _) = fixture(port);
        let server = tokio::spawn(async move {
            let mut tls = accept_tls(listener, server_config).await;
            let mut decoder = FrameDecoder::new();
            let stream_id = read_request_close(&mut tls, &mut decoder).await;
            send_frame(
                &mut tls,
                stream_id,
                FLAG_DATA,
                &vec![b'x'; INITIAL_WINDOW + 19],
            )
            .await;
            let reset = next_frame(&mut tls, &mut decoder).await;
            assert_eq!(reset.stream_id, stream_id);
            assert_eq!(reset.flags, FLAG_RESET);
            assert_eq!(reset.payload, vec![RESET_FLOW_CONTROL_ERROR]);
        });
        let error = request_once(
            client_config,
            "127.0.0.1",
            port,
            "GET",
            "/over-window",
            &[],
            b"",
        )
        .await
        .expect_err("over-window response");
        assert!(matches!(error, TransportError::Mux(MuxError::FlowControl)));
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn one_shot_excess_send_credit_writes_flow_control_reset_before_error() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("TLS listener");
        let port = listener.local_addr().expect("TLS address").port();
        let (server_config, client_config, _) = fixture(port);
        let server = tokio::spawn(async move {
            let mut tls = accept_tls(listener, server_config).await;
            let mut decoder = FrameDecoder::new();
            let mut stream_id = None;
            let mut data = 0usize;
            while data < INITIAL_WINDOW {
                let frame = next_frame(&mut tls, &mut decoder).await;
                if frame.flags & FLAG_DATA != 0 {
                    stream_id = Some(frame.stream_id);
                    data += frame.payload.len();
                }
            }
            let stream_id = stream_id.expect("request stream");
            send_frame(&mut tls, stream_id, FLAG_WINDOW, &u32::MAX.to_be_bytes()).await;
            let reset = next_frame(&mut tls, &mut decoder).await;
            assert_eq!(reset.stream_id, stream_id);
            assert_eq!(reset.flags, FLAG_RESET);
            assert_eq!(reset.payload, vec![RESET_FLOW_CONTROL_ERROR]);
        });
        let error = request_once(
            client_config,
            "127.0.0.1",
            port,
            "POST",
            "/excess-credit",
            &[],
            &vec![b'x'; INITIAL_WINDOW + 257],
        )
        .await
        .expect_err("excess credit");
        assert!(matches!(error, TransportError::Mux(MuxError::FlowControl)));
        server.await.expect("server task");
    }
}
