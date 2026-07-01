// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Dialer-side request/response over the mux — the pure half of the spl client.
//!
//! A single PL request is: open a dialer stream, send the HTTP request bytes as
//! `OPEN|DATA` (chunked at [`RECOMMENDED_CHUNK`](crate::frame::RECOMMENDED_CHUNK)
//! for large bodies), half-close with a `CLOSE` frame, then read the peer's
//! frames for that stream until it `CLOSE`s — answering any control `PING` on
//! stream 0 with a `PONG`. This module is the pure state machine: it turns a
//! request into frame bytes ([`WindowedUpload`]) and re-assembles response
//! frames into an [`HttpResponse`] ([`ResponseAssembler`]). The socket lives in
//! `pl-transport-win`; everything here is host-testable by feeding the encoded
//! frames straight back in.
//!
//! Flow control: the journal advertises a [`INITIAL_WINDOW`] (1 MiB) send window
//! per stream and replenishes it with `WINDOW` frames as it consumes the body
//! (`convey/secure_listener/mux.py` grants at 50% consumed). [`WindowedUpload`]
//! tracks send credit, never sends more unacknowledged DATA payload than the
//! window allows, and resumes once the transport feeds back inbound grants — the
//! same credit loop iOS's `MuxStream` ships. This makes multi-MiB segments (an
//! encoded screen segment is ~37.5 MB) uploadable; before it, the client was
//! only correct for bodies that fit the 1 MiB initial window.

use crate::frame::{
    Frame, FrameDecoder, FrameError, FLAG_CLOSE, FLAG_DATA, FLAG_OPEN, FLAG_RESET, MAX_PAYLOAD,
    RECOMMENDED_CHUNK,
};
use crate::http::{self, ChunkedDecoder, HttpError, HttpResponse};
use thiserror::Error;

/// Initial per-stream send window the journal advertises, byte-identical to
/// `convey/secure_listener/framing.py::INITIAL_WINDOW` and the iOS
/// `MuxConstants.initialCredit`.
pub const INITIAL_WINDOW: usize = 1 << 20;

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

/// Send-side flow control for one dialer stream.
///
/// Emits the HTTP request as `OPEN|DATA…` frames followed by a half-closing
/// `CLOSE`, never letting the in-flight (un-granted) DATA payload exceed the
/// peer's advertised window. The transport pumps [`poll_send`](Self::poll_send)
/// to drain everything the window currently permits, then reads inbound frames
/// and feeds any [`grant`](Self::grant)s back before pumping again — full-duplex,
/// exactly the credit loop the journal expects (and iOS already ships).
pub struct WindowedUpload {
    stream_id: u32,
    request: Vec<u8>,
    offset: usize,
    /// Bytes of DATA payload we may still send before waiting for a grant.
    send_credit: usize,
    opened: bool,
    closed: bool,
}

impl WindowedUpload {
    /// Begin uploading `request` (the full HTTP/1.1 bytes — head + body; the
    /// journal counts every DATA payload byte against the window) on `stream_id`.
    pub fn new(stream_id: u32, request: &[u8]) -> Self {
        Self {
            stream_id,
            request: request.to_vec(),
            offset: 0,
            send_credit: INITIAL_WINDOW,
            opened: false,
            closed: false,
        }
    }

    /// Credit an inbound `WINDOW` grant. Saturating: a malicious/huge grant can
    /// never overflow, and we never send beyond the bytes we actually have.
    pub fn grant(&mut self, credit: u32) {
        self.send_credit = self.send_credit.saturating_add(credit as usize);
    }

    /// The next frame to write, or `None` when there is nothing to send right now
    /// — either because the window is exhausted (call again after a [`grant`](Self::grant))
    /// or the upload is [`done`](Self::is_done). The transport loops this until
    /// it returns `None`, then reads.
    pub fn poll_send(&mut self) -> Result<Option<Vec<u8>>, FrameError> {
        // Empty request (e.g. a bodyless GET): a single OPEN|CLOSE.
        if self.request.is_empty() {
            if self.closed {
                return Ok(None);
            }
            self.opened = true;
            self.closed = true;
            return Ok(Some(
                Frame::new(self.stream_id, FLAG_OPEN | FLAG_CLOSE, Vec::new()).encode()?,
            ));
        }

        let remaining = self.request.len() - self.offset;
        if remaining > 0 {
            if self.send_credit == 0 {
                return Ok(None); // blocked: wait for a WINDOW grant
            }
            let n = remaining
                .min(RECOMMENDED_CHUNK)
                .min(MAX_PAYLOAD)
                .min(self.send_credit);
            let chunk = self.request[self.offset..self.offset + n].to_vec();
            let flags = if self.opened {
                FLAG_DATA
            } else {
                FLAG_OPEN | FLAG_DATA
            };
            self.opened = true;
            self.offset += n;
            self.send_credit -= n;
            return Ok(Some(Frame::new(self.stream_id, flags, chunk).encode()?));
        }

        // Body fully sent — emit the half-closing CLOSE exactly once.
        if !self.closed {
            self.closed = true;
            return Ok(Some(
                Frame::new(self.stream_id, FLAG_CLOSE, Vec::new()).encode()?,
            ));
        }
        Ok(None)
    }

