// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! File requests reuse the shared bridge's credit-bounded streaming engine.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use spl_core::bridge::BridgeNames;
use spl_core::http::{self, HttpResponse};
use spl_transport::journal_bridge::{self, BridgePolicy, CarrierOpener, JournalBridgeConfig};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::ordinary_request::OrdinaryRequest;
use crate::{ObserverClient, RouteError, TransportError};

pub const REQUEST_BODY_LIMIT: u64 = 4 * 1024 * 1024 * 1024 + 1024 * 1024;
const RESPONSE_CAP: usize = 64 * 1024;
const HEAD_CAP: usize = 16 * 1024;
// A moving upload has no total deadline. A stalled local exchange is bounded.
const IDLE: Duration = Duration::from_secs(300);

struct Opener(Arc<ObserverClient>);

impl CarrierOpener for Opener {
    fn proxy_headers(
        &self,
        headers: &[(String, String)],
    ) -> Result<Vec<(String, String)>, spl_transport::TransportError> {
        Ok(self.0.proxy_headers(headers))
    }

    fn dial_carrier(
        &self,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<spl_transport::DialedCarrier, spl_transport::TransportError>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            if !self.0.gate_open() || self.0.refusal_stop().is_some() {
                return Err(spl_transport::TransportError::NoEndpoint);
            }
            self.0
                .transport_client()
                .open_carrier(self.0.operation_observer())
                .await
                .map_err(|error| match error {
                    spl_transport::CarrierOpenError::Transport(error) => error,
                    _ => spl_transport::TransportError::NoEndpoint,
                })
        })
    }
}

struct BridgeGuard(Option<journal_bridge::JournalBridgeHandle>);
impl Drop for BridgeGuard {
    fn drop(&mut self) {
        if let Some(bridge) = self.0.take() {
            bridge.begin_shutdown();
        }
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

async fn progress<T>(work: impl Future<Output = io::Result<T>>) -> io::Result<T> {
    tokio::time::timeout(IDLE, work)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "file request stalled"))?
}

/// The reader never advances more than one fixed-size chunk ahead of writes.
async fn upload<W: AsyncWrite + Unpin, R: AsyncRead + Unpin>(
    mut write: W,
    mut body: R,
    length: u64,
    activity: tokio::sync::watch::Sender<()>,
) -> io::Result<()> {
    let mut remaining = length;
    let mut chunk = [0; 64 * 1024];
    while remaining != 0 {
        let want = usize::try_from(remaining.min(chunk.len() as u64)).expect("bounded chunk");
        let n = progress(body.read(&mut chunk[..want])).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "short file request body",
            ));
        }
        progress(write.write_all(&chunk[..n])).await?;
        remaining -= n as u64;
        activity.send_replace(());
    }
    progress(write.flush()).await
}

async fn response<R: AsyncRead + Unpin>(
    mut read: R,
    mut activity: tokio::sync::watch::Receiver<()>,
) -> Result<HttpResponse, TransportError> {
    let mut bytes = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        let n = tokio::select! {
            result = read.read(&mut chunk) => result?,
            _ = tokio::time::sleep(IDLE) => return Err(io::Error::new(io::ErrorKind::TimedOut, "file request stalled").into()),
            _ = activity.changed(), if activity.has_changed().is_ok() => continue,
        };
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..n]);
        let split = bytes.windows(4).position(|s| s == b"\r\n\r\n");
        if let Some(split) = split {
            if split > HEAD_CAP || bytes.len() - split - 4 > RESPONSE_CAP {
                return Err(invalid("file response exceeds limit").into());
            }
            let (_, headers) = http::parse_head(&bytes[..split])?;
            if headers.iter().any(|(name, value)| {
                name == "content-length" && value.parse::<usize>().is_ok_and(|n| n > RESPONSE_CAP)
            }) {
                return Err(invalid("file response exceeds limit").into());
            }
        } else if bytes.len() > HEAD_CAP {
            return Err(invalid("file response head exceeds limit").into());
        }
    }
    let parsed = http::parse_response(&bytes)?;
    if parsed.body.len() > RESPONSE_CAP {
        return Err(invalid("file response exceeds limit").into());
    }
    Ok(parsed)
}

