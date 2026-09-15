// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::time::Duration;

    use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use rustls::ServerConfig;
    use spl_core::bridge::BridgeNames;
    use spl_core::ca::sha256;
    use spl_core::frame::{
        Frame, FrameDecoder, FLAG_CLOSE, FLAG_DATA, FLAG_RESET, FLAG_WINDOW,
        RESET_FLOW_CONTROL_ERROR,
    };
    use spl_core::mux::INITIAL_WINDOW;
    use spl_transport::credential::{Credential, EndpointAddr};
    use spl_transport::journal_bridge::{self, BridgePolicy, CarrierOpener, JournalBridgeConfig};
    use spl_transport::{CarrierOpenError, TransportClient};
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::oneshot;
    use tokio::task::JoinHandle;
    use tokio_rustls::TlsAcceptor;

    type ServerStream = tokio_rustls::server::TlsStream<TcpStream>;

    struct TestOpener {
        client: Arc<TransportClient>,
    }

    impl CarrierOpener for TestOpener {
        fn proxy_headers(
            &self,
            upstream_headers: &[(String, String)],
        ) -> Result<Vec<(String, String)>, spl_transport::TransportError> {
            Ok(upstream_headers.to_vec())
        }

        fn dial_carrier(
            &self,
        ) -> Pin<
            Box<
                dyn Future<
                        Output = Result<
                            spl_transport::DialedCarrier,
                            spl_transport::TransportError,
                        >,
                    > + Send
                    + '_,
            >,
        > {
            Box::pin(async move {
                self.client
                    .open_carrier(None)
                    .await
                    .map_err(|error| match error {
                        CarrierOpenError::Transport(error) => error,
                        CarrierOpenError::RelayDisabled
                        | CarrierOpenError::RelayRetired
                        | CarrierOpenError::PublicationRejected
                        | CarrierOpenError::PublicationIndeterminate => {
                            spl_transport::TransportError::NoEndpoint
                        }
                    })
            })
        }
    }

    fn tls_fixture(port: u16) -> (ServerConfig, Credential) {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).expect("server key");
        let params = CertificateParams::new(vec!["spl.local".to_owned()]).expect("server params");
        let cert = params.self_signed(&key).expect("server certificate");
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
        let fingerprint = sha256(cert_der.as_ref());
        let credential = Credential {
            client_key_pem: key.serialize_pem(),
            client_cert_pem: cert.pem(),
            ca_chain_pem: vec![cert.pem()],
            ca_fp_prefix: fingerprint[..16].to_vec(),
            instance_id: "test-instance".into(),
            home_label: "Home".into(),
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
        (server, credential)
    }

    async fn start_bridge<F, Fut>(serve: F) -> (journal_bridge::JournalBridgeHandle, JoinHandle<()>)
    where
        F: FnOnce(ServerStream) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("TLS listener");
        let port = listener.local_addr().expect("TLS address").port();
        let (server, credential) = tls_fixture(port);
        let task = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("carrier TCP accept");
            let tls = TlsAcceptor::from(Arc::new(server))
                .accept(tcp)
                .await
                .expect("carrier TLS accept");
            serve(tls).await;
        });
        let client = Arc::new(TransportClient::new(credential, None).expect("transport client"));
        let handle = journal_bridge::start(JournalBridgeConfig {
            opener: Arc::new(TestOpener { client }),
            bridge_names: BridgeNames {
                capability_cookie_name: observer_pl::CAP_COOKIE_NAME.into(),
                upstream_cookie_prefix: observer_pl::UPSTREAM_COOKIE_PREFIX.into(),
                observer_header_name: observer_pl::OBSERVER_HANDLE_HEADER.to_ascii_lowercase(),
                protocol_version_header_name: observer_pl::PROTOCOL_VERSION_HEADER
                    .to_ascii_lowercase(),
            },
            endpoint_hosts: Vec::new(),
            policy: BridgePolicy::default(),
        })
        .await
        .expect("shared bridge");
        (handle, task)
    }

    async fn next_frame<S: AsyncRead + Unpin>(stream: &mut S, decoder: &mut FrameDecoder) -> Frame {
        loop {
            if let Some(frame) = decoder.next_frame().expect("frame decode") {
                return frame;
            }
            let mut bytes = [0u8; 16 * 1024];
            let count = stream.read(&mut bytes).await.expect("carrier read");
            assert_ne!(count, 0, "carrier closed before expected frame");
            decoder.feed(&bytes[..count]);
        }
    }

    async fn possible_frame<S: AsyncRead + Unpin>(
        stream: &mut S,
        decoder: &mut FrameDecoder,
    ) -> Option<Frame> {
        loop {
            if let Some(frame) = decoder.next_frame().expect("frame decode") {
                return Some(frame);
            }
            let mut bytes = [0u8; 16 * 1024];
            let count = match stream.read(&mut bytes).await {
                Ok(count) => count,
                Err(_) => return None,
            };
            if count == 0 {
                return None;
            }
            decoder.feed(&bytes[..count]);
        }
    }

    async fn send_frame<S: AsyncWrite + Unpin>(
        stream: &mut S,
        stream_id: u32,
        flags: u8,
        payload: &[u8],
    ) {
        stream
            .write_all(
                &Frame::new(stream_id, flags, payload.to_vec())
                    .encode()
                    .expect("response frame"),
            )
            .await
            .expect("carrier write");
        stream.flush().await.expect("carrier flush");
    }

    async fn request_close<S: AsyncRead + Unpin>(
        stream: &mut S,
        decoder: &mut FrameDecoder,
    ) -> u32 {
        loop {
            let frame = next_frame(stream, decoder).await;
            if frame.flags & FLAG_CLOSE != 0 {
                return frame.stream_id;
            }
        }
    }

    async fn wait_for_reset<S: AsyncRead + Unpin>(
        stream: &mut S,
        decoder: &mut FrameDecoder,
        stream_id: u32,
    ) {
        loop {
            let frame = next_frame(stream, decoder).await;
            if frame.stream_id == stream_id && frame.flags == FLAG_RESET {
                assert_eq!(frame.payload, vec![RESET_FLOW_CONTROL_ERROR]);
                return;
            }
        }
    }

    fn capability(handle: &journal_bridge::JournalBridgeHandle) -> String {
        handle
            .bootstrap_url()
            .expect("capability URL")
            .split_once("cap=")
            .map(|(_, cap)| cap.to_owned())
            .expect("capability")
    }

    async fn browser_request(
        handle: &journal_bridge::JournalBridgeHandle,
        method: &str,
        body: &[u8],
    ) -> TcpStream {
        let mut socket = TcpStream::connect(("127.0.0.1", handle.port()))
            .await
            .expect("bridge connect");
        let request = format!(
            "{method} / HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: {}={}\r\nContent-Length: {}\r\n\r\n",
            handle.port(),
            observer_pl::CAP_COOKIE_NAME,
            capability(handle),
            body.len(),
        );
        socket
            .write_all(request.as_bytes())
            .await
            .expect("browser head");
        socket.write_all(body).await.expect("browser body");
        socket.flush().await.expect("browser flush");
        socket
    }

    async fn completed_response(socket: &mut TcpStream) -> Vec<u8> {
        let mut response = Vec::new();
        socket
            .read_to_end(&mut response)
            .await
            .expect("bridge response");
        response
    }

    fn response(body: &[u8]) -> Vec<u8> {
        let mut bytes =
            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
        bytes.extend_from_slice(body);
        bytes
    }

    #[tokio::test]
    async fn carrier_allocates_distinct_odd_stream_ids() {
        let (handle, server) = start_bridge(|mut tls| async move {
            let mut decoder = FrameDecoder::new();
            let first = request_close(&mut tls, &mut decoder).await;
            send_frame(&mut tls, first, FLAG_DATA | FLAG_CLOSE, &response(b"ok")).await;
            let second = request_close(&mut tls, &mut decoder).await;
            assert_eq!((first, second), (1, 3));
            send_frame(&mut tls, second, FLAG_DATA | FLAG_CLOSE, &response(b"ok")).await;
        })
        .await;
        for _ in 0..2 {
            let mut socket = browser_request(&handle, "GET", b"").await;
            assert!(completed_response(&mut socket)
                .await
                .starts_with(b"HTTP/1.1 200"));
        }
        handle.shutdown_and_wait().await;
        server.await.expect("carrier server");
    }

    #[tokio::test]
    async fn carrier_routes_window_grants_to_the_owning_upload() {
        let body = vec![b'x'; INITIAL_WINDOW + 1024];
        let (handle, server) = start_bridge(|mut tls| async move {
            let mut decoder = FrameDecoder::new();
            let first = next_frame(&mut tls, &mut decoder).await;
            assert_ne!(first.flags & FLAG_DATA, 0);
            let stream_id = first.stream_id;
            let mut initial = first.payload.len();
            while initial < INITIAL_WINDOW {
                let frame = next_frame(&mut tls, &mut decoder).await;
                assert_eq!(frame.stream_id, stream_id);
                assert_ne!(frame.flags & FLAG_DATA, 0);
                initial += frame.payload.len();
            }
            assert_eq!(initial, INITIAL_WINDOW);
            send_frame(&mut tls, stream_id, FLAG_WINDOW, &73u32.to_be_bytes()).await;
            let grant = next_frame(&mut tls, &mut decoder).await;
            assert_eq!(grant.stream_id, stream_id);
            assert_eq!(grant.flags & FLAG_DATA, FLAG_DATA);
            assert_eq!(grant.payload.len(), 73);
            send_frame(&mut tls, stream_id, FLAG_WINDOW, &2048u32.to_be_bytes()).await;
            let _ = request_close(&mut tls, &mut decoder).await;
            send_frame(
                &mut tls,
                stream_id,
                FLAG_DATA | FLAG_CLOSE,
                &response(b"ok"),
            )
            .await;
        })
        .await;
        let mut socket = browser_request(&handle, "POST", &body).await;
        assert!(completed_response(&mut socket)
            .await
            .starts_with(b"HTTP/1.1 200"));
        handle.shutdown_and_wait().await;
        server.await.expect("carrier server");
    }

    #[tokio::test]
    async fn carrier_excess_send_credit_resets_only_owning_upload() {
        let (handle, server) = start_bridge(|mut tls| async move {
            let mut decoder = FrameDecoder::new();
            let request = next_frame(&mut tls, &mut decoder).await;
            send_frame(
                &mut tls,
                request.stream_id,
                FLAG_WINDOW,
                &u32::MAX.to_be_bytes(),
            )
            .await;
            wait_for_reset(&mut tls, &mut decoder, request.stream_id).await;
        })
        .await;
        let socket = browser_request(&handle, "POST", &vec![b'x'; INITIAL_WINDOW + 9]).await;
        server.await.expect("carrier server");
        drop(socket);
        handle.shutdown_and_wait().await;
    }

    #[tokio::test]
    async fn carrier_response_over_initial_window_replenishes_credit_on_consumer_drain() {
        let body = vec![b'y'; INITIAL_WINDOW + 1024];
        let expected = body.clone();
        let (release_tx, release_rx) = oneshot::channel();
        let (handle, server) = start_bridge(move |mut tls| async move {
            let mut decoder = FrameDecoder::new();
            let stream_id = request_close(&mut tls, &mut decoder).await;
            let wire = response(&body);
            let mut offset = 0;
            let mut credit = INITIAL_WINDOW;
            while offset < wire.len() {
                if credit == 0 {
                    let frame = next_frame(&mut tls, &mut decoder).await;
                    if frame.stream_id == stream_id {
                        credit += frame.window_credit().expect("response credit") as usize;
                    }
                    continue;
                }
                let count = (wire.len() - offset).min(16 * 1024).min(credit);
                send_frame(
                    &mut tls,
                    stream_id,
                    FLAG_DATA,
                    &wire[offset..offset + count],
                )
                .await;
                offset += count;
                credit -= count;
            }
            send_frame(&mut tls, stream_id, FLAG_CLOSE, b"").await;
            let _ = release_rx.await;
        })
        .await;
        let mut socket = browser_request(&handle, "GET", b"").await;
        let received = completed_response(&mut socket).await;
        assert!(received.ends_with(&expected));
        let _ = release_tx.send(());
        handle.shutdown_and_wait().await;
        server.await.expect("carrier server");
    }

    #[tokio::test]
    async fn carrier_without_body_drain_depletes_window_then_flow_control_resets() {
        let (handle, server) = start_bridge(|mut tls| async move {
            let mut decoder = FrameDecoder::new();
            let stream_id = request_close(&mut tls, &mut decoder).await;
            send_frame(
                &mut tls,
                stream_id,
                FLAG_DATA,
                &vec![b'x'; INITIAL_WINDOW + 1],
            )
            .await;
            wait_for_reset(&mut tls, &mut decoder, stream_id).await;
        })
        .await;
        let _socket = browser_request(&handle, "GET", b"").await;
        server.await.expect("carrier server");
        handle.shutdown_and_wait().await;
    }

    #[tokio::test]
    async fn carrier_grants_exact_wire_bytes_after_body_drain() {
        let body = vec![b'x'; INITIAL_WINDOW / 2];
        let (handle, server) = start_bridge(move |mut tls| async move {
            let mut decoder = FrameDecoder::new();
            let stream_id = request_close(&mut tls, &mut decoder).await;
            let wire = response(&body);
            let expected_credit = wire.len() as u32;
            send_frame(&mut tls, stream_id, FLAG_DATA, &wire).await;
            let grant = next_frame(&mut tls, &mut decoder).await;
            assert_eq!(grant.stream_id, stream_id);
            assert_eq!(grant.window_credit(), Some(expected_credit));
            send_frame(&mut tls, stream_id, FLAG_CLOSE, b"").await;
        })
        .await;
        let mut socket = browser_request(&handle, "GET", b"").await;
        let _ = completed_response(&mut socket).await;
        handle.shutdown_and_wait().await;
        server.await.expect("carrier server");
    }

    #[tokio::test]
    async fn carrier_subthreshold_response_emits_no_window() {
        let (handle, server) = start_bridge(|mut tls| async move {
            let mut decoder = FrameDecoder::new();
            let stream_id = request_close(&mut tls, &mut decoder).await;
            send_frame(
                &mut tls,
                stream_id,
                FLAG_DATA | FLAG_CLOSE,
                &response(b"ok"),
            )
            .await;
            let unexpected = tokio::time::timeout(
                Duration::from_millis(100),
                possible_frame(&mut tls, &mut decoder),
            )
            .await;
            if let Ok(Some(frame)) = unexpected {
                assert!(
                    frame.window_credit().is_none(),
                    "small response must not replenish credit"
                );
            }
        })
        .await;
        let mut socket = browser_request(&handle, "GET", b"").await;
        let _ = completed_response(&mut socket).await;
        handle.shutdown_and_wait().await;
        server.await.expect("carrier server");
    }

    #[tokio::test]
    async fn carrier_chunked_window_counts_framing_wire_bytes_on_drain() {
        let chunks = b"4\r\nWiki\r\n".repeat((INITIAL_WINDOW / 2 / 9) + 1);
        let (handle, server) = start_bridge(move |mut tls| async move {
            let mut decoder = FrameDecoder::new();
            let stream_id = request_close(&mut tls, &mut decoder).await;
            let mut wire = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
            wire.extend_from_slice(&chunks);
            let expected_credit = wire.len() as u32;
            send_frame(&mut tls, stream_id, FLAG_DATA, &wire).await;
            let grant = loop {
                let frame = next_frame(&mut tls, &mut decoder).await;
                if frame.stream_id == stream_id && frame.window_credit().is_some() {
                    break frame;
                }
            };
            assert_eq!(grant.window_credit(), Some(expected_credit));
            send_frame(&mut tls, stream_id, FLAG_DATA | FLAG_CLOSE, b"0\r\n\r\n").await;
        })
        .await;
        let mut socket = browser_request(&handle, "GET", b"").await;
        let _ = completed_response(&mut socket).await;
        handle.shutdown_and_wait().await;
        server.await.expect("carrier server");
    }

    #[tokio::test]
    async fn carrier_over_window_resets_one_stream_and_keeps_sibling_alive() {
        let (handle, server) = start_bridge(|mut tls| async move {
            let mut decoder = FrameDecoder::new();
            let first = request_close(&mut tls, &mut decoder).await;
            let second = request_close(&mut tls, &mut decoder).await;
            send_frame(&mut tls, first, FLAG_DATA, &vec![b'x'; INITIAL_WINDOW + 1]).await;
            wait_for_reset(&mut tls, &mut decoder, first).await;
            send_frame(&mut tls, second, FLAG_DATA | FLAG_CLOSE, &response(b"ok")).await;
        })
        .await;
        let mut first = browser_request(&handle, "GET", b"").await;
        let mut second = browser_request(&handle, "GET", b"").await;
        let _ = completed_response(&mut first).await;
        assert!(completed_response(&mut second)
            .await
            .starts_with(b"HTTP/1.1 200"));
        handle.shutdown_and_wait().await;
        server.await.expect("carrier server");
    }

    #[tokio::test]
    async fn carrier_answers_stream_zero_ping_while_stream_is_active() {
        let (handle, server) = start_bridge(|mut tls| async move {
            let mut decoder = FrameDecoder::new();
            let request = next_frame(&mut tls, &mut decoder).await;
            assert_ne!(request.stream_id, 0);
            tls.write_all(&Frame::control_ping(*b"pingpong").encode().expect("ping"))
                .await
                .expect("ping write");
            tls.flush().await.expect("ping flush");
            loop {
                let Some(frame) = possible_frame(&mut tls, &mut decoder).await else {
                    return;
                };
                if frame.stream_id == 0 {
                    assert_eq!(frame.control_pong_nonce(), Some(*b"pingpong"));
                    return;
                }
            }
        })
        .await;
        let _socket = browser_request(&handle, "POST", &vec![b'x'; INITIAL_WINDOW + 1]).await;
        server.await.expect("carrier server");
        handle.shutdown_and_wait().await;
    }

    #[tokio::test(start_paused = true)]
    async fn carrier_keepalive_tears_down_silent_wedged_carrier() {
        let (handle, server) = start_bridge(|mut tls| async move {
            let mut decoder = FrameDecoder::new();
            let stream_id = request_close(&mut tls, &mut decoder).await;
            send_frame(
                &mut tls,
                stream_id,
                FLAG_DATA | FLAG_CLOSE,
                &response(b"ok"),
            )
            .await;
            loop {
                let Some(frame) = possible_frame(&mut tls, &mut decoder).await else {
                    return;
                };
                if frame.stream_id == 0 {
                    assert!(
                        frame.control_pong_nonce().is_none(),
                        "silent peer must not answer keepalive"
                    );
                }
            }
        })
        .await;
        let mut socket = browser_request(&handle, "GET", b"").await;
        assert!(completed_response(&mut socket)
            .await
            .starts_with(b"HTTP/1.1 200"));
        assert!(handle.status().carrier_live);
        for _ in 0..6 {
            tokio::time::advance(Duration::from_secs(31)).await;
            tokio::task::yield_now().await;
        }
        assert!(
            !handle.status().carrier_live,
            "missed keepalives retire the carrier"
        );
        handle.shutdown_and_wait().await;
        server.abort();
    }

    #[tokio::test]
    async fn carrier_drop_stream_rx_sends_reset_for_that_stream_only() {
        let (handle, server) = start_bridge(|mut tls| async move {
            let mut decoder = FrameDecoder::new();
            let first = request_close(&mut tls, &mut decoder).await;
            let second = request_close(&mut tls, &mut decoder).await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            send_frame(
                &mut tls,
                first,
                FLAG_DATA,
                &response(&vec![b'x'; INITIAL_WINDOW + 1]),
            )
            .await;
            tokio::time::timeout(
                Duration::from_secs(2),
                wait_for_reset(&mut tls, &mut decoder, first),
            )
            .await
            .expect("dropped browser stream must reset its carrier stream");
            send_frame(&mut tls, second, FLAG_DATA | FLAG_CLOSE, &response(b"ok")).await;
        })
        .await;
        let first = browser_request(&handle, "GET", b"").await;
        let mut second = browser_request(&handle, "GET", b"").await;
        drop(first);
        assert!(completed_response(&mut second)
            .await
            .starts_with(b"HTTP/1.1 200"));
        handle.shutdown_and_wait().await;
        server.await.expect("carrier server");
    }

    #[tokio::test]
    async fn carrier_slow_consumer_resets_only_that_stream() {
        let (handle, server) = start_bridge(|mut tls| async move {
            let mut decoder = FrameDecoder::new();
            let first = request_close(&mut tls, &mut decoder).await;
            let second = request_close(&mut tls, &mut decoder).await;
            send_frame(&mut tls, first, FLAG_DATA, &vec![b'x'; INITIAL_WINDOW + 1]).await;
            wait_for_reset(&mut tls, &mut decoder, first).await;
            send_frame(&mut tls, second, FLAG_DATA | FLAG_CLOSE, &response(b"ok")).await;
        })
        .await;
        let _slow = browser_request(&handle, "GET", b"").await;
        let mut sibling = browser_request(&handle, "GET", b"").await;
        assert!(completed_response(&mut sibling)
            .await
            .starts_with(b"HTTP/1.1 200"));
        handle.shutdown_and_wait().await;
        server.await.expect("carrier server");
    }

    #[tokio::test]
    async fn carrier_death_fans_out_eof_to_all_active_streams() {
        let (handle, server) = start_bridge(|mut tls| async move {
            let mut decoder = FrameDecoder::new();
            let _ = request_close(&mut tls, &mut decoder).await;
            let _ = request_close(&mut tls, &mut decoder).await;
            drop(tls);
        })
        .await;
        let mut first = browser_request(&handle, "GET", b"").await;
        let mut second = browser_request(&handle, "GET", b"").await;
        let first_response = completed_response(&mut first).await;
        let second_response = completed_response(&mut second).await;
        assert!(!first_response.starts_with(b"HTTP/1.1 200"));
        assert!(!second_response.starts_with(b"HTTP/1.1 200"));
        assert!(!handle.status().carrier_live);
        handle.shutdown_and_wait().await;
        server.await.expect("carrier server");
    }
}