    /// True once the half-closing `CLOSE` has been emitted (nothing left to send).
    pub fn is_done(&self) -> bool {
        self.closed
    }

    /// True when bytes remain but the window is exhausted — the transport must
    /// read an inbound `WINDOW` grant before [`poll_send`](Self::poll_send) will
    /// produce anything. (Distinguishes "blocked" from "done" for callers/tests.)
    pub fn is_blocked(&self) -> bool {
        !self.closed && self.offset < self.request.len() && self.send_credit == 0
    }
}

/// What a [`ResponseAssembler::feed`] pass surfaced for the transport to act on:
/// control `PONG`s to write back, and inbound `WINDOW` grants to credit the
/// matching [`WindowedUpload`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct FeedOutput {
    /// Encoded `PONG` frames that must be written back to keep the mux alive.
    pub pongs: Vec<Vec<u8>>,
    /// Credit (bytes) granted by inbound `WINDOW` frames for this stream.
    pub window_grants: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpHead {
    pub status: u16,
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamEnd {
    Close,
    Reset,
    Eof,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamItem {
    Head(HttpHead),
    Body(Vec<u8>),
    End(StreamEnd),
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct StreamingFeed {
    pub pongs: Vec<Vec<u8>>,
    pub window_grants: Vec<u32>,
    pub items: Vec<StreamItem>,
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

    /// Feed bytes read off the transport. Returns the control `PONG`s to write
    /// back and any `WINDOW` grants for this stream (so the transport can credit
    /// its in-flight upload). DATA accrues into the body; CLOSE/RESET end it.
    pub fn feed(&mut self, data: &[u8]) -> Result<FeedOutput, MuxError> {
        self.decoder.feed(data);
        let mut out = FeedOutput::default();
        for frame in self.decoder.drain()? {
            if let Some(pong) = frame.control_pong() {
                out.pongs.push(pong.encode()?);
                continue;
            }
            if frame.stream_id != self.stream_id {
                continue; // not our stream (other muxed streams / stray control)
            }
            if let Some(credit) = frame.window_credit() {
                out.window_grants.push(credit);
                continue;
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
        Ok(out)
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

/// Head-first streaming response assembler for long-lived journal responses.
pub struct StreamingResponseAssembler {
    stream_id: u32,
    decoder: FrameDecoder,
    head_buf: Vec<u8>,
    head_emitted: bool,
    chunked: bool,
    chunked_decoder: ChunkedDecoder,
    closed: bool,
    reset: bool,
}

impl StreamingResponseAssembler {
    pub fn new(stream_id: u32) -> Self {
        Self {
            stream_id,
            decoder: FrameDecoder::new(),
            head_buf: Vec::new(),
            head_emitted: false,
            chunked: false,
            chunked_decoder: ChunkedDecoder::new(),
            closed: false,
            reset: false,
        }
    }

    pub fn feed(&mut self, data: &[u8]) -> Result<StreamingFeed, MuxError> {
        self.decoder.feed(data);
        let mut out = StreamingFeed::default();
        for frame in self.decoder.drain()? {
            if let Some(pong) = frame.control_pong() {
                out.pongs.push(pong.encode()?);
                continue;
            }
            if frame.stream_id != self.stream_id {
                continue;
            }
            if let Some(credit) = frame.window_credit() {
                out.window_grants.push(credit);
                continue;
            }
            if frame.flags & FLAG_RESET != 0 {
                self.reset = true;
                self.closed = true;
                out.items.push(StreamItem::End(StreamEnd::Reset));
                continue;
            }
            if frame.flags & FLAG_DATA != 0 {
                self.feed_data(&frame.payload, &mut out.items)?;
            }
            if frame.flags & FLAG_CLOSE != 0 {
                self.closed = true;
                out.items.push(StreamItem::End(StreamEnd::Close));
            }
        }
        Ok(out)
    }

    pub fn head_emitted(&self) -> bool {
        self.head_emitted
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    pub fn finish_eof(&mut self) -> StreamEnd {
        self.closed = true;
        StreamEnd::Eof
    }

    fn feed_data(&mut self, payload: &[u8], items: &mut Vec<StreamItem>) -> Result<(), MuxError> {
        if !self.head_emitted {
            self.head_buf.extend_from_slice(payload);
            let Some(split) = http::find_subsequence(&self.head_buf, b"\r\n\r\n") else {
                return Ok(());
            };
            let (status, headers) = http::parse_head(&self.head_buf[..split])?;
            self.chunked = headers
                .iter()
                .find(|(k, _)| k == "transfer-encoding")
                .map(|(_, v)| v.eq_ignore_ascii_case("chunked"))
                .unwrap_or(false);
            self.head_emitted = true;
            items.push(StreamItem::Head(HttpHead { status, headers }));

            let body_start = split + 4;
            let body = self.head_buf[body_start..].to_vec();
            self.head_buf.clear();
            if !body.is_empty() {
                self.feed_body(&body, items)?;
            }
            return Ok(());
        }

        self.feed_body(payload, items)
    }

    fn feed_body(&mut self, payload: &[u8], items: &mut Vec<StreamItem>) -> Result<(), MuxError> {
        if payload.is_empty() {
            return Ok(());
        }
        let body = if self.chunked {
            self.chunked_decoder.push(payload)?
        } else {
            payload.to_vec()
        };
        if !body.is_empty() {
            items.push(StreamItem::Body(body));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{
        Frame, FLAG_CLOSE, FLAG_DATA, FLAG_OPEN, FLAG_PING, FLAG_PONG, FLAG_RESET, FLAG_WINDOW,
    };

    /// Drain everything a [`WindowedUpload`] will emit under its current credit,
    /// returning the decoded frames.
    fn drain_permitted(up: &mut WindowedUpload) -> Vec<Frame> {
        let mut dec = FrameDecoder::new();
        while let Some(bytes) = up.poll_send().unwrap() {
            dec.feed(&bytes);
        }
        dec.drain().unwrap()
    }

    #[test]
    fn small_request_opens_data_then_closes_in_one_pass() {
        let request = http::build_request("GET", "/healthz", &[], b"");
        let mut up = WindowedUpload::new(1, &request);
        let frames = drain_permitted(&mut up);
        assert!(up.is_done());
        assert_eq!(frames[0].flags, FLAG_OPEN | FLAG_DATA);
        assert_eq!(frames.last().unwrap().flags, FLAG_CLOSE);
        let reassembled: Vec<u8> = frames.iter().flat_map(|f| f.payload.clone()).collect();
        assert_eq!(reassembled, request);
    }

    #[test]
    fn empty_request_is_a_single_open_close() {
        let mut up = WindowedUpload::new(7, b"");
        let frames = drain_permitted(&mut up);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].flags, FLAG_OPEN | FLAG_CLOSE);
        assert!(up.is_done());
    }

    #[test]
    fn body_within_initial_window_sends_without_blocking() {
        // 2 chunks + change, all well under the 1 MiB initial window.
        let body = vec![0xABu8; RECOMMENDED_CHUNK * 2 + 17];
        let request = http::build_request("POST", "/app/observer/ingest", &[], &body);
        let mut up = WindowedUpload::new(5, &request);
        let frames = drain_permitted(&mut up);
        assert!(up.is_done(), "small body completes in one credit pass");
        assert!(frames[0].flags & FLAG_OPEN != 0);
        assert!(frames.iter().filter(|f| f.flags & FLAG_DATA != 0).count() >= 3);
        let reassembled: Vec<u8> = frames.iter().flat_map(|f| f.payload.clone()).collect();
        assert_eq!(reassembled, request);
    }

    #[test]
    fn body_over_window_blocks_until_granted_then_completes() {
        // 2.5 MiB body — far past the 1 MiB initial window, so the upload must
        // pause and resume on WINDOW grants (the >1 MiB path encoded segments hit).
        let body = vec![0x5Au8; INITIAL_WINDOW * 2 + INITIAL_WINDOW / 2];
        let request = http::build_request("POST", "/app/observer/ingest", &[], &body);
        let mut up = WindowedUpload::new(3, &request);

        let mut all = FrameDecoder::new();
        // First pass drains exactly the initial window, then blocks (body remains).
        while let Some(bytes) = up.poll_send().unwrap() {
            all.feed(&bytes);
        }
        assert!(
            up.is_blocked(),
            "exhausting the window must block, not finish"
        );
        assert!(!up.is_done());

        // Grant credit in 512 KiB slices (the journal's replenishment grain)
        // until the whole body — plus the half-closing CLOSE — is out.
        let mut guard = 0;
        while !up.is_done() {
            up.grant((INITIAL_WINDOW / 2) as u32);
            while let Some(bytes) = up.poll_send().unwrap() {
                all.feed(&bytes);
            }
            guard += 1;
            assert!(guard < 100, "should converge well before this");
        }

        let frames = all.drain().unwrap();
        assert_eq!(frames.last().unwrap().flags, FLAG_CLOSE);
        // Every byte of the request made it out, in order, exactly once.
        let reassembled: Vec<u8> = frames.iter().flat_map(|f| f.payload.clone()).collect();
        assert_eq!(reassembled, request);
        // No single DATA frame exceeded the recommended chunk.
        assert!(frames
            .iter()
            .filter(|f| f.flags & FLAG_DATA != 0)
            .all(|f| f.payload.len() <= RECOMMENDED_CHUNK));
    }

    #[test]
    fn response_data_close_round_trips() {
        let resp_bytes = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi";
        let server_frame = Frame::new(1, FLAG_DATA | FLAG_CLOSE, resp_bytes.to_vec());
        let mut asm = ResponseAssembler::new(1);
        let out = asm.feed(&server_frame.encode().unwrap()).unwrap();
        assert!(out.pongs.is_empty());
        assert!(out.window_grants.is_empty());
        assert!(asm.is_closed());
        let response = asm.into_response().unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"hi");
    }

    #[test]
    fn streaming_emits_head_then_body_from_one_data_frame() {
        let resp_bytes = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\ndata: a\n\n";
        let server_frame = Frame::new(1, FLAG_DATA, resp_bytes.to_vec());
        let mut asm = StreamingResponseAssembler::new(1);

        let out = asm.feed(&server_frame.encode().unwrap()).unwrap();

        assert_eq!(
            out.items,
            vec![
                StreamItem::Head(HttpHead {
                    status: 200,
                    headers: vec![("content-type".to_string(), "text/event-stream".to_string())],
                }),
                StreamItem::Body(b"data: a\n\n".to_vec()),
            ]
        );
        assert!(asm.head_emitted());
        assert!(!asm.is_closed());
    }

    #[test]
    fn streaming_waits_for_split_head_terminator() {
        let mut asm = StreamingResponseAssembler::new(1);
        let first = Frame::new(1, FLAG_DATA, b"HTTP/1.1 200 OK\r\nContent-Type".to_vec());
        let second = Frame::new(
            1,
            FLAG_DATA,
            b": text/event-stream\r\n\r\ndata: b\n\n".to_vec(),
        );
        let third = Frame::new(1, FLAG_DATA, b"data: c\n\n".to_vec());

        let out = asm.feed(&first.encode().unwrap()).unwrap();
        assert!(out.items.is_empty());
        assert!(!asm.head_emitted());

        let out = asm.feed(&second.encode().unwrap()).unwrap();
        assert_eq!(out.items.len(), 2);
        assert!(matches!(out.items[0], StreamItem::Head(_)));
        assert_eq!(out.items[1], StreamItem::Body(b"data: b\n\n".to_vec()));
        assert!(asm.head_emitted());

        let out = asm.feed(&third.encode().unwrap()).unwrap();
        assert_eq!(out.items, vec![StreamItem::Body(b"data: c\n\n".to_vec())]);
    }

    #[test]
    fn streaming_dechunks_incrementally() {
        let mut asm = StreamingResponseAssembler::new(1);
        let head = Frame::new(
            1,
            FLAG_DATA,
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec(),
        );
        let first_chunk = Frame::new(1, FLAG_DATA, b"4\r\nWiki\r\n".to_vec());
        let split_size = Frame::new(1, FLAG_DATA, b"5".to_vec());
        let rest = Frame::new(1, FLAG_DATA, b"\r\npedia\r\n0\r\n\r\n".to_vec());

        let out = asm.feed(&head.encode().unwrap()).unwrap();
        assert_eq!(out.items.len(), 1);
        assert!(matches!(out.items[0], StreamItem::Head(_)));

        let out = asm.feed(&first_chunk.encode().unwrap()).unwrap();
        assert_eq!(out.items, vec![StreamItem::Body(b"Wiki".to_vec())]);

        let out = asm.feed(&split_size.encode().unwrap()).unwrap();
        assert!(out.items.is_empty());

        let out = asm.feed(&rest.encode().unwrap()).unwrap();
        assert_eq!(out.items, vec![StreamItem::Body(b"pedia".to_vec())]);
    }

    #[test]
    fn streaming_close_after_body_emits_end() {
        let resp_bytes = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\ndata: a\n\n";
        let server_frame = Frame::new(1, FLAG_DATA | FLAG_CLOSE, resp_bytes.to_vec());
        let mut asm = StreamingResponseAssembler::new(1);

        let out = asm.feed(&server_frame.encode().unwrap()).unwrap();

        assert_eq!(out.items.last(), Some(&StreamItem::End(StreamEnd::Close)));
        assert!(asm.is_closed());
    }

    #[test]
    fn streaming_reset_mid_stream_emits_end_and_closes() {
        let mut asm = StreamingResponseAssembler::new(1);
        asm.feed(
            &Frame::new(
                1,
                FLAG_DATA,
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n".to_vec(),
            )
            .encode()
            .unwrap(),
        )
        .unwrap();

        let out = asm
            .feed(&Frame::new(1, FLAG_RESET, Vec::new()).encode().unwrap())
            .unwrap();

        assert_eq!(out.items, vec![StreamItem::End(StreamEnd::Reset)]);
        assert!(asm.is_closed());
    }

    #[test]
    fn streaming_finish_eof_marks_closed() {
        let mut asm = StreamingResponseAssembler::new(1);
        assert_eq!(asm.finish_eof(), StreamEnd::Eof);
        assert!(asm.is_closed());
    }

    #[test]
    fn answers_control_ping_with_pong() {
        let mut asm = ResponseAssembler::new(3);
        let ping = Frame::new(0, FLAG_PING, vec![9, 8, 7, 6, 5, 4, 3, 2]);
        let out = asm.feed(&ping.encode().unwrap()).unwrap();
        assert_eq!(out.pongs.len(), 1);
        let mut dec = FrameDecoder::new();
        dec.feed(&out.pongs[0]);
        let pong = dec.next_frame().unwrap().unwrap();
        assert_eq!(pong.flags, FLAG_PONG);
        assert_eq!(pong.payload, vec![9, 8, 7, 6, 5, 4, 3, 2]);
    }

    #[test]
    fn streaming_surfaces_control_pong_and_window_grant() {
        let mut asm = StreamingResponseAssembler::new(3);
        let ping = Frame::new(0, FLAG_PING, vec![9, 8, 7, 6, 5, 4, 3, 2]);
        let ours = Frame::new(3, FLAG_WINDOW, vec![0x00, 0x08, 0x00, 0x00]);
        let mut wire = ping.encode().unwrap();
        wire.extend(ours.encode().unwrap());

        let out = asm.feed(&wire).unwrap();

        assert_eq!(out.pongs.len(), 1);
        let mut dec = FrameDecoder::new();
        dec.feed(&out.pongs[0]);
        let pong = dec.next_frame().unwrap().unwrap();
        assert_eq!(pong.flags, FLAG_PONG);
        assert_eq!(out.window_grants, vec![512 * 1024]);
        assert!(out.items.is_empty());
        assert!(!asm.is_closed());
    }

    #[test]
    fn surfaces_window_grant_for_our_stream_only() {
        let mut asm = ResponseAssembler::new(3);
        let ours = Frame::new(3, FLAG_WINDOW, vec![0x00, 0x08, 0x00, 0x00]); // 512 KiB
        let other = Frame::new(9, FLAG_WINDOW, vec![0x00, 0x10, 0x00, 0x00]); // not our stream
        let mut wire = ours.encode().unwrap();
        wire.extend(other.encode().unwrap());
        let out = asm.feed(&wire).unwrap();
        assert_eq!(out.window_grants, vec![512 * 1024]);
        assert!(!asm.is_closed(), "a WINDOW frame must not close the stream");
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
