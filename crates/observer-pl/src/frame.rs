// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! spl multiplex framing.
//!
//! 8-byte header + payload, byte-identical to the journal
//! (`convey/secure_listener/framing.py`) and the Android `MuxFrame`:
//!
//! ```text
//! +------+------+-----------+
//! | sid4 | flg1 | len3      |  header (big-endian)
//! +------+------+-----------+
//! | payload (len bytes)     |
//! +-------------------------+
//! ```
//!
//! The flag bitfield names exactly one of OPEN/DATA/CLOSE/RESET/WINDOW/PING/PONG
//! per frame, except the two legal combos `OPEN|DATA` (open with initial bytes)
//! and `DATA|CLOSE` (last data + half-close). PING/PONG ride stream 0 only with
//! an 8-byte nonce. Bit 7 is reserved and must be zero.

use thiserror::Error;

pub const FLAG_OPEN: u8 = 0x01;
pub const FLAG_DATA: u8 = 0x02;
pub const FLAG_CLOSE: u8 = 0x04;
pub const FLAG_RESET: u8 = 0x08;
pub const FLAG_WINDOW: u8 = 0x10;
pub const FLAG_PING: u8 = 0x20;
pub const FLAG_PONG: u8 = 0x40;
pub const FLAG_RESERVED_MASK: u8 = 0x80;

pub const HEADER_LEN: usize = 8;
/// Max payload that fits the 3-byte length field (16 MiB - 1).
pub const MAX_PAYLOAD: usize = (1 << 24) - 1;
/// Recommended per-DATA-frame chunk (64 KiB), matching the spl spec.
pub const RECOMMENDED_CHUNK: usize = 64 * 1024;
/// PING/PONG control-nonce length.
pub const CONTROL_NONCE_LEN: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub stream_id: u32,
    pub flags: u8,
    pub payload: Vec<u8>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FrameError {
    #[error("frame payload exceeds 16 MiB - 1: {0}")]
    PayloadTooLarge(usize),
    #[error("reserved flag bit set: {0:#x}")]
    ReservedFlag(u8),
}

impl Frame {
    pub fn new(stream_id: u32, flags: u8, payload: Vec<u8>) -> Self {
        Self {
            stream_id,
            flags,
            payload,
        }
    }

    /// Encode this frame to the wire (header + payload).
    pub fn encode(&self) -> Result<Vec<u8>, FrameError> {
        if self.payload.len() > MAX_PAYLOAD {
            return Err(FrameError::PayloadTooLarge(self.payload.len()));
        }
        if self.flags & FLAG_RESERVED_MASK != 0 {
            return Err(FrameError::ReservedFlag(self.flags));
        }
        let len = self.payload.len() as u32;
        let mut out = Vec::with_capacity(HEADER_LEN + self.payload.len());
        out.extend_from_slice(&self.stream_id.to_be_bytes());
        out.push(self.flags);
        out.push((len >> 16) as u8);
        out.push((len >> 8) as u8);
        out.push(len as u8);
        out.extend_from_slice(&self.payload);
        Ok(out)
    }

    /// If this is a control PING (stream 0, 8-byte nonce), the PONG to return.
    pub fn control_pong(&self) -> Option<Frame> {
        if self.stream_id == 0
            && self.flags & FLAG_PING != 0
            && self.payload.len() == CONTROL_NONCE_LEN
        {
            Some(Frame::new(0, FLAG_PONG, self.payload.clone()))
        } else {
            None
        }
    }
}

/// Streaming frame decoder. Feed bytes off the wire; pull complete frames.
/// Re-frames across transport read boundaries, exactly like the journal decoder.
#[derive(Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Pull the next complete frame, if one is buffered.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, FrameError> {
        if self.buf.len() < HEADER_LEN {
            return Ok(None);
        }
        let stream_id = u32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]);
        let flags = self.buf[4];
        if flags & FLAG_RESERVED_MASK != 0 {
            return Err(FrameError::ReservedFlag(flags));
        }
        let len =
            ((self.buf[5] as usize) << 16) | ((self.buf[6] as usize) << 8) | (self.buf[7] as usize);
        let end = HEADER_LEN + len;
        if self.buf.len() < end {
            return Ok(None);
        }
        let payload = self.buf[HEADER_LEN..end].to_vec();
        self.buf.drain(..end);
        Ok(Some(Frame::new(stream_id, flags, payload)))
    }

    /// Drain all currently-complete frames.
    pub fn drain(&mut self) -> Result<Vec<Frame>, FrameError> {
        let mut out = Vec::new();
        while let Some(frame) = self.next_frame()? {
            out.push(frame);
        }
        Ok(out)
    }
}

/// Dialer stream-id allocator: odd IDs starting at 1, the client side of the
/// mux (the journal/listener uses even IDs).
#[derive(Debug)]
pub struct FrameDialer {
    next: u32,
}

impl Default for FrameDialer {
    fn default() -> Self {
        Self { next: 1 }
    }
}

impl FrameDialer {
    pub fn allocate(&mut self) -> u32 {
        let id = self.next;
        self.next = self.next.wrapping_add(2);
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_round_trip() {
        let frame = Frame::new(7, FLAG_OPEN | FLAG_DATA, b"hello".to_vec());
        let bytes = frame.encode().unwrap();
        // Header is exactly the documented layout.
        assert_eq!(&bytes[0..4], &7u32.to_be_bytes());
        assert_eq!(bytes[4], FLAG_OPEN | FLAG_DATA);
        assert_eq!(&bytes[5..8], &[0, 0, 5]);
        let mut decoder = FrameDecoder::new();
        decoder.feed(&bytes);
        assert_eq!(decoder.next_frame().unwrap(), Some(frame));
        assert_eq!(decoder.next_frame().unwrap(), None);
    }

    #[test]
    fn decoder_reframes_across_split_reads() {
        let f1 = Frame::new(1, FLAG_DATA, b"abc".to_vec());
        let f2 = Frame::new(1, FLAG_CLOSE, Vec::new());
        let mut wire = f1.encode().unwrap();
        wire.extend(f2.encode().unwrap());
        let mut decoder = FrameDecoder::new();
        // Feed one byte at a time — framing must not depend on read boundaries.
        for b in wire {
            decoder.feed(&[b]);
        }
        assert_eq!(decoder.drain().unwrap(), vec![f1, f2]);
    }

    #[test]
    fn control_ping_yields_pong_with_same_nonce() {
        let ping = Frame::new(0, FLAG_PING, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        let pong = ping.control_pong().unwrap();
        assert_eq!(pong.flags, FLAG_PONG);
        assert_eq!(pong.stream_id, 0);
        assert_eq!(pong.payload, ping.payload);
    }

    #[test]
    fn non_ping_is_not_a_pong() {
        let data = Frame::new(1, FLAG_DATA, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(data.control_pong().is_none());
    }

    #[test]
    fn dialer_allocates_odd_ids() {
        let mut dialer = FrameDialer::default();
        assert_eq!(dialer.allocate(), 1);
        assert_eq!(dialer.allocate(), 3);
        assert_eq!(dialer.allocate(), 5);
    }

    #[test]
    fn reserved_flag_is_rejected() {
        let frame = Frame::new(1, FLAG_RESERVED_MASK, Vec::new());
        assert_eq!(frame.encode().unwrap_err(), FrameError::ReservedFlag(0x80));
    }
}
