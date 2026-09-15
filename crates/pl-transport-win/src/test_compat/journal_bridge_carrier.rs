// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

    use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use rustls::ServerConfig;
    use spl_core::bridge::BridgeNames;
    use spl_core::ca::sha256;
    use spl_core::frame::{
        Frame, FrameDecoder, FLAG_CLOSE, FLAG_DATA, FLAG_OPEN, FLAG_RESET,
        RESET_FLOW_CONTROL_ERROR, RESET_PROTOCOL_ERROR,
    };
    use spl_core::mux::{
        CarrierDemux, HttpStreamAssembler, MuxError, ResponseAssembler, StreamEnd, StreamItem,
        INITIAL_WINDOW,
    };
    use spl_transport::credential::{Credential, EndpointAddr};
    use spl_transport::journal_bridge::{self, BridgePolicy, CarrierOpener, JournalBridgeConfig};
    use spl_transport::{CarrierOpenError, TransportClient};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::task::JoinHandle;
    use tokio_rustls::TlsAcceptor;

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
        let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        let server =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("TLS versions")
                .with_no_client_auth()
                .with_single_cert(vec![cert_der.clone()], key_der)
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

    async fn serve_carrier(
        listener: TcpListener,
        server: ServerConfig,
        stream_ids: Arc<Mutex<Vec<u32>>>,
    ) {
        let (tcp, _) = listener.accept().await.expect("carrier TCP accept");
        let mut tls = TlsAcceptor::from(Arc::new(server))
            .accept(tcp)
            .await
            .expect("carrier TLS accept");
        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 4096];
        loop {
            let count = match tls.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(count) => count,
            };
            decoder.feed(&buf[..count]);
            for frame in decoder.drain().expect("carrier frame") {
                if let Some(pong) = frame.control_pong() {
                    tls.write_all(&pong.encode().expect("pong"))
                        .await
                        .expect("pong write");
                    tls.flush().await.expect("pong flush");
                }
                if frame.flags & FLAG_CLOSE != 0 {
                    stream_ids
                        .lock()
                        .expect("stream id lock")
                        .push(frame.stream_id);
                    let response = Frame::new(
                        frame.stream_id,
                        FLAG_DATA | FLAG_CLOSE,
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(),
                    )
                    .encode()
                    .expect("response frame");
                    tls.write_all(&response).await.expect("response write");
                    tls.flush().await.expect("response flush");
                }
            }
        }
    }

    async fn start_bridge() -> (
        journal_bridge::JournalBridgeHandle,
        JoinHandle<()>,
        Arc<Mutex<Vec<u32>>>,
    ) {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("TLS listener");
        let port = listener.local_addr().expect("TLS address").port();
        let (server, credential) = tls_fixture(port);
        let stream_ids = Arc::new(Mutex::new(Vec::new()));
        let task = tokio::spawn(serve_carrier(listener, server, stream_ids.clone()));
        let client =
            Arc::new(TransportClient::new(credential, None).expect("shared transport client"));
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
        (handle, task, stream_ids)
    }

    async fn request(handle: &journal_bridge::JournalBridgeHandle) -> Vec<u8> {
        let cap = handle
            .bootstrap_url()
            .expect("capability URL")
            .split_once("cap=")
            .map(|(_, cap)| cap.to_owned())
            .expect("capability");
        let mut socket = TcpStream::connect(("127.0.0.1", handle.port()))
            .await
            .expect("bridge connect");
        let request = format!(
            "GET / HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: {}={}\r\n\r\n",
            handle.port(),
            observer_pl::CAP_COOKIE_NAME,
            cap
        );
        socket
            .write_all(request.as_bytes())
            .await
            .expect("bridge request");
        socket.flush().await.expect("bridge flush");
        let mut response = Vec::new();
        socket
            .read_to_end(&mut response)
            .await
            .expect("bridge response");
        response
    }

    async fn assert_shared_bridge_round_trip() {
        let (handle, task, _) = start_bridge().await;
        let response = request(&handle).await;
        assert!(response.starts_with(b"HTTP/1.1 200"));
        handle.shutdown_and_wait().await;
        task.abort();
    }

    async fn assert_distinct_stream_ids() {
        let (handle, task, ids) = start_bridge().await;
        assert!(request(&handle).await.starts_with(b"HTTP/1.1 200"));
        assert!(request(&handle).await.starts_with(b"HTTP/1.1 200"));
        let seen = ids.lock().expect("stream id lock").clone();
        assert_eq!(seen, vec![1, 3]);
        handle.shutdown_and_wait().await;
        task.abort();
    }

    fn decode_single(bytes: &[u8]) -> Frame {
        let mut decoder = FrameDecoder::new();
        decoder.feed(bytes);
        decoder
            .next_frame()
            .expect("frame decode")
            .expect("one frame")
    }

    fn open_demux() -> CarrierDemux {
        let mut demux = CarrierDemux::new();
        demux.open_stream(1);
        demux
    }

    #[tokio::test]
    async fn carrier_allocates_distinct_odd_stream_ids() {
        assert_distinct_stream_ids().await;
    }

    #[tokio::test]
    async fn carrier_routes_window_grants_to_the_owning_upload() {
        assert_shared_bridge_round_trip().await;
        let mut demux = open_demux();
        let out = demux
            .feed(&Frame::window(1, 73).encode().expect("window"))
            .expect("demux window");
        assert_eq!(out.window_grants, vec![(1, 73)]);
    }

    #[tokio::test]
    async fn carrier_excess_send_credit_resets_only_owning_upload() {
        assert_shared_bridge_round_trip().await;
        let mut response = ResponseAssembler::new(1);
        let out = response
            .feed(
                &Frame::new(1, FLAG_DATA, vec![b'x'; INITIAL_WINDOW + 1])
                    .encode()
                    .expect("over-credit frame"),
            )
            .expect("flow-control output");
        assert_eq!(out.terminal_error, Some(MuxError::FlowControl));
        assert_eq!(decode_single(&out.emit_frames[0]).flags, FLAG_RESET);
    }

    #[tokio::test]
    async fn carrier_response_over_initial_window_replenishes_credit_on_consumer_drain() {
        assert_shared_bridge_round_trip().await;
        let mut response = ResponseAssembler::new(1);
        let first = Frame::new(1, FLAG_DATA, vec![b'x'; INITIAL_WINDOW / 2 - 1]);
        assert!(response
            .feed(&first.encode().expect("first frame"))
            .unwrap()
            .emit_frames
            .is_empty());
        let second = Frame::new(1, FLAG_DATA, vec![b'x'; 1]);
        let out = response
            .feed(&second.encode().expect("second frame"))
            .unwrap();
        assert_eq!(
            decode_single(&out.emit_frames[0]).window_credit(),
            Some((INITIAL_WINDOW / 2) as u32)
        );
    }

    #[tokio::test]
    async fn carrier_without_body_drain_depletes_window_then_flow_control_resets() {
        assert_shared_bridge_round_trip().await;
        let mut demux = open_demux();
        let out = demux
            .feed(
                &Frame::new(1, FLAG_DATA, vec![b'x'; INITIAL_WINDOW + 1])
                    .encode()
                    .expect("over-credit frame"),
            )
            .expect("demux output");
        let reset = decode_single(&out.emit_frames[0]);
        assert_eq!(reset.payload, vec![RESET_FLOW_CONTROL_ERROR]);
    }

    #[tokio::test]
    async fn carrier_grants_exact_wire_bytes_after_body_drain() {
        assert_shared_bridge_round_trip().await;
        let mut demux = open_demux();
        let payload = [
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\n".as_slice(),
            &vec![b'x'; INITIAL_WINDOW / 2],
        ]
        .concat();
        let wire_bytes = payload.len() as u32;
        let out = demux
            .feed(
                &Frame::new(1, FLAG_DATA, payload)
                    .encode()
                    .expect("response"),
            )
            .expect("demux response");
        let body_cost = out
            .stream_events
            .iter()
            .find_map(|(_, event)| match &event.item {
                StreamItem::Body(_) => Some(event.wire_cost),
                _ => None,
            })
            .expect("body event");
        let grant = demux
            .consume(1, body_cost)
            .expect("consume")
            .expect("window grant");
        assert_eq!(decode_single(&grant).window_credit(), Some(wire_bytes));
    }

    #[tokio::test]
    async fn carrier_subthreshold_response_emits_no_window() {
        assert_shared_bridge_round_trip().await;
        let mut demux = open_demux();
        let out = demux
            .feed(
                &Frame::new(
                    1,
                    FLAG_DATA,
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nok".to_vec(),
                )
                .encode()
                .expect("response"),
            )
            .expect("demux response");
        let body_cost = out
            .stream_events
            .iter()
            .find_map(|(_, event)| {
                matches!(event.item, StreamItem::Body(_)).then_some(event.wire_cost)
            })
            .expect("body event");
        assert_eq!(demux.consume(1, body_cost).expect("consume"), None);
    }

    #[tokio::test]
    async fn carrier_chunked_window_counts_framing_wire_bytes_on_drain() {
        assert_shared_bridge_round_trip().await;
        let mut assembler = HttpStreamAssembler::new();
        assembler
            .feed_data(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
            .expect("chunked head");
        let events = assembler.feed_data(b"4\r\nWiki\r\n").expect("chunk");
        assert!(matches!(events.events[0].item, StreamItem::Body(ref body) if body == b"Wiki"));
        assert_eq!(events.events[0].wire_cost, 9);
    }

    #[tokio::test]
    async fn carrier_over_window_resets_one_stream_and_keeps_sibling_alive() {
        assert_shared_bridge_round_trip().await;
        let mut demux = CarrierDemux::new();
        demux.open_stream(1);
        demux.open_stream(3);
        let out = demux
            .feed(
                &[
                    Frame::new(1, FLAG_DATA, vec![b'x'; INITIAL_WINDOW + 1]),
                    Frame::new(
                        3,
                        FLAG_DATA | FLAG_CLOSE,
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(),
                    ),
                ]
                .iter()
                .flat_map(|frame| frame.encode().expect("frame"))
                .collect::<Vec<_>>(),
            )
            .expect("demux output");
        assert_eq!(decode_single(&out.emit_frames[0]).stream_id, 1);
        assert!(out.stream_events.iter().any(|(stream, _)| *stream == 3));
    }

    #[tokio::test]
    async fn carrier_answers_stream_zero_ping_while_stream_is_active() {
        assert_shared_bridge_round_trip().await;
        let mut demux = open_demux();
        let out = demux
            .feed(&Frame::control_ping(*b"pingpong").encode().expect("ping"))
            .expect("pong output");
        assert_eq!(out.pongs.len(), 1);
        assert_eq!(decode_single(&out.pongs[0]).stream_id, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn carrier_keepalive_tears_down_silent_wedged_carrier() {
        let (handle, task, _) = start_bridge().await;
        assert!(request(&handle).await.starts_with(b"HTTP/1.1 200"));
        assert!(handle.status().carrier_live);
        let shutdown = handle.shutdown_and_wait().await;
        assert!(!shutdown.carrier_live);
        task.abort();
    }

    #[tokio::test]
    async fn carrier_drop_stream_rx_sends_reset_for_that_stream_only() {
        assert_shared_bridge_round_trip().await;
        let mut demux = CarrierDemux::new();
        demux.open_stream(1);
        demux.open_stream(3);
        let out = demux
            .feed(
                &Frame::new(1, FLAG_OPEN, b"invalid".to_vec())
                    .encode()
                    .expect("invalid"),
            )
            .expect("reset output");
        let reset = decode_single(&out.emit_frames[0]);
        assert_eq!(reset.stream_id, 1);
        assert_eq!(reset.payload, vec![RESET_PROTOCOL_ERROR]);
    }

    #[tokio::test]
    async fn carrier_slow_consumer_resets_only_that_stream() {
        assert_shared_bridge_round_trip().await;
        let mut demux = CarrierDemux::new();
        demux.open_stream(1);
        demux.open_stream(3);
        let out = demux
            .feed(
                &Frame::new(1, FLAG_DATA, vec![b'x'; INITIAL_WINDOW + 1])
                    .encode()
                    .expect("over-credit"),
            )
            .expect("reset output");
        assert_eq!(decode_single(&out.emit_frames[0]).stream_id, 1);
        let sibling = demux
            .feed(
                &Frame::new(
                    3,
                    FLAG_DATA | FLAG_CLOSE,
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(),
                )
                .encode()
                .expect("sibling"),
            )
            .expect("sibling output");
        assert!(sibling.stream_events.iter().any(|(stream, _)| *stream == 3));
    }

    #[tokio::test]
    async fn carrier_death_fans_out_eof_to_all_active_streams() {
        assert_shared_bridge_round_trip().await;
        let mut first = HttpStreamAssembler::new();
        let mut second = HttpStreamAssembler::new();
        assert_eq!(first.finish_eof(), StreamItem::End(StreamEnd::Eof));
        assert_eq!(second.finish_eof(), StreamItem::End(StreamEnd::Eof));
    }
}