pub(crate) async fn post<R: AsyncRead + Unpin>(
    client: Arc<ObserverClient>,
    route: OrdinaryRequest,
    content_type: &str,
    body: R,
    length: u64,
) -> Result<HttpResponse, RouteError> {
    if !client.gate_open() {
        return Err(RouteError::AwaitingConfirmation);
    }
    if let Some(stop) = client.refusal_stop() {
        return Err(RouteError::Transport(crate::pairing::map_shared_error(
            stop.error(),
        )));
    }
    if length > REQUEST_BODY_LIMIT {
        return Err(RouteError::Transport(
            invalid("file request exceeds journal limit").into(),
        ));
    }
    let spec = route.spec();
    let allowed_path = spec.path_pattern;
    let bridge = journal_bridge::start(JournalBridgeConfig {
        opener: Arc::new(Opener(client.clone())),
        bridge_names: BridgeNames {
            capability_cookie_name: observer_pl::CAP_COOKIE_NAME.into(),
            upstream_cookie_prefix: observer_pl::UPSTREAM_COOKIE_PREFIX.into(),
            observer_header_name: observer_pl::OBSERVER_HANDLE_HEADER.to_ascii_lowercase(),
            protocol_version_header_name: observer_pl::PROTOCOL_VERSION_HEADER.to_ascii_lowercase(),
        },
        endpoint_hosts: Vec::new(),
        policy: BridgePolicy {
            max_request_body_bytes: REQUEST_BODY_LIMIT as usize,
            stream_response: Arc::new(|_| true),
            local_response: Arc::new(move |head, _| {
                if head.method == spec.method && head.target == allowed_path {
                    None
                } else {
                    Some(journal_bridge::LocalResponse {
                        status: 403,
                        content_type: "text/plain".into(),
                        body: Vec::new(),
                    })
                }
            }),
            ..Default::default()
        },
    })
    .await
    .map_err(|_| RouteError::Transport(TransportError::NoEndpoint))?;
    let port = bridge.port();
    let cookie = bridge.capability_cookie().expect("capability enabled");
    let cookie = cookie.split(';').next().expect("capability cookie");
    let headers = vec![
        ("Content-Type".into(), content_type.into()),
        ("Cookie".into(), cookie.into()),
    ];
    let head = http::build_request_head_with_host(
        spec.method,
        allowed_path,
        &format!("127.0.0.1:{port}"),
        &headers,
        length as usize,
    );
    let mut guard = BridgeGuard(Some(bridge));
    let exchange = async {
        let mut stream = progress(tokio::net::TcpStream::connect(("127.0.0.1", port))).await?;
        progress(stream.write_all(&head)).await?;
        let (read, write) = stream.into_split();
        let (activity, progress_rx) = tokio::sync::watch::channel(());
        let send = upload(write, body, length, activity);
        let receive = response(read, progress_rx);
        tokio::pin!(send, receive);
        tokio::select! {
            biased;
            answer = &mut receive => answer,
            sent = &mut send => {
                // An early journal refusal can close the local writer first.
                // Read its bounded answer before judging a broken pipe.
                if let Err(error) = &sent {
                    if !matches!(error.kind(), io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset) {
                        return Err(io::Error::new(error.kind(), "file upload failed").into());
                    }
                }
                match receive.await {
                    Ok(answer) => Ok(answer),
                    Err(error) => { sent?; Err(error) }
                }
            }
        }
    }
    .await;
    let bridge = guard.0.take().expect("bridge alive");
    let stop = client.note_streamed_attempt(&bridge.status(), exchange.is_ok());
    bridge.shutdown_and_wait().await;
    if let Some(stop) = stop {
        return Err(RouteError::Transport(crate::pairing::map_shared_error(
            stop.error(),
        )));
    }
    exchange.map_err(RouteError::Transport)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll};
    use tokio::io::ReadBuf;

    struct PatternReader {
        left: usize,
        read: Arc<AtomicUsize>,
    }
    impl AsyncRead for PatternReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buffer: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let count = buffer.remaining().min(self.left);
            let offset = self.read.fetch_add(count, Ordering::SeqCst);
            for i in 0..count {
                buffer.put_slice(&[((offset + i) % 251) as u8]);
            }
            self.left -= count;
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn a_backpressured_upload_reads_only_one_chunk_and_preserves_every_byte() {
        let count = 3 * 1024 * 1024 + 17;
        let read = Arc::new(AtomicUsize::new(0));
        let source = PatternReader {
            left: count,
            read: read.clone(),
        };
        let (write, mut peer) = tokio::io::duplex(64);
        let (activity, _) = tokio::sync::watch::channel(());
        let task = tokio::spawn(upload(write, source, count as u64, activity));
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert!(read.load(Ordering::SeqCst) <= 64 * 1024);
        let mut bytes = Vec::new();
        peer.read_to_end(&mut bytes).await.unwrap();
        task.await.unwrap().unwrap();
        assert_eq!(bytes.len(), count);
        assert!(bytes.iter().enumerate().all(|(i, b)| *b == (i % 251) as u8));
    }

    #[tokio::test]
    async fn a_short_body_is_an_error() {
        let (write, mut peer) = tokio::io::duplex(64);
        let (activity, _) = tokio::sync::watch::channel(());
        let (sent, _) = tokio::join!(upload(write, &b"short"[..], 6, activity), async {
            let mut bytes = Vec::new();
            peer.read_to_end(&mut bytes).await.unwrap();
        });
        assert_eq!(sent.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn oversized_declared_and_close_delimited_answers_stop_at_the_cap() {
        for head in [
            "HTTP/1.1 200 OK\r\nContent-Length: 10000000\r\n\r\n",
            "HTTP/1.1 200 OK\r\n\r\n",
        ] {
            let mut bytes = head.as_bytes().to_vec();
            bytes.resize(2 * RESPONSE_CAP, b'x');
            let (_, activity) = tokio::sync::watch::channel(());
            assert!(response(bytes.as_slice(), activity).await.is_err());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn upload_progress_keeps_a_slow_response_wait_alive() {
        let (mut peer, read) = tokio::io::duplex(1024);
        let (activity, rx) = tokio::sync::watch::channel(());
        let task = tokio::spawn(response(read, rx));
        for _ in 0..5 {
            tokio::time::advance(Duration::from_secs(200)).await;
            activity.send_replace(());
            tokio::task::yield_now().await;
            assert!(!task.is_finished());
        }
        peer.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .await
            .unwrap();
        drop(peer);
        assert_eq!(task.await.unwrap().unwrap().body, b"ok");
    }
}
