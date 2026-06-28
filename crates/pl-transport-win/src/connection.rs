// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! One PL request over a fresh framed-mTLS connection.
//!
//! Each observer request opens a TCP connection to the journal, completes the
//! TLS 1.3 handshake (CA-fp pinned via the supplied [`ClientConfig`]), opens one
//! dialer stream, and runs the **windowed** upload/response loop: it writes the
//! HTTP request as `OPEN|DATA…|CLOSE` frames but never sends more un-granted DATA
//! payload than the peer's advertised window ([`WindowedUpload`]), reading
//! inbound frames between bursts to pick up `WINDOW` grants (which unblock more
//! sending), answer control `PING`s with `PONG`s, and assemble the response —
//! full-duplex, so a multi-MiB segment streams correctly instead of stalling at
//! the 1 MiB initial window. Connection-per-request keeps the mux trivially
//! correct (no concurrent-stream bookkeeping); the cost is a handshake per call,
//! fine at observer cadence (one ingest per segment, one heartbeat per 15s).

use std::sync::Arc;
use std::time::Duration;

use observer_pl::frame::FrameDialer;
use observer_pl::http::{self, HttpResponse};
use observer_pl::mux::{MuxError, ResponseAssembler, WindowedUpload};
use rustls::ClientConfig;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use crate::tls::pinned_server_name;
use crate::TransportError;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Upper bound on a single inbound read while uploading/awaiting the response.
/// A healthy journal replenishes the window at 50% consumed and answers
/// promptly; a stall this long is a dead peer, not back-pressure — fail fast and
/// let the coordinator's backoff retry rather than hang a segment forever.
const READ_TIMEOUT: Duration = Duration::from_secs(60);
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
    let tls = connector
        .connect(pinned_server_name(), tcp)
        .await
        .map_err(|e| TransportError::Tls(format!("handshake to {host}:{port}: {e}")))?;

    run_request_over_stream(tls, method, path, headers, body).await
}

pub(crate) async fn run_request_over_stream<S>(
    mut stream: S,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> Result<HttpResponse, TransportError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut dialer = FrameDialer::default();
    let stream_id = dialer.allocate();
    let request_bytes = http::build_request(method, path, headers, body);
    let mut upload = WindowedUpload::new(stream_id, &request_bytes);
    let mut assembler = ResponseAssembler::new(stream_id);

    let mut buf = vec![0u8; READ_BUF];
    loop {
        // Send everything the current window permits — unless the peer has
        // already responded and closed our stream (e.g. an early rejection),
        // in which case there is nothing more worth sending.
        if !assembler.is_closed() {
            let mut wrote = false;
            while let Some(frame) = upload
                .poll_send()
                .map_err(|e| TransportError::Mux(MuxError::Frame(e)))?
            {
                stream.write_all(&frame).await?;
                wrote = true;
            }
            if wrote {
                stream.flush().await?;
            }
        }
        if assembler.is_closed() {
            break;
        }

        // Read inbound. WINDOW grants unblock more sending; PONGs keep the mux
        // alive; DATA/CLOSE/RESET drive the response assembler.
        let n = tokio::time::timeout(READ_TIMEOUT, stream.read(&mut buf))
            .await
            .map_err(|_| {
                TransportError::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "PL read timed out awaiting response or window grant",
                ))
            })??;
        if n == 0 {
            break; // peer closed the connection
        }
        let out = assembler.feed(&buf[..n])?;
        for credit in out.window_grants {
            upload.grant(credit);
        }
        if !out.pongs.is_empty() {
            for pong in out.pongs {
                stream.write_all(&pong).await?;
            }
            stream.flush().await?;
        }
    }
    // Best-effort clean close.
    let _ = stream.shutdown().await;

    Ok(assembler.into_response()?)
}
