// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#[cfg(test)]
mod tests {
    use spl_core::frame::*;
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
    fn flags_valid_matches_exact_spl_set() {
        let valid = [
            0x01u8, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x10, 0x20, 0x40,
        ];
        for flags in u8::MIN..=u8::MAX {
            assert_eq!(
                flags_valid(flags),
                valid.contains(&flags),
                "unexpected validity for {flags:#04x}"
            );
        }
    }

    #[test]
    fn control_helpers_require_exact_flags() {
        let combined = Frame::new(0, FLAG_PING | FLAG_PONG, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(combined.control_pong().is_none());
        assert!(combined.control_pong_nonce().is_none());
    }

    #[test]
    fn control_ping_builds_stream_zero_ping() {
        let nonce = [1, 2, 3, 4, 5, 6, 7, 8];
        let ping = Frame::control_ping(nonce);
        assert_eq!(ping.stream_id, 0);
        assert_eq!(ping.flags, FLAG_PING);
        assert_eq!(ping.payload, nonce.to_vec());
    }

    #[test]
    fn window_and_reset_builders_encode_protocol_payloads() {
        let window = Frame::window(5, 524_599);
        assert_eq!(window.stream_id, 5);
        assert_eq!(window.flags, FLAG_WINDOW);
        assert_eq!(window.payload, 524_599u32.to_be_bytes());
        assert_eq!(window.window_credit(), Some(524_599));

        let reset = Frame::reset(7, RESET_FLOW_CONTROL_ERROR);
        assert_eq!(reset.stream_id, 7);
        assert_eq!(reset.flags, FLAG_RESET);
        assert_eq!(reset.payload, vec![RESET_FLOW_CONTROL_ERROR]);
    }

    #[test]
    fn control_pong_nonce_round_trips() {
        let nonce = [9, 8, 7, 6, 5, 4, 3, 2];
        let ping = Frame::control_ping(nonce);
        let pong = ping.control_pong().unwrap();
        assert_eq!(pong.control_pong_nonce(), Some(nonce));
    }

    #[test]
    fn non_ping_is_not_a_pong() {
        let data = Frame::new(1, FLAG_DATA, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(data.control_pong().is_none());
    }

    #[test]
    fn non_pong_is_not_a_pong_nonce() {
        let data = Frame::new(1, FLAG_DATA, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(data.control_pong_nonce().is_none());
        let malformed = Frame::new(0, FLAG_PONG, vec![1, 2, 3]);
        assert!(malformed.control_pong_nonce().is_none());
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

    #[test]
    fn window_frame_parses_big_endian_credit() {
        // 0x00_08_00_00 = 512 KiB, the journal's 50%-consumed replenishment grant.
        let frame = Frame::new(5, FLAG_WINDOW, vec![0x00, 0x08, 0x00, 0x00]);
        assert_eq!(frame.window_credit(), Some(512 * 1024));
    }

    #[test]
    fn non_window_or_malformed_is_not_a_credit() {
        // Right flag, wrong length.
        assert!(Frame::new(5, FLAG_WINDOW, vec![1, 2, 3])
            .window_credit()
            .is_none());
        // Right length, wrong flag.
        assert!(Frame::new(5, FLAG_DATA, vec![0, 0, 0, 1])
            .window_credit()
            .is_none());
    }
}
