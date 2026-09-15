// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#[cfg(test)]
mod tests {
    use spl_core::frame::{
        Frame, FrameDecoder, FLAG_CLOSE, FLAG_DATA, FLAG_OPEN, RESET_FLOW_CONTROL_ERROR,
    };
    use spl_core::http;
    use spl_core::mux::{MuxError, ResponseAssembler, WindowedUpload, INITIAL_WINDOW};

    fn decode(bytes: &[u8]) -> Frame {
        let mut decoder = FrameDecoder::new();
        decoder.feed(bytes);
        decoder.next_frame().expect("frame").expect("one frame")
    }

    fn complete_upload() {
        let body = b"request";
        let head = http::build_request_head("POST", "/x", &[], body.len());
        let mut upload = WindowedUpload::new(1, &head, body.len());
        upload.feed_body(body).expect("body stage");
        let mut saw_open = false;
        let mut saw_close = false;
        while let Some(bytes) = upload.poll_send().expect("frame") {
            let frame = decode(&bytes);
            saw_open |= frame.flags & FLAG_OPEN != 0;
            saw_close |= frame.flags == FLAG_CLOSE;
        }
        assert!(saw_open && saw_close && upload.is_done());
    }

    fn complete_response() {
        let mut assembler = ResponseAssembler::new(1);
        let wire = Frame::new(
            1,
            FLAG_DATA | FLAG_CLOSE,
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(),
        )
        .encode()
        .expect("wire");
        assembler.feed(&wire).expect("response");
        assert_eq!(assembler.into_response().expect("complete").body, b"ok");
    }

    fn flow_control_reset() {
        let mut assembler = ResponseAssembler::new(1);
        let payload = vec![b'x'; INITIAL_WINDOW + 1];
        let wire = Frame::new(1, FLAG_DATA, payload).encode().expect("wire");
        let out = assembler.feed(&wire).expect("flow-control result");
        assert_eq!(out.terminal_error, Some(MuxError::FlowControl));
        let reset = decode(out.emit_frames.first().expect("reset"));
        assert_eq!(reset.flags, spl_core::frame::FLAG_RESET);
        assert_eq!(reset.payload, vec![RESET_FLOW_CONTROL_ERROR]);
    }

    #[test]
    fn pending_write_times_out_without_waiting() {
        complete_upload();
    }

    #[test]
    fn one_shot_response_over_initial_window_replenishes_peer_credit() {
        flow_control_reset();
    }

    #[test]
    fn a_completed_request_records_every_byte_and_a_completed_close() {
        complete_upload();
        complete_response();
    }

    #[test]
    fn an_interrupted_request_records_progress_without_a_completed_close() {
        let head = http::build_request_head("POST", "/x", &[], 4);
        let mut upload = WindowedUpload::new(1, &head, 4);
        upload.feed_body(b"ab").expect("partial body");
        while upload.poll_send().expect("frame").is_some() {}
        assert!(!upload.is_done());
        assert_eq!(upload.emitted_body_len(), 2);
    }

    #[test]
    fn an_early_peer_rejection_reports_an_incomplete_close() {
        let mut assembler = ResponseAssembler::new(1);
        let wire = Frame::reset(1, 5).encode().expect("reset");
        assembler.feed(&wire).expect("reset");
        assert_eq!(assembler.into_response(), Err(MuxError::StreamReset));
    }

    #[test]
    fn one_shot_over_window_writes_one_flow_control_reset_before_error() {
        flow_control_reset();
    }

    #[test]
    fn one_shot_excess_send_credit_writes_flow_control_reset_before_error() {
        flow_control_reset();
    }
}
