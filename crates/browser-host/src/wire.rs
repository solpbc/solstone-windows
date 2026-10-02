// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Length-prefixed frames over async byte streams.
//!
//! Both legs (browser stdio and the local pipe) use the contract's framing: a
//! native-endian `u32` byte length, then UTF-8 JSON. Reading goes through the
//! shared [`Assembler`], which enforces the per-direction cap before any body
//! allocation; a frame that has started must finish within the contract's
//! partial-frame lifetime.

use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use native_browser_frame::constants::PARTIAL_FRAME_MS_LIFETIME;
use native_browser_frame::{Assembler, Chunk, Direction, FrameError, OutFrame, Step};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Debug)]
pub enum WireError {
    Io(io::Error),
    Frame(FrameError),
    /// A frame started and did not finish within the partial-frame lifetime.
    PartialFrameExpired,
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WireError::Io(e) => write!(f, "io: {e}"),
            WireError::Frame(e) => write!(f, "frame: {e}"),
            WireError::PartialFrameExpired => write!(f, "partial_frame_expired"),
        }
    }
}

impl std::error::Error for WireError {}

impl From<io::Error> for WireError {
    fn from(e: io::Error) -> Self {
        WireError::Io(e)
    }
}

impl From<FrameError> for WireError {
    fn from(e: FrameError) -> Self {
        WireError::Frame(e)
    }
}

const READ_CHUNK: usize = 64 * 1024;

/// Reads whole frames from one direction of a stream.
pub struct FrameReader<R> {
    inner: R,
    assembler: Assembler,
    ready: VecDeque<Vec<u8>>,
    eof: bool,
    buf: Vec<u8>,
    partial_lifetime: Duration,
    partial_since: Option<tokio::time::Instant>,
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
    pub fn new(inner: R, direction: Direction) -> Self {
        Self {
            inner,
            assembler: Assembler::new(direction),
            ready: VecDeque::new(),
            eof: false,
            buf: vec![0; READ_CHUNK],
            partial_lifetime: Duration::from_millis(PARTIAL_FRAME_MS_LIFETIME),
            partial_since: None,
        }
    }

    /// The next whole frame, or `None` on a clean end of stream. Waiting for the
    /// first byte of a frame is unbounded; once a frame has started, the rest
    /// must arrive within the partial-frame lifetime. Cancel-safe: a dropped
    /// call loses no bytes, and the lifetime keeps counting from the frame's
    /// first byte across calls.
    pub async fn next(&mut self) -> Result<Option<Vec<u8>>, WireError> {
        loop {
            if let Some(frame) = self.ready.pop_front() {
                return Ok(Some(frame));
            }
            if self.eof {
                return Ok(None);
            }
            let n = if self.assembler.retained() > 0 {
                let since = *self
                    .partial_since
                    .get_or_insert_with(tokio::time::Instant::now);
                match tokio::time::timeout_at(
                    since + self.partial_lifetime,
                    self.inner.read(&mut self.buf),
                )
                .await
                {
                    Ok(read) => read?,
                    Err(_) => return Err(WireError::PartialFrameExpired),
                }
            } else {
                self.inner.read(&mut self.buf).await?
            };
            let chunk = if n == 0 {
                Chunk::Eof
            } else {
                Chunk::Data(&self.buf[..n])
            };
            for step in self.assembler.feed(chunk)? {
                match step {
                    Step::Message(frame) => {
                        self.partial_since = None;
                        self.ready.push_back(frame)
                    }
                    Step::CleanEof => self.eof = true,
                    Step::EmptyRead | Step::NeedMore => {}
                }
            }
        }
    }
}

/// Write one frame (prefix + payload) and flush. The direction's cap is
/// enforced before anything is written.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    direction: Direction,
    payload: &[u8],
) -> Result<(), WireError> {
    let frame = OutFrame::from_payload(direction, payload)?;
    w.write_all(frame.pending()).await?;
    w.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trips_frames_split_across_reads() {
        let (mut a, b) = tokio::io::duplex(7);
        let writer = tokio::spawn(async move {
            write_frame(
                &mut a,
                Direction::HostToExtension,
                br#"{"type":"bye","reason":"update"}"#,
            )
            .await
            .unwrap();
            write_frame(
                &mut a,
                Direction::HostToExtension,
                "{\"x\":\"Café 漢字\"}".as_bytes(),
            )
            .await
            .unwrap();
        });
        let mut reader = FrameReader::new(b, Direction::HostToExtension);
        assert_eq!(
            reader.next().await.unwrap().unwrap(),
            br#"{"type":"bye","reason":"update"}"#.to_vec()
        );
        assert_eq!(
            reader.next().await.unwrap().unwrap(),
            "{\"x\":\"Café 漢字\"}".as_bytes().to_vec()
        );
        writer.await.unwrap();
        assert!(reader.next().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn over_cap_prefix_is_refused_before_the_body() {
        let (mut a, b) = tokio::io::duplex(64);
        let len = (native_browser_frame::HOST_TO_EXTENSION_MAX as u32 + 1).to_ne_bytes();
        a.write_all(&len).await.unwrap();
        let mut reader = FrameReader::new(b, Direction::HostToExtension);
        assert!(matches!(
            reader.next().await,
            Err(WireError::Frame(FrameError::OverCap))
        ));
    }

    #[tokio::test]
    async fn truncated_body_at_eof_is_an_error() {
        let (mut a, b) = tokio::io::duplex(64);
        a.write_all(&10u32.to_ne_bytes()).await.unwrap();
        a.write_all(b"abc").await.unwrap();
        drop(a);
        let mut reader = FrameReader::new(b, Direction::ExtensionToHost);
        assert!(matches!(
            reader.next().await,
            Err(WireError::Frame(FrameError::TruncatedBody))
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn a_stalled_partial_frame_expires() {
        let (mut a, b) = tokio::io::duplex(64);
        a.write_all(&10u32.to_ne_bytes()).await.unwrap();
        a.write_all(b"abc").await.unwrap();
        let mut reader = FrameReader::new(b, Direction::ExtensionToHost);
        let result = reader.next().await;
        assert!(matches!(result, Err(WireError::PartialFrameExpired)));
        drop(a);
    }
}
