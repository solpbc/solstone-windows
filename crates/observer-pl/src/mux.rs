// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Dialer-side request/response over the mux — the pure half of the spl client.
//!
//! A single PL request is: open a dialer stream, send the HTTP request bytes as
//! `OPEN|DATA` (chunked at [`RECOMMENDED_CHUNK`](crate::frame::RECOMMENDED_CHUNK)
//! for large bodies), half-close with a `CLOSE` frame, then read the peer's
//! frames for that stream until it `CLOSE`s — answering any control `PING` on
//! stream 0 with a `PONG`. This module is the pure state machine: it turns a
//! request into frame bytes and re-assembles response frames into an
//! [`HttpResponse`]. The socket lives in `pl-transport-win`; everything here is
//! host-testable by feeding the encoded frames straight back in.
//!
//! Flow control: the journal advertises a 1 MiB initial window. This skeleton
//! sends the request body in `RECOMMENDED_CHUNK` DATA frames without negotiating
//! WINDOW replenishment, so it is correct for bodies up to the initial window
//! (the live cross-repo gate uses small segments). Full window-aware streaming
//! for multi-MiB segments is a hardening follow-up, tracked with the encoder.

use crate::frame::{
    Frame, FrameDecoder, FrameError, FLAG_CLOSE, FLAG_DATA, FLAG_OPEN, FLAG_RESET,
    RECOMMENDED_CHUNK,
};
use crate::http::{self, HttpError, HttpResponse};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MuxError {
    #[error("frame error: {0}")]
    Frame(#[from] FrameError),
    #[error("peer reset the stream")]
    StreamReset,
    #[error("response not complete (stream not closed)")]
    Incomplete,
    #[error("http parse error: {0}")]
    Http(#[from] HttpError),
}

/// Encode an HTTP request for one dialer stream: `OPEN|DATA` (+ continuation
/// `DATA` frames for large bodies) then a half-closing `CLOSE`.
pub fn request_frames(stream_id: u32, http_bytes: &[u8]) -> Result<Vec<u8>, FrameError> {
    let mut out = Vec::new();
    if http_bytes.is_empty() {
        out.extend(Frame::new(stream_id, FLAG_OPEN | FLAG_CLOSE, Vec::new()).encode()?);
        return Ok(out);
    }
    for (i, chunk) in http_bytes.chunks(RECOMMENDED_CHUNK).enumerate() {
        let flags = if i == 0 {
            FLAG_OPEN | FLAG_DATA
        } else {
            FLAG_DATA
        };
        out.extend(Frame::new(stream_id, flags, chunk.to_vec()).encode()?);
    }
    out.extend(Frame::new(stream_id, FLAG_CLOSE, Vec::new()).encode()?);
    Ok(out)
}

/// Re-assembles response frames for one dialer stream into the HTTP body.
pub struct ResponseAssembler {
    stream_id: u32,
    decoder: FrameDecoder,
    body: Vec<u8>,
    closed: bool,
    reset: bool,
}

impl ResponseAssembler {
    pub fn new(stream_id: u32) -> Self {
        Self {
            stream_id,
            decoder: FrameDecoder::new(),
            body: Vec::new(),
            closed: false,
            reset: false,
        }
    }

    /// Feed bytes read off the transport. Returns any control `PONG` frames
    /// (already encoded) that must be written back to keep the mux alive.
    pub fn feed(&mut self, data: &[u8]) -> Result<Vec<Vec<u8>>, MuxError> {
        self.decoder.feed(data);
        let mut pongs = Vec::new();
        for frame in self.decoder.drain()? {
            if let Some(pong) = frame.control_pong() {
                pongs.push(pong.encode()?);
                continue;
            }
            if frame.stream_id != self.stream_id {
                continue; // not our stream (other muxed streams / stray control)
            }
            if frame.flags & FLAG_RESET != 0 {
                self.reset = true;
                self.closed = true;
                continue;
            }
            if frame.flags & FLAG_DATA != 0 {
                self.body.extend_from_slice(&frame.payload);
            }
            if frame.flags & FLAG_CLOSE != 0 {
                self.closed = true;
            }
        }
        Ok(pongs)
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    pub fn was_reset(&self) -> bool {
        self.reset
    }

    /// Parse the assembled body into an [`HttpResponse`]. Errors if the stream
    /// was reset or has not closed yet.
    pub fn into_response(self) -> Result<HttpResponse, MuxError> {
        if self.reset {
            return Err(MuxError::StreamReset);
        }
        if !self.closed {
            return Err(MuxError::Incomplete);
        }
        Ok(http::parse_response(&self.body)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{Frame, FLAG_DATA, FLAG_OPEN, FLAG_PING, FLAG_PONG};

    #[test]
    fn request_then_response_round_trips() {
        let request = http::build_request("GET", "/healthz", &[], b"");
        let frames = request_frames(1, &request).unwrap();

        // Decode what the client would send and confirm it reconstructs the request.
        let mut dec = FrameDecoder::new();
        dec.feed(&frames);
        let sent = dec.drain().unwrap();
        assert_eq!(sent[0].flags, FLAG_OPEN | FLAG_DATA);
        assert_eq!(sent.last().unwrap().flags, FLAG_CLOSE);
        let reassembled: Vec<u8> = sent.iter().flat_map(|f| f.payload.clone()).collect();
        assert_eq!(reassembled, request);

        // Now feed a server response (DATA|CLOSE on the same stream).
        let resp_bytes = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi";
        let server_frame = Frame::new(1, FLAG_DATA | FLAG_CLOSE, resp_bytes.to_vec());
        let mut asm = ResponseAssembler::new(1);
        let pongs = asm.feed(&server_frame.encode().unwrap()).unwrap();
        assert!(pongs.is_empty());
        assert!(asm.is_closed());
        let response = asm.into_response().unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"hi");
    }

    #[test]
    fn answers_control_ping_with_pong() {
        let mut asm = ResponseAssembler::new(3);
        let ping = Frame::new(0, FLAG_PING, vec![9, 8, 7, 6, 5, 4, 3, 2]);
        let pongs = asm.feed(&ping.encode().unwrap()).unwrap();
        assert_eq!(pongs.len(), 1);
        let mut dec = FrameDecoder::new();
        dec.feed(&pongs[0]);
        let pong = dec.next_frame().unwrap().unwrap();
        assert_eq!(pong.flags, FLAG_PONG);
        assert_eq!(pong.payload, vec![9, 8, 7, 6, 5, 4, 3, 2]);
    }

    #[test]
    fn large_body_is_chunked_and_reassembles() {
        let body = vec![0xABu8; RECOMMENDED_CHUNK * 2 + 17];
        let request = http::build_request("POST", "/app/observer/ingest", &[], &body);
        let frames = request_frames(5, &request).unwrap();
        let mut dec = FrameDecoder::new();
        dec.feed(&frames);
        let sent = dec.drain().unwrap();
        // First DATA frame carries OPEN; there is more than one DATA frame.
        assert!(sent[0].flags & FLAG_OPEN != 0);
        assert!(sent.iter().filter(|f| f.flags & FLAG_DATA != 0).count() >= 3);
        let reassembled: Vec<u8> = sent.iter().flat_map(|f| f.payload.clone()).collect();
        assert_eq!(reassembled, request);
    }

    #[test]
    fn reset_frame_surfaces_as_error() {
        let mut asm = ResponseAssembler::new(1);
        asm.feed(&Frame::new(1, FLAG_RESET, vec![0x01]).encode().unwrap())
            .unwrap();
        assert!(asm.was_reset());
        assert_eq!(asm.into_response().unwrap_err(), MuxError::StreamReset);
    }

    #[test]
    fn unclosed_stream_is_incomplete() {
        let mut asm = ResponseAssembler::new(1);
        asm.feed(
            &Frame::new(1, FLAG_DATA, b"partial".to_vec())
                .encode()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(asm.into_response().unwrap_err(), MuxError::Incomplete);
    }
}
