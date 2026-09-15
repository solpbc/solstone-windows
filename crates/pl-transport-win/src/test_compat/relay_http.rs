// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#[cfg(test)]
mod tests {
    use std::io;
    use std::time::Duration;

    use spl_transport::relay_pairing::enroll_device;
    use spl_transport::{
        same_relay_origin, validate_relay_origin, RelayControlEndpoint, TransportError,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::oneshot;

    async fn read_control_request(stream: &mut TcpStream) {
        let mut request = Vec::new();
        let mut buffer = [0u8; 1024];
        loop {
            let count = stream.read(&mut buffer).await.expect("control request");
            assert_ne!(count, 0, "control request ended before headers");
            request.extend_from_slice(&buffer[..count]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                return;
            }
        }
    }

    async fn listener() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("relay control listener");
        let origin = format!(
            "http://{}",
            listener.local_addr().expect("relay control address")
        );
        (listener, origin)
    }

    fn rejected(error: TransportError, status: u16) {
        assert!(matches!(
            error,
            TransportError::RelayControlRejected {
                endpoint: RelayControlEndpoint::EnrollDevice,
                status: actual,
            } if actual == status
        ));
    }

    #[tokio::test]
    async fn chunked_response_split_across_reads_waits_for_eof() {
        let (listener, origin) = listener().await;
        let token = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJpc3MiOiJjb21wYXQiLCJzdWIiOiJpbnN0YW5jZTpjb21wYXQtaW5zdGFuY2UiLCJhdWQiOiJzcGwtcmVsYXkiLCJzY29wZSI6InNlc3Npb24uZGlhbCIsInZlciI6MiwiaW5zdGFuY2VfaWQiOiJjb21wYXQtaW5zdGFuY2UiLCJpYXQiOjE3MDAwMDAwMDAsImV4cCI6NDEwMjQ0NDgwMCwianRpIjoiY29tcGF0In0.testsig";
        let body = format!(
            r#"{{"device_token":"{token}","protocol_version":2,"expires_at":"2100-01-01T00:00:00Z"}}"#
        )
        .into_bytes();
        let split = body.len() / 2;
        let (partial_tx, partial_rx) = oneshot::channel();
        let (finish_tx, finish_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("control accept");
            read_control_request(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                .await
                .expect("response head");
            stream
                .write_all(format!("{:X}\r\n", split).as_bytes())
                .await
                .expect("first chunk length");
            stream
                .write_all(&body[..split])
                .await
                .expect("first chunk body");
            stream.write_all(b"\r\n").await.expect("first chunk end");
            partial_tx.send(()).expect("partial notification");
            finish_rx.await.expect("finish notification");
            stream
                .write_all(format!("{:X}\r\n", body.len() - split).as_bytes())
                .await
                .expect("second chunk length");
            stream
                .write_all(&body[split..])
                .await
                .expect("second chunk body");
            stream
                .write_all(b"\r\n0\r\n\r\n")
                .await
                .expect("terminal chunk");
            stream.shutdown().await.expect("control EOF");
        });

        let client =
            tokio::spawn(
                async move { enroll_device(&origin, "compat-instance", "attestation").await },
            );
        partial_rx.await.expect("first chunk sent");
        tokio::task::yield_now().await;
        assert!(
            !client.is_finished(),
            "a partial chunk must not complete the control operation"
        );
        finish_tx.send(()).expect("finish response");
        assert_eq!(
            client
                .await
                .expect("control task")
                .expect("completed chunked enrollment"),
            token
        );
        server.await.expect("control server");
    }

    #[test]
    fn origin_comparison_normalizes_default_ports_and_rejects_injection() {
        assert!(
            same_relay_origin("https://Relay.Example/", "https://relay.example:443")
                .expect("valid origins")
        );
        assert!(
            !same_relay_origin("http://relay.example", "https://relay.example")
                .expect("valid origins")
        );
        for origin in [
            "https://user@relay.example",
            "https://relay.example/extra",
            "https://relay.example\r\nx: y",
            "https://[not-ipv6]",
        ] {
            assert!(validate_relay_origin(origin).is_err(), "{origin}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn total_deadline_cancels_blocked_write_and_progressing_reads() {
        let (listener, origin) = listener().await;
        let (accepted_tx, accepted_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("control accept");
            read_control_request(&mut stream).await;
            let _ = accepted_tx.send(());
            std::future::pending::<()>().await;
        });
        let client =
            tokio::spawn(
                async move { enroll_device(&origin, "compat-instance", "attestation").await },
            );
        accepted_rx.await.expect("request reached relay");
        tokio::time::advance(Duration::from_secs(15)).await;
        let error = client
            .await
            .expect("control task")
            .expect_err("control deadline");
        assert!(matches!(
            error,
            TransportError::Io(error) if error.kind() == io::ErrorKind::TimedOut
        ));
        server.abort();
    }

    #[tokio::test]
    async fn complete_chunked_response_returns_without_eof() {
        let (listener, origin) = listener().await;
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("control accept");
            read_control_request(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 401 Unauthorized\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\nX-Trailer: ignored\r\n\r\n")
                .await
                .expect("chunked response");
            let mut byte = [0u8; 1];
            assert_eq!(stream.read(&mut byte).await.expect("client EOF"), 0);
        });
        rejected(
            enroll_device(&origin, "compat-instance", "attestation")
                .await
                .expect_err("401 response"),
            401,
        );
        server.await.expect("control server");
    }

    #[tokio::test]
    async fn control_body_limit_and_framing_errors_return_fixed_classifications() {
        for response in [
            b"HTTP/1.1 private-control-sentinel\r\n\r\n".to_vec(),
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n".to_vec(),
            {
                let mut response = b"HTTP/1.1 200 OK\r\nContent-Length: 70000\r\n\r\n".to_vec();
                response.extend(vec![b'x'; 70_000]);
                response
            },
        ] {
            let (listener, origin) = listener().await;
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.expect("control accept");
                read_control_request(&mut stream).await;
                stream.write_all(&response).await.expect("control response");
            });
            let error = enroll_device(&origin, "compat-instance", "attestation")
                .await
                .expect_err("malformed control response");
            assert!(matches!(error, TransportError::Pairing(_)));
            assert!(!format!("{error:?} {error}").contains("private-control-sentinel"));
            server.await.expect("control server");
        }
    }
}
