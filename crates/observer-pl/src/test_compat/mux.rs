// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#[cfg(test)]
mod tests {
    use spl_core::frame::{
        Frame, FrameDecoder, FrameError, FrameViolation, FLAG_CLOSE, FLAG_DATA, FLAG_OPEN,
        FLAG_PING, FLAG_PONG, FLAG_RESERVED_MASK, FLAG_RESET, FLAG_WINDOW, RECOMMENDED_CHUNK,
        RESET_FLOW_CONTROL_ERROR, RESET_PROTOCOL_ERROR,
    };
    use spl_core::http;
    use spl_core::mux::{
        CarrierDemux, HttpHead, HttpStreamAssembler, MuxError, RecvWindow, ResetReason,
        ResponseAssembler, StreamEnd, StreamEvent, StreamItem, UploadBodyError, WindowedUpload,
        INITIAL_WINDOW, MAX_ASSEMBLED_BYTES, UPLOAD_BODY_STAGE_CAPACITY,
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

    fn pump_source(
        up: &mut WindowedUpload,
        body: &[u8],
        body_offset: &mut usize,
        decoder: &mut FrameDecoder,
    ) {
        loop {
            let capacity = up.body_capacity();
            if capacity > 0 && *body_offset < body.len() {
                let end = (*body_offset + capacity).min(body.len());
                up.feed_body(&body[*body_offset..end]).unwrap();
                *body_offset = end;
            }
            let Some(bytes) = up.poll_send().unwrap() else {
                break;
            };
            decoder.feed(&bytes);
        }
    }

    fn encode_frames(frames: &[Frame]) -> Vec<u8> {
        let mut wire = Vec::new();
        for frame in frames {
            wire.extend(frame.encode().unwrap());
        }
        wire
    }

    fn text_event_head() -> StreamItem {
        StreamItem::Head(HttpHead {
            status: 200,
            headers: vec![("content-type".to_string(), "text/event-stream".to_string())],
        })
    }

    fn plain_head() -> StreamItem {
        StreamItem::Head(HttpHead {
            status: 200,
            headers: vec![("content-type".to_string(), "text/plain".to_string())],
        })
    }

    fn event(item: StreamItem, wire_cost: u64) -> StreamEvent {
        StreamEvent { item, wire_cost }
    }

    fn stream_event(stream_id: u32, item: StreamItem, wire_cost: u64) -> (u32, StreamEvent) {
        (stream_id, event(item, wire_cost))
    }

    fn decode_single(bytes: &[u8]) -> Frame {
        let mut decoder = FrameDecoder::new();
        decoder.feed(bytes);
        let frame = decoder.next_frame().unwrap().unwrap();
        assert!(decoder.next_frame().unwrap().is_none());
        frame
    }

    fn frame_violation(frame: &Frame) -> FrameViolation {
        FrameViolation {
            stream_id: frame.stream_id,
            flags: frame.flags,
            length: frame.payload.len(),
        }
    }

    #[test]
    fn recv_window_grants_all_consumed_bytes_at_half_window() {
        let mut window = RecvWindow::new();
        window.debit(524_461).unwrap();
        assert_eq!(window.consume(524_259), None);
        assert_eq!(window.consume(202), Some(524_461));

        window.debit(INITIAL_WINDOW).unwrap();
    }

    #[test]
    fn recv_window_rejects_over_credit_without_mutation() {
        let mut window = RecvWindow::new();
        assert_eq!(window.debit(INITIAL_WINDOW + 1), Err(MuxError::FlowControl));
        window.debit(INITIAL_WINDOW).unwrap();
        assert_eq!(
            window.consume(INITIAL_WINDOW as u64),
            Some(INITIAL_WINDOW as u32)
        );
    }

    #[test]
    fn small_request_opens_data_then_closes_in_one_pass() {
        // Protocol: `.proto-ref/framing.md`, "stream lifecycle" — OPEN starts
        // the stream and CLOSE half-closes it exactly once.
        let head = http::build_request_head("GET", "/healthz", &[], 0);
        let mut up = WindowedUpload::new(1, &head, 0);
        let frames = drain_permitted(&mut up);
        assert!(up.is_done());
        assert_eq!(frames[0].flags, FLAG_OPEN | FLAG_DATA);
        assert_eq!(frames.last().unwrap().flags, FLAG_CLOSE);
        let reassembled: Vec<u8> = frames.iter().flat_map(|f| f.payload.clone()).collect();
        assert_eq!(reassembled, head);
        assert_eq!(
            frames
                .iter()
                .filter(|frame| frame.flags == FLAG_CLOSE)
                .count(),
            1
        );
        assert!(up.poll_send().unwrap().is_none());
        assert!(up.poll_send().unwrap().is_none());
    }

    #[test]
    fn empty_request_is_a_single_open_close() {
        let head = http::build_request_head("GET", "/healthz", &[], 0);
        let mut upload = WindowedUpload::new(7, &head, 0);
        let frames = drain_permitted(&mut upload);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].flags, FLAG_OPEN | FLAG_DATA);
        assert_eq!(frames[1].flags, FLAG_CLOSE);
        assert!(upload.is_done());
    }

    #[test]
    fn body_within_initial_window_sends_without_blocking() {
        // 2 chunks + change, all well under the 1 MiB initial window.
        let body = vec![0xABu8; RECOMMENDED_CHUNK * 2 + 17];
        let head = http::build_request_head("POST", "/app/observer/ingest", &[], body.len());
        let mut up = WindowedUpload::new(5, &head, body.len());
        up.feed_body(&body).unwrap();
        let frames = drain_permitted(&mut up);
        assert!(up.is_done(), "small body completes in one credit pass");
        assert!(frames[0].flags & FLAG_OPEN != 0);
        assert!(frames.iter().filter(|f| f.flags & FLAG_DATA != 0).count() >= 3);
        let reassembled: Vec<u8> = frames.iter().flat_map(|f| f.payload.clone()).collect();
        assert_eq!(
            reassembled,
            http::build_request("POST", "/app/observer/ingest", &[], &body)
        );
        assert_eq!(up.emitted_body_len(), body.len());
    }

    #[test]
    fn body_over_window_blocks_until_granted_then_completes() {
        // Protocol: `.proto-ref/framing.md`, "flow control and backpressure"
        // and "fragmentation" — head and body share credit, zero credit blocks
        // DATA, and frames stay within the recommended chunk size.
        // 2.5 MiB body — far past the 1 MiB initial window, so the upload must
        // pause and resume on WINDOW grants.
        let body = vec![0x5Au8; INITIAL_WINDOW * 2 + INITIAL_WINDOW / 2];
        let head = http::build_request_head("POST", "/app/observer/ingest", &[], body.len());
        let mut up = WindowedUpload::new(3, &head, body.len());

        let mut all = FrameDecoder::new();
        let mut body_offset = 0;
        // First pass drains exactly the initial window, then blocks (body remains).
        pump_source(&mut up, &body, &mut body_offset, &mut all);
        assert!(
            up.is_blocked(),
            "exhausting the window must block, not finish"
        );
        assert!(!up.is_done());
        assert_eq!(up.emitted_body_len(), INITIAL_WINDOW - head.len());

        // Grant credit in 512 KiB slices (the journal's replenishment grain)
        // until the whole body — plus the half-closing CLOSE — is out.
        let mut guard = 0;
        while !up.is_done() {
            up.grant((INITIAL_WINDOW / 2) as u32).unwrap();
            pump_source(&mut up, &body, &mut body_offset, &mut all);
            guard += 1;
            assert!(guard < 100, "should converge well before this");
        }

        let frames = all.drain().unwrap();
        assert_eq!(frames.last().unwrap().flags, FLAG_CLOSE);
        // Every byte of the request made it out, in order, exactly once.
        let reassembled: Vec<u8> = frames.iter().flat_map(|f| f.payload.clone()).collect();
        assert_eq!(
            reassembled,
            http::build_request("POST", "/app/observer/ingest", &[], &body)
        );
        // No single DATA frame exceeded the recommended chunk.
        assert!(frames
            .iter()
            .filter(|f| f.flags & FLAG_DATA != 0)
            .all(|f| f.payload.len() <= RECOMMENDED_CHUNK));
    }

    #[test]
    fn sent_bytes_tracks_progress_and_stops_at_the_window_until_granted() {
        // Protocol: `.proto-ref/framing.md`, "flow control and backpressure"
        // and "fragmentation" — DATA never exceeds current credit or the
        // recommended chunk.
        let head = b"request head";
        let remaining_after_initial = RECOMMENDED_CHUNK + 8;
        let body_len = INITIAL_WINDOW - head.len() + remaining_after_initial;
        let body = vec![b'x'; body_len];
        let mut upload = WindowedUpload::new(15, head, body.len());
        let mut decoder = FrameDecoder::new();
        let mut body_offset = 0;
        pump_source(&mut upload, &body, &mut body_offset, &mut decoder);
        let initial_frames = decoder.drain().unwrap();
        assert_eq!(
            initial_frames
                .iter()
                .map(|frame| frame.payload.len())
                .sum::<usize>(),
            INITIAL_WINDOW
        );
        assert!(initial_frames
            .iter()
            .all(|frame| frame.payload.len() <= RECOMMENDED_CHUNK));
        assert!(upload.is_blocked());
        assert!(!upload.is_done());

        upload.grant(7).unwrap();
        let seven = decode_single(&upload.poll_send().unwrap().unwrap());
        assert_eq!(seven.payload.len(), 7);
        assert!(upload.poll_send().unwrap().is_none());
        assert!(upload.is_blocked());

        upload.grant((RECOMMENDED_CHUNK + 1) as u32).unwrap();
        let chunk = decode_single(&upload.poll_send().unwrap().unwrap());
        let final_byte = decode_single(&upload.poll_send().unwrap().unwrap());
        let close = decode_single(&upload.poll_send().unwrap().unwrap());
        assert_eq!(chunk.payload.len(), RECOMMENDED_CHUNK);
        assert_eq!(final_byte.payload.len(), 1);
        assert_eq!(close.flags, FLAG_CLOSE);
        assert!(upload.is_done());
        assert!(upload.poll_send().unwrap().is_none());
    }

    #[test]
    fn windowed_upload_accepts_max_remaining_credit_and_rejects_one_over() {
        let mut upload = WindowedUpload::new(7, b"request", 0);
        upload
            .grant((i32::MAX as usize - INITIAL_WINDOW) as u32)
            .unwrap();

        assert_eq!(
            upload.grant(1),
            Err(FrameViolation {
                stream_id: 7,
                flags: FLAG_WINDOW,
                length: 4,
            })
        );
    }

    #[test]
    fn windowed_upload_credit_cap_excludes_consumed_credit() {
        let head = vec![b'x'; RECOMMENDED_CHUNK];
        let mut upload = WindowedUpload::new(9, &head, 0);
        let first = decode_single(&upload.poll_send().unwrap().unwrap());
        assert_eq!(first.payload.len(), RECOMMENDED_CHUNK);
        let remaining = INITIAL_WINDOW - RECOMMENDED_CHUNK;
        let grant = i32::MAX as usize - remaining;

        upload.grant(grant as u32).unwrap();
    }

    #[test]
    fn windowed_upload_waits_for_exact_body_and_rejects_overfeed() {
        // Protocol: `.proto-ref/framing.md`, "stream lifecycle" — CLOSE means
        // the sender will write no more bytes, so it follows the exact body.
        let head = b"request head";
        let mut upload = WindowedUpload::new(11, head, 5);
        upload.feed_body(b"ab").unwrap();

        let partial = drain_permitted(&mut upload);
        assert_eq!(
            partial
                .iter()
                .flat_map(|frame| frame.payload.clone())
                .collect::<Vec<_>>(),
            [head.as_slice(), b"ab"].concat()
        );
        assert!(!upload.is_done());
        assert!(!upload.is_blocked(), "the source, not credit, is exhausted");
        assert_eq!(upload.emitted_body_len(), 2);
        assert!(upload.poll_send().unwrap().is_none());

        assert_eq!(
            upload.feed_body(b"cdef"),
            Err(UploadBodyError::DeclaredLengthExceeded)
        );
        assert_eq!(upload.body_capacity(), 3);
        upload.feed_body(b"cde").unwrap();
        assert_eq!(
            upload.feed_body(b"f"),
            Err(UploadBodyError::DeclaredLengthExceeded)
        );

        let finished = drain_permitted(&mut upload);
        assert_eq!(finished.len(), 2);
        assert_eq!(finished[0].payload, b"cde");
        assert_eq!(finished[1].flags, FLAG_CLOSE);
        assert_eq!(upload.emitted_body_len(), 5);
        assert!(upload.is_done());
        assert!(upload.poll_send().unwrap().is_none());
        assert!(upload.poll_send().unwrap().is_none());
    }

    #[test]
    fn windowed_upload_stage_is_fixed_and_overflow_is_non_mutating() {
        let declared = UPLOAD_BODY_STAGE_CAPACITY + 1;
        let mut upload = WindowedUpload::new(13, b"request head", declared);
        let over_capacity = vec![b'x'; declared];
        assert_eq!(
            upload.feed_body(&over_capacity),
            Err(UploadBodyError::StageCapacityExceeded)
        );
        assert_eq!(upload.body_capacity(), UPLOAD_BODY_STAGE_CAPACITY);

        upload
            .feed_body(&over_capacity[..UPLOAD_BODY_STAGE_CAPACITY])
            .unwrap();
        assert_eq!(upload.body_capacity(), 0);
    }

    #[test]
    fn reset_reason_parses_known_unknown_empty_and_overlong_payloads() {
        for (payload, expected) in [
            (&[RESET_PROTOCOL_ERROR][..], ResetReason::ProtocolError),
            (
                &[RESET_FLOW_CONTROL_ERROR][..],
                ResetReason::FlowControlError,
            ),
            (&[][..], ResetReason::Unspecified),
            (&[0x03][..], ResetReason::Unspecified),
            (&[0xff][..], ResetReason::Unspecified),
            (
                &[RESET_FLOW_CONTROL_ERROR, 0xaa][..],
                ResetReason::FlowControlError,
            ),
        ] {
            let mut demux = CarrierDemux::new();
            demux.open_stream(1);
            let out = demux
                .feed(
                    &Frame::new(1, FLAG_RESET, payload.to_vec())
                        .encode()
                        .unwrap(),
                )
                .unwrap();
            assert_eq!(
                out.stream_events,
                vec![stream_event(
                    1,
                    StreamItem::End(StreamEnd::Reset(expected)),
                    0
                )]
            );
        }
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
    fn response_assembler_cap_remains_exactly_four_mib() {
        let mut asm = ResponseAssembler::new(1);

        for _ in 0..(4 * 1024 * 1024 / RECOMMENDED_CHUNK) {
            let frame = Frame::new(1, FLAG_DATA, vec![b'x'; RECOMMENDED_CHUNK]);
            asm.feed(&frame.encode().unwrap()).unwrap();
        }

        assert_eq!(
            asm.feed(&Frame::new(1, FLAG_DATA, vec![b'x']).encode().unwrap())
                .unwrap_err(),
            MuxError::CapExceeded
        );
    }

    #[test]
    fn response_assembler_honors_custom_cap_before_append() {
        let mut asm = ResponseAssembler::with_cap(1, 3);
        let frame_abc = Frame::new(1, FLAG_DATA, b"abc".to_vec());
        asm.feed(&frame_abc.encode().unwrap()).unwrap();

        let frame_d = Frame::new(1, FLAG_DATA, b"d".to_vec());
        assert_eq!(
            asm.feed(&frame_d.encode().unwrap()).unwrap_err(),
            MuxError::CapExceeded
        );
    }

    #[test]
    fn response_assembler_grants_exact_wire_bytes_at_half_window() {
        let mut asm = ResponseAssembler::new(1);
        let first = Frame::new(1, FLAG_DATA, vec![b'x'; 524_247]);
        let out = asm.feed(&first.encode().unwrap()).unwrap();
        assert!(out.emit_frames.is_empty());

        let second = Frame::new(1, FLAG_DATA, vec![b'x'; 514]);
        let out = asm.feed(&second.encode().unwrap()).unwrap();
        assert_eq!(out.emit_frames.len(), 1);
        let window = decode_single(&out.emit_frames[0]);
        assert_eq!(window.window_credit(), Some(524_761));
    }

    #[test]
    fn response_assembler_subthreshold_response_emits_no_window() {
        let mut asm = ResponseAssembler::new(1);
        let frame = Frame::new(1, FLAG_DATA | FLAG_CLOSE, vec![b'x'; 333_337]);
        let out = asm.feed(&frame.encode().unwrap()).unwrap();
        assert!(out.emit_frames.is_empty());
        assert!(out.terminal_error.is_none());
    }

    #[test]
    fn response_assembler_close_suppresses_terminal_window() {
        let mut asm = ResponseAssembler::new(1);
        let frame = Frame::new(
            1,
            FLAG_DATA | FLAG_CLOSE,
            vec![b'x'; INITIAL_WINDOW / 2 + 113],
        );
        let out = asm.feed(&frame.encode().unwrap()).unwrap();

        assert!(out.emit_frames.is_empty());
        assert!(asm.is_closed());
    }

    #[test]
    fn response_assembler_over_credit_emits_one_flow_control_reset() {
        let mut asm = ResponseAssembler::new(1);
        let frame = Frame::new(1, FLAG_DATA, vec![b'x'; INITIAL_WINDOW + 19]);
        let out = asm.feed(&frame.encode().unwrap()).unwrap();

        assert_eq!(out.terminal_error, Some(MuxError::FlowControl));
        assert_eq!(out.emit_frames.len(), 1);
        let reset = decode_single(&out.emit_frames[0]);
        assert_eq!(reset.flags, FLAG_RESET);
        assert_eq!(reset.payload, vec![RESET_FLOW_CONTROL_ERROR]);
        assert_eq!(
            asm.feed(&Frame::new(1, FLAG_DATA, b"late".to_vec()).encode().unwrap())
                .unwrap_err(),
            MuxError::FlowControl
        );
    }

    #[test]
    fn http_stream_assembler_emits_head_body_and_split_head() {
        let mut asm = HttpStreamAssembler::new();

        let first = b"HTTP/1.1 200 OK\r\nContent-Type";
        let out = asm.feed_data(first).unwrap();
        assert!(out.events.is_empty());
        assert_eq!(out.auto_consumed, first.len() as u64);
        assert!(!asm.head_emitted());

        let second = b": text/event-stream\r\n\r\ndata: b\n\n";
        let out = asm.feed_data(second).unwrap();
        assert_eq!(
            out.events,
            vec![
                event(text_event_head(), 0),
                event(StreamItem::Body(b"data: b\n\n".to_vec()), 9),
            ]
        );
        assert_eq!(out.auto_consumed, (second.len() - 9) as u64);
        assert!(asm.head_emitted());

        let out = asm.feed_data(b"data: c\n\n").unwrap();
        assert_eq!(
            out.events,
            vec![event(StreamItem::Body(b"data: c\n\n".to_vec()), 9)]
        );
        assert_eq!(out.auto_consumed, 0);
        assert!(!asm.is_closed());
    }

    #[test]
    fn http_stream_assembler_dechunks_incrementally() {
        let mut asm = HttpStreamAssembler::new();

        let out = asm
            .feed_data(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
            .unwrap();
        assert!(matches!(
            out.events.as_slice(),
            [StreamEvent {
                item: StreamItem::Head(_),
                wire_cost: 0
            }]
        ));

        let out = asm.feed_data(b"4\r\nWiki\r\n").unwrap();
        assert_eq!(
            out.events,
            vec![event(StreamItem::Body(b"Wiki".to_vec()), 9)]
        );

        let out = asm.feed_data(b"5").unwrap();
        assert!(out.events.is_empty());

        let final_wire = b"\r\npedia\r\n0\r\n\r\n";
        let out = asm.feed_data(final_wire).unwrap();
        assert_eq!(
            out.events,
            vec![event(
                StreamItem::Body(b"pedia".to_vec()),
                (1 + final_wire.len()) as u64,
            )]
        );
    }

    #[test]
    fn http_stream_assembler_end_methods_mark_closed() {
        let mut close = HttpStreamAssembler::new();
        assert_eq!(close.close(), StreamItem::End(StreamEnd::Close));
        assert!(close.is_closed());

        let mut reset = HttpStreamAssembler::new();
        assert_eq!(
            reset.reset(ResetReason::ProtocolError),
            StreamItem::End(StreamEnd::Reset(ResetReason::ProtocolError))
        );
        assert!(reset.is_closed());

        let mut eof = HttpStreamAssembler::new();
        assert_eq!(eof.finish_eof(), StreamItem::End(StreamEnd::Eof));
        assert!(eof.is_closed());
    }

    #[test]
    fn carrier_demux_routes_interleaved_streams() {
        let mut demux = CarrierDemux::new();
        demux.open_stream(1);
        demux.open_stream(3);
        let wire = encode_frames(&[
            Frame::new(
                1,
                FLAG_DATA,
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\none".to_vec(),
            ),
            Frame::new(
                3,
                FLAG_DATA,
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nthree".to_vec(),
            ),
            Frame::new(1, FLAG_CLOSE, Vec::new()),
            Frame::new(3, FLAG_CLOSE, Vec::new()),
        ]);

        let out = demux.feed(&wire).unwrap();

        assert!(out.pongs.is_empty());
        assert!(out.inbound_pongs.is_empty());
        assert!(out.window_grants.is_empty());
        assert_eq!(
            out.stream_events,
            vec![
                stream_event(1, plain_head(), 0),
                stream_event(1, StreamItem::Body(b"one".to_vec()), 3),
                stream_event(3, plain_head(), 0),
                stream_event(3, StreamItem::Body(b"three".to_vec()), 5),
                stream_event(1, StreamItem::End(StreamEnd::Close), 0),
                stream_event(3, StreamItem::End(StreamEnd::Close), 0),
            ]
        );
    }

    #[test]
    fn carrier_demux_surfaces_inbound_reset_reason() {
        let mut demux = CarrierDemux::new();
        demux.open_stream(1);
        demux.open_stream(3);
        demux.open_stream(5);
        let wire = encode_frames(&[
            Frame::new(1, FLAG_RESET, vec![RESET_PROTOCOL_ERROR]),
            Frame::new(3, FLAG_RESET, vec![RESET_FLOW_CONTROL_ERROR, 0xaa]),
            Frame::new(5, FLAG_RESET, Vec::new()),
        ]);

        let out = demux.feed(&wire).unwrap();

        assert_eq!(
            out.stream_events,
            vec![
                stream_event(
                    1,
                    StreamItem::End(StreamEnd::Reset(ResetReason::ProtocolError)),
                    0,
                ),
                stream_event(
                    3,
                    StreamItem::End(StreamEnd::Reset(ResetReason::FlowControlError)),
                    0,
                ),
                stream_event(
                    5,
                    StreamItem::End(StreamEnd::Reset(ResetReason::Unspecified)),
                    0,
                ),
            ]
        );
        assert!(out.emit_frames.is_empty());
        assert!(out.violations.is_empty());
    }

    #[test]
    fn carrier_demux_rejects_invalid_flag_combinations_without_delivering_payload() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nforbidden";
        for flags in [FLAG_DATA | FLAG_RESET, FLAG_DATA | FLAG_WINDOW] {
            let mut demux = CarrierDemux::new();
            demux.open_stream(1);
            demux.open_stream(3);
            let wire = encode_frames(&[
                Frame::new(1, flags, response.to_vec()),
                Frame::new(
                    3,
                    FLAG_DATA | FLAG_CLOSE,
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nok".to_vec(),
                ),
            ]);

            let out = demux.feed(&wire).unwrap();

            assert_eq!(out.emit_frames.len(), 1);
            let reset = decode_single(&out.emit_frames[0]);
            assert_eq!(reset.stream_id, 1);
            assert_eq!(reset.flags, FLAG_RESET);
            assert_eq!(reset.payload, vec![RESET_PROTOCOL_ERROR]);
            assert_eq!(
                out.stream_events,
                vec![
                    stream_event(
                        1,
                        StreamItem::End(StreamEnd::Reset(ResetReason::ProtocolError)),
                        0,
                    ),
                    stream_event(3, plain_head(), 0),
                    stream_event(3, StreamItem::Body(b"ok".to_vec()), 2),
                    stream_event(3, StreamItem::End(StreamEnd::Close), 0),
                ]
            );
            assert_eq!(
                out.violations,
                vec![FrameViolation {
                    stream_id: 1,
                    flags,
                    length: response.len(),
                }]
            );
        }

        let mut demux = CarrierDemux::new();
        let invalid = Frame::new(9, FLAG_DATA | FLAG_WINDOW, vec![0; 4]);
        let out = demux.feed(&invalid.encode().unwrap()).unwrap();
        assert_eq!(out.emit_frames.len(), 1);
        assert_eq!(decode_single(&out.emit_frames[0]).stream_id, 9);
        assert_eq!(out.violations.len(), 1);
        assert!(out.stream_events.is_empty());

        let zero = Frame::new(11, 0, Vec::new());
        let out = demux.feed(&zero.encode().unwrap()).unwrap();
        assert_eq!(out.emit_frames.len(), 1);
        assert_eq!(decode_single(&out.emit_frames[0]).stream_id, 11);
        assert_eq!(out.violations, vec![frame_violation(&zero)]);
    }

    #[test]
    fn carrier_demux_rejects_window_close_as_protocol_error() {
        let mut demux = CarrierDemux::new();
        demux.open_stream(1);
        let frame = Frame::new(1, FLAG_WINDOW | FLAG_CLOSE, 17u32.to_be_bytes().to_vec());

        let out = demux.feed(&frame.encode().unwrap()).unwrap();

        assert!(out.window_grants.is_empty());
        assert_eq!(out.emit_frames.len(), 1);
        assert_eq!(
            decode_single(&out.emit_frames[0]).payload,
            vec![RESET_PROTOCOL_ERROR]
        );
        assert_eq!(
            out.stream_events,
            vec![stream_event(
                1,
                StreamItem::End(StreamEnd::Reset(ResetReason::ProtocolError)),
                0,
            )]
        );
        assert_eq!(out.violations, vec![frame_violation(&frame)]);
    }

    #[test]
    fn carrier_demux_rejects_inbound_open_for_dialer_role() {
        let mut demux = CarrierDemux::new();
        demux.open_stream(1);
        let known = Frame::new(1, FLAG_OPEN | FLAG_DATA, b"not delivered".to_vec());
        let unknown = Frame::new(8, FLAG_OPEN, Vec::new());
        let wire = encode_frames(&[known.clone(), unknown.clone()]);

        let out = demux.feed(&wire).unwrap();

        assert_eq!(out.emit_frames.len(), 2);
        assert_eq!(
            out.violations,
            vec![frame_violation(&known), frame_violation(&unknown)]
        );
        assert_eq!(
            out.stream_events,
            vec![stream_event(
                1,
                StreamItem::End(StreamEnd::Reset(ResetReason::ProtocolError)),
                0,
            )]
        );
    }

    #[test]
    fn carrier_demux_rejects_ping_pong_on_nonzero_streams() {
        let mut demux = CarrierDemux::new();
        demux.open_stream(1);
        let ping = Frame::new(1, FLAG_PING, vec![0; 8]);
        let pong = Frame::new(9, FLAG_PONG, vec![1; 8]);
        let wire = encode_frames(&[ping.clone(), pong.clone()]);

        let out = demux.feed(&wire).unwrap();

        assert!(out.pongs.is_empty());
        assert!(out.inbound_pongs.is_empty());
        assert_eq!(out.emit_frames.len(), 2);
        assert_eq!(
            out.violations,
            vec![frame_violation(&ping), frame_violation(&pong)]
        );
        assert_eq!(
            out.stream_events,
            vec![stream_event(
                1,
                StreamItem::End(StreamEnd::Reset(ResetReason::ProtocolError)),
                0,
            )]
        );
    }

    #[test]
    fn carrier_demux_rejects_malformed_window_payload() {
        let mut demux = CarrierDemux::new();
        demux.open_stream(1);
        let frame = Frame::new(1, FLAG_WINDOW, vec![0, 0, 1]);

        let out = demux.feed(&frame.encode().unwrap()).unwrap();

        assert!(out.window_grants.is_empty());
        assert_eq!(out.emit_frames.len(), 1);
        assert_eq!(out.violations, vec![frame_violation(&frame)]);
        assert_eq!(
            out.stream_events,
            vec![stream_event(
                1,
                StreamItem::End(StreamEnd::Reset(ResetReason::ProtocolError)),
                0,
            )]
        );
    }

    #[test]
    fn carrier_demux_stream_zero_misuse_is_tunnel_fatal() {
        let cases = [
            Frame::new(0, FLAG_DATA, b"x".to_vec()),
            Frame::new(0, FLAG_WINDOW, 1u32.to_be_bytes().to_vec()),
            Frame::new(0, FLAG_OPEN, Vec::new()),
            Frame::new(0, FLAG_CLOSE, Vec::new()),
            Frame::new(0, FLAG_RESET, vec![RESET_PROTOCOL_ERROR]),
            Frame::new(0, 0, Vec::new()),
            Frame::new(0, FLAG_PING, vec![0; 7]),
            Frame::new(0, FLAG_PING | FLAG_PONG, vec![0; 8]),
        ];

        for frame in cases {
            let mut demux = CarrierDemux::new();
            assert_eq!(
                demux.feed(&frame.encode().unwrap()).unwrap_err(),
                MuxError::Protocol(frame_violation(&frame))
            );
        }
    }

    #[test]
    fn carrier_demux_tunnel_fatal_is_latched() {
        let mut demux = CarrierDemux::new();
        let fatal = Frame::new(0, FLAG_DATA, b"fatal".to_vec());
        let expected = MuxError::Protocol(frame_violation(&fatal));
        assert_eq!(demux.feed(&fatal.encode().unwrap()).unwrap_err(), expected);

        let ping = Frame::control_ping([1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(
            demux.feed(&ping.encode().unwrap()).unwrap_err(),
            MuxError::Protocol(frame_violation(&fatal))
        );
    }

    #[test]
    fn carrier_demux_reserved_flag_remains_frame_fatal() {
        let wire = [0, 0, 0, 1, FLAG_RESERVED_MASK, 0, 0, 0];
        let mut demux = CarrierDemux::new();
        assert_eq!(
            demux.feed(&wire).unwrap_err(),
            MuxError::Frame(FrameError::ReservedFlag(FLAG_RESERVED_MASK))
        );
    }

    #[test]
    fn carrier_demux_control_and_window_outputs_are_tagged() {
        let mut demux = CarrierDemux::new();
        demux.open_stream(1);
        demux.open_stream(3);
        let ping_nonce = [9, 8, 7, 6, 5, 4, 3, 2];
        let pong_nonce = [1, 3, 5, 7, 9, 11, 13, 15];
        let wire = encode_frames(&[
            Frame::control_ping(ping_nonce),
            Frame::new(0, FLAG_PONG, pong_nonce.to_vec()),
            Frame::new(1, FLAG_WINDOW, vec![0x00, 0x08, 0x00, 0x00]),
            Frame::new(3, FLAG_WINDOW, vec![0x00, 0x10, 0x00, 0x00]),
            Frame::new(5, FLAG_WINDOW, vec![0x00, 0x20, 0x00, 0x00]),
        ]);

        let out = demux.feed(&wire).unwrap();

        assert_eq!(out.pongs.len(), 1);
        let mut dec = FrameDecoder::new();
        dec.feed(&out.pongs[0]);
        let pong = dec.next_frame().unwrap().unwrap();
        assert_eq!(pong.flags, FLAG_PONG);
        assert_eq!(pong.stream_id, 0);
        assert_eq!(pong.payload, ping_nonce.to_vec());
        assert_eq!(out.inbound_pongs, vec![pong_nonce]);
        assert_eq!(out.window_grants, vec![(1, 512 * 1024), (3, 1024 * 1024)]);
        assert!(out.stream_events.is_empty());
        assert_eq!(out.emit_frames.len(), 1);
        let reset = decode_single(&out.emit_frames[0]);
        assert_eq!(reset.stream_id, 5);
        assert_eq!(reset.flags, FLAG_RESET);
        assert_eq!(reset.payload, vec![RESET_PROTOCOL_ERROR]);
        assert_eq!(
            out.violations,
            vec![FrameViolation {
                stream_id: 5,
                flags: FLAG_WINDOW,
                length: 4,
            }]
        );
    }

    #[test]
    fn carrier_demux_discriminates_unknown_or_closed_stream_frames() {
        let mut demux = CarrierDemux::new();
        demux.open_stream(1);
        let wire = encode_frames(&[
            Frame::new(9, FLAG_DATA, b"ignored".to_vec()),
            Frame::new(11, FLAG_WINDOW, 17u32.to_be_bytes().to_vec()),
            Frame::new(13, FLAG_CLOSE, Vec::new()),
            Frame::new(15, FLAG_RESET, vec![RESET_PROTOCOL_ERROR]),
            Frame::new(
                1,
                FLAG_DATA | FLAG_CLOSE,
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nok".to_vec(),
            ),
        ]);

        let out = demux.feed(&wire).unwrap();
        assert_eq!(
            out.stream_events,
            vec![
                stream_event(1, plain_head(), 0),
                stream_event(1, StreamItem::Body(b"ok".to_vec()), 2),
                stream_event(1, StreamItem::End(StreamEnd::Close), 0),
            ]
        );
        assert_eq!(out.emit_frames.len(), 2);
        assert_eq!(decode_single(&out.emit_frames[0]).stream_id, 9);
        assert_eq!(decode_single(&out.emit_frames[1]).stream_id, 11);
        assert_eq!(
            out.violations,
            vec![
                FrameViolation {
                    stream_id: 9,
                    flags: FLAG_DATA,
                    length: 7,
                },
                FrameViolation {
                    stream_id: 11,
                    flags: FLAG_WINDOW,
                    length: 4,
                },
            ]
        );

        let out = demux
            .feed(&Frame::new(1, FLAG_DATA, b"late".to_vec()).encode().unwrap())
            .unwrap();
        assert!(out.stream_events.is_empty());
        assert_eq!(out.emit_frames.len(), 1);
        let reset = decode_single(&out.emit_frames[0]);
        assert_eq!(reset.stream_id, 1);
        assert_eq!(reset.payload, vec![RESET_PROTOCOL_ERROR]);
        assert_eq!(out.violations.len(), 1);
    }

    #[test]
    fn carrier_demux_data_close_orders_body_before_end() {
        let mut demux = CarrierDemux::new();
        demux.open_stream(1);
        let frame = Frame::new(
            1,
            FLAG_DATA | FLAG_CLOSE,
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nbody".to_vec(),
        );

        let out = demux.feed(&frame.encode().unwrap()).unwrap();

        assert_eq!(
            out.stream_events,
            vec![
                stream_event(1, plain_head(), 0),
                stream_event(1, StreamItem::Body(b"body".to_vec()), 4),
                stream_event(1, StreamItem::End(StreamEnd::Close), 0),
            ]
        );
    }

    #[test]
    fn carrier_demux_isolates_http_parse_error_to_one_stream() {
        let mut demux = CarrierDemux::new();
        demux.open_stream(1);
        demux.open_stream(3);
        let wire = encode_frames(&[
            Frame::new(1, FLAG_DATA, b"GARBAGE NOT HTTP\r\n\r\n".to_vec()),
            Frame::new(
                3,
                FLAG_DATA | FLAG_CLOSE,
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nok".to_vec(),
            ),
        ]);

        let out = demux.feed(&wire).unwrap();

        assert_eq!(
            out.stream_events,
            vec![
                stream_event(
                    1,
                    StreamItem::End(StreamEnd::Reset(ResetReason::Unspecified)),
                    0,
                ),
                stream_event(3, plain_head(), 0),
                stream_event(3, StreamItem::Body(b"ok".to_vec()), 2),
                stream_event(3, StreamItem::End(StreamEnd::Close), 0),
            ]
        );

        let out = demux
            .feed(
                &Frame::new(1, FLAG_DATA | FLAG_CLOSE, b"late".to_vec())
                    .encode()
                    .unwrap(),
            )
            .unwrap();
        assert!(out.stream_events.is_empty());
        assert_eq!(out.emit_frames.len(), 1);
        assert_eq!(
            decode_single(&out.emit_frames[0]).payload,
            vec![RESET_PROTOCOL_ERROR]
        );
        assert_eq!(out.violations.len(), 1);
    }

    #[test]
    fn carrier_demux_isolates_head_cap_to_one_stream() {
        let mut demux = CarrierDemux::new();
        demux.open_stream(1);
        demux.open_stream(3);
        for _ in 0..(MAX_ASSEMBLED_BYTES / RECOMMENDED_CHUNK) {
            let head = Frame::new(1, FLAG_DATA, vec![b'x'; RECOMMENDED_CHUNK]);
            demux.feed(&head.encode().unwrap()).unwrap();
        }
        let overflow = Frame::new(1, FLAG_DATA, b"x".to_vec());
        let out = demux.feed(&overflow.encode().unwrap()).unwrap();

        assert_eq!(
            out.stream_events,
            vec![stream_event(
                1,
                StreamItem::End(StreamEnd::Reset(ResetReason::Unspecified)),
                0,
            )]
        );
        let removed = demux
            .feed(
                &Frame::new(1, FLAG_DATA, b"again".to_vec())
                    .encode()
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(decode_single(&removed.emit_frames[0]).stream_id, 1);
        let sibling = demux
            .feed(
                &Frame::new(
                    3,
                    FLAG_DATA | FLAG_CLOSE,
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nok".to_vec(),
                )
                .encode()
                .unwrap(),
            )
            .unwrap();
        assert!(sibling.stream_events.iter().any(|(id, _)| *id == 3));
    }

    #[test]
    fn carrier_demux_over_credit_resets_only_offending_stream() {
        let mut demux = CarrierDemux::new();
        demux.open_stream(1);
        demux.open_stream(3);
        let wire = encode_frames(&[
            Frame::new(1, FLAG_DATA, vec![b'x'; INITIAL_WINDOW + 37]),
            Frame::new(
                3,
                FLAG_DATA | FLAG_CLOSE,
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nok".to_vec(),
            ),
        ]);

        let out = demux.feed(&wire).unwrap();

        assert_eq!(out.emit_frames.len(), 1);
        let reset = decode_single(&out.emit_frames[0]);
        assert_eq!(reset.stream_id, 1);
        assert_eq!(reset.flags, FLAG_RESET);
        assert_eq!(reset.payload, vec![RESET_FLOW_CONTROL_ERROR]);
        assert_eq!(
            out.stream_events,
            vec![
                stream_event(
                    1,
                    StreamItem::End(StreamEnd::Reset(ResetReason::FlowControlError)),
                    0,
                ),
                stream_event(3, plain_head(), 0),
                stream_event(3, StreamItem::Body(b"ok".to_vec()), 2),
                stream_event(3, StreamItem::End(StreamEnd::Close), 0),
            ]
        );
        assert_eq!(demux.consume(1, 1).unwrap(), None);
        assert_eq!(demux.consume(3, 1).unwrap(), None);
    }

    #[test]
    fn carrier_consume_after_close_emits_no_late_window() {
        let mut demux = CarrierDemux::new();
        demux.open_stream(1);
        let response = Frame::new(
            1,
            FLAG_DATA | FLAG_CLOSE,
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nbody".to_vec(),
        );
        let out = demux.feed(&response.encode().unwrap()).unwrap();
        let body_cost = out
            .stream_events
            .iter()
            .find_map(|(_, event)| match &event.item {
                StreamItem::Body(_) => Some(event.wire_cost),
                _ => None,
            })
            .unwrap();

        assert_eq!(demux.consume(1, body_cost).unwrap(), None);
    }

    #[test]
    fn carrier_close_suppresses_decode_time_window() {
        let mut demux = CarrierDemux::new();
        demux.open_stream(1);
        let mut response = b"HTTP/1.1 200 OK\r\nX-Padding: ".to_vec();
        response.extend(vec![b'x'; INITIAL_WINDOW / 2]);
        response.extend_from_slice(b"\r\n\r\n");
        let frame = Frame::new(1, FLAG_DATA | FLAG_CLOSE, response);

        let out = demux.feed(&frame.encode().unwrap()).unwrap();

        assert!(out.emit_frames.is_empty());
        assert!(out
            .stream_events
            .iter()
            .any(|(_, event)| event.item == StreamItem::End(StreamEnd::Close)));
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

    #[test]
    fn response_assembler_with_cap_enforces_exact_limit() {
        let mut asm = ResponseAssembler::with_cap(1, 3);
        let frame1 = Frame::new(1, FLAG_DATA, b"abc".to_vec()).encode().unwrap();
        assert!(asm.feed(&frame1).is_ok());

        let frame2 = Frame::new(1, FLAG_DATA, b"d".to_vec()).encode().unwrap();
        assert_eq!(asm.feed(&frame2).unwrap_err(), MuxError::CapExceeded);
    }

    #[test]
    fn response_assembler_default_cap_is_max_assembled_bytes() {
        let mut asm = ResponseAssembler::new(1);
        for _ in 0..(MAX_ASSEMBLED_BYTES / INITIAL_WINDOW) {
            let frame = Frame::new(1, FLAG_DATA, vec![b'x'; INITIAL_WINDOW]);
            asm.feed(&frame.encode().unwrap()).unwrap();
        }
        assert_eq!(
            asm.feed(&Frame::new(1, FLAG_DATA, b"x".to_vec()).encode().unwrap())
                .unwrap_err(),
            MuxError::CapExceeded
        );
        assert_eq!(MAX_ASSEMBLED_BYTES, 4 * 1024 * 1024);
    }
}
