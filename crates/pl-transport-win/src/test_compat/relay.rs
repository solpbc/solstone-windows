// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use spl_core::pairlink::RelayPairLink;
    use spl_transport::{pair_over_relay, RelayError, TransportError};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    fn relay_link(origin: String) -> RelayPairLink {
        RelayPairLink {
            s: [0x01; 8],
            ca_fp_spki: vec![0; 16],
            relay_origin: origin,
        }
    }

    async fn read_upgrade_headers(stream: &mut TcpStream) -> Vec<u8> {
        let mut request = Vec::new();
        let mut buffer = [0u8; 1024];
        loop {
            let count = stream
                .read(&mut buffer)
                .await
                .expect("relay upgrade request");
            assert_ne!(count, 0, "upgrade request ended before headers");
            request.extend_from_slice(&buffer[..count]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                return request;
            }
        }
    }

    async fn reject_upgrade(mut stream: TcpStream, status: u16) {
        let _ = read_upgrade_headers(&mut stream).await;
        stream
            .write_all(
                format!(
                    "HTTP/1.1 {status} Rejected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .expect("relay rejection");
    }

    #[tokio::test]
    async fn build_pair_dial_request_sets_pair_key_without_authorization() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("relay listener");
        let origin = format!("http://{}", listener.local_addr().expect("relay address"));
        let headers = Arc::new(Mutex::new(None));
        let seen = headers.clone();
        let server = tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.expect("relay accept");
            let request = read_upgrade_headers(&mut tcp).await;
            let text = std::str::from_utf8(&request).expect("ASCII upgrade request");
            let header = |name: &str| {
                text.lines().find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case(name)
                        .then(|| value.trim().to_owned())
                })
            };
            *seen.lock().expect("header lock") =
                Some((header("sec-pair-key"), header("authorization")));
            tcp.write_all(
                b"HTTP/1.1 401 Rejected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .expect("relay rejection");
        });

        let error = pair_over_relay(&relay_link(origin), "compat-test", &serde_json::Map::new())
            .await
            .expect_err("pair-window rejection");
        assert!(matches!(
            error,
            TransportError::Relay(RelayError::PairWindowClosed)
        ));
        server.await.expect("relay task");
        let (pair_key, authorization) = headers
            .lock()
            .expect("header lock")
            .take()
            .expect("pair request observed");
        assert!(pair_key.is_some(), "pair dial must carry Sec-Pair-Key");
        assert!(
            authorization.is_none(),
            "pair dial must not carry Authorization"
        );
    }

    #[tokio::test]
    async fn pair_upgrade_401_maps_to_pair_window_closed() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("relay listener");
        let origin = format!("http://{}", listener.local_addr().expect("relay address"));
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("relay accept");
            reject_upgrade(tcp, 401).await;
        });

        let error = pair_over_relay(&relay_link(origin), "compat-test", &serde_json::Map::new())
            .await
            .expect_err("pair-window rejection");
        assert!(matches!(
            error,
            TransportError::Relay(RelayError::PairWindowClosed)
        ));
        server.await.expect("relay task");
    }

    #[tokio::test]
    async fn pair_upgrade_402_maps_to_unpaid() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("relay listener");
        let origin = format!("http://{}", listener.local_addr().expect("relay address"));
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("relay accept");
            reject_upgrade(tcp, 402).await;
        });

        let error = pair_over_relay(&relay_link(origin), "compat-test", &serde_json::Map::new())
            .await
            .expect_err("402 rejection");
        assert!(matches!(
            error,
            TransportError::Relay(RelayError::UpgradeRejected)
        ));
        server.await.expect("relay task");
    }
}
