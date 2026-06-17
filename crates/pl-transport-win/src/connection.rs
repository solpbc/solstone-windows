// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! One PL request over a fresh framed-mTLS connection.
//!
//! Each observer request opens a TCP connection to the journal, completes the
//! TLS 1.3 handshake (CA-fp pinned via the supplied [`ClientConfig`]), opens one
//! dialer stream, writes the HTTP request as `OPEN|DATA…|CLOSE` frames, and
//! reads response frames until the stream closes — answering any control `PING`
//! with a `PONG`. Connection-per-request keeps the mux trivially correct (no
//! concurrent-stream bookkeeping); the cost is a handshake per call, which is
//! fine at observer cadence (one ingest per segment, one heartbeat per 15s).

use std::sync::Arc;
use std::time::Duration;

use observer_pl::frame::FrameDialer;
use observer_pl::http::{self, HttpResponse};
use observer_pl::mux::{request_frames, ResponseAssembler};
use rustls::ClientConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use crate::tls::pinned_server_name;
use crate::TransportError;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_BUF: usize = 64 * 1024;

/// Send one HTTP request over a fresh PL connection and return the response.
/// `headers` are the caller's extra headers (auth, content-type); framing-owned
/// headers are added by [`http::build_request`].
pub async fn request_once(
    config: Arc<ClientConfig>,
    host: &str,
    port: u16,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> Result<HttpResponse, TransportError> {
    let tcp = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port)))
        .await
        .map_err(|_| {
            TransportError::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("connect to {host}:{port} timed out"),
            ))
        })??;
    tcp.set_nodelay(true).ok();

    let connector = TlsConnector::from(config);
    let mut tls = connector
        .connect(pinned_server_name(), tcp)
        .await
        .map_err(|e| TransportError::Tls(format!("handshake to {host}:{port}: {e}")))?;

    let mut dialer = FrameDialer::default();
    let stream_id = dialer.allocate();
    let request_bytes = http::build_request(method, path, headers, body);
    let frames = request_frames(stream_id, &request_bytes)
        .map_err(|e| TransportError::Mux(observer_pl::mux::MuxError::Frame(e)))?;
    tls.write_all(&frames).await?;
    tls.flush().await?;

    let mut assembler = ResponseAssembler::new(stream_id);
    let mut buf = vec![0u8; READ_BUF];
    while !assembler.is_closed() {
        let n = tls.read(&mut buf).await?;
        if n == 0 {
            break; // peer closed the connection
        }
        let pongs = assembler.feed(&buf[..n])?;
        let wrote_pong = !pongs.is_empty();
        for pong in pongs {
            tls.write_all(&pong).await?;
        }
        if wrote_pong {
            tls.flush().await?;
        }
    }
    // Best-effort clean close.
    let _ = tls.shutdown().await;

    Ok(assembler.into_response()?)
}
