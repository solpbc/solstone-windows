// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#[cfg(test)]
mod tests {
    use spl_core::frame::MAX_PAYLOAD;
    use spl_core::http::*;
    #[test]
    fn build_request_owns_host_accept_and_content_length() {
        let headers = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("X-Solstone-Observer".to_string(), "handle123".to_string()),
            // Caller attempts to set framing-owned headers — must be dropped.
            ("host".to_string(), "evil".to_string()),
            ("content-length".to_string(), "999".to_string()),
        ];
        let bytes = build_request("POST", "/app/devices/ingest", &headers, b"payload");
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("POST /app/devices/ingest HTTP/1.1\r\n"));
        assert!(text.contains("host: spl.local\r\n"));
        assert!(text.contains("accept: application/json\r\n"));
        assert!(text.contains("Content-Type: application/json\r\n"));
        assert!(text.contains("X-Solstone-Observer: handle123\r\n"));
        assert!(text.contains("content-length: 7\r\n"));
        // The caller's spoofed host/content-length never reach the wire.
        assert!(!text.contains("host: evil"));
        assert!(!text.contains("content-length: 999"));
        assert!(text.ends_with("\r\n\r\npayload"));
    }

    #[test]
    fn explicit_host_targets_the_loopback_leg_without_admitting_a_caller_header() {
        let headers = vec![
            ("Cookie".to_string(), "__solstone_journal_cap=x".to_string()),
            // Still framing-owned: a caller header can never retarget the request.
            ("host".to_string(), "evil".to_string()),
        ];
        let text = String::from_utf8(build_request_with_host(
            "GET",
            "/x",
            "127.0.0.1:8080",
            &headers,
            b"",
        ))
        .unwrap();
        assert!(text.contains("host: 127.0.0.1:8080\r\n"));
        assert!(!text.contains("host: evil"));
        assert!(!text.contains("host: spl.local"));
        assert_eq!(text.matches("host: ").count(), 1);
    }

    #[test]
    fn build_request_defaults_to_the_pinned_journal_host() {
        let text = String::from_utf8(build_request("GET", "/x", &[], b"")).unwrap();
        assert!(text.contains(&format!("host: {DEFAULT_HTTP_HOST}\r\n")));
    }

    #[test]
    fn caller_can_override_accept() {
        let headers = vec![("Accept".to_string(), "*/*".to_string())];
        let text = String::from_utf8(build_request("GET", "/x", &headers, b"")).unwrap();
        assert!(text.contains("Accept: */*\r\n"));
        assert!(!text.contains("accept: application/json"));
    }

    #[test]
    fn parses_content_length_response() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 4\r\n\r\n{ok}trailing-garbage";
        let resp = parse_response(raw).unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, b"{ok}");
        assert_eq!(resp.header("content-type"), Some("application/json"));
    }

    #[test]
    fn short_content_length_is_an_error() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nhi";
        assert_eq!(parse_response(raw).unwrap_err(), HttpError::TruncatedBody);
    }

    #[test]
    fn parses_chunked_response() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n";
        let resp = parse_response(raw).unwrap();
        assert_eq!(resp.body, b"Wikipedia");
    }

    #[test]
    fn chunked_response_without_terminal_chunk_is_an_error() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nWiki\r\n";
        assert_eq!(
            parse_response(raw).unwrap_err(),
            HttpError::BadChunkedBody("missing terminal chunk".into())
        );
    }

    #[test]
    fn parses_401_with_body() {
        let raw = b"HTTP/1.1 401 UNAUTHORIZED\r\nContent-Length: 16\r\n\r\n{\"error\":\"auth\"}";
        let resp = parse_response(raw).unwrap();
        assert_eq!(resp.status, 401);
        assert!(!resp.is_success());
    }

    #[test]
    fn parse_head_lowercases_headers() {
        let (status, headers) =
            parse_head(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream").unwrap();
        assert_eq!(status, 200);
        assert_eq!(
            headers,
            vec![("content-type".to_string(), "text/event-stream".to_string())]
        );
    }

    #[test]
    fn parse_head_accepts_401() {
        let (status, headers) =
            parse_head(b"HTTP/1.1 401 UNAUTHORIZED\r\nWWW-Authenticate: Bearer").unwrap();
        assert_eq!(status, 401);
        assert_eq!(
            headers,
            vec![("www-authenticate".to_string(), "Bearer".to_string())]
        );
    }

    #[test]
    fn chunked_decoder_byte_at_a_time_reconstructs_body() {
        let raw = b"4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n";
        let mut decoder = ChunkedDecoder::new();
        let mut out = Vec::new();
        for byte in raw {
            out.extend(decoder.push(&[*byte]).unwrap());
        }
        assert_eq!(out, b"Wikipedia");
    }

    #[test]
    fn chunked_decoder_handles_arbitrary_splits() {
        let mut decoder = ChunkedDecoder::new();
        let mut out = Vec::new();
        out.extend(decoder.push(b"4\r\nWi").unwrap());
        out.extend(decoder.push(b"ki\r\n5").unwrap());
        out.extend(decoder.push(b"\r\npedia\r\n0\r").unwrap());
        out.extend(decoder.push(b"\n\r\nignored").unwrap());
        assert_eq!(out, b"Wikipedia");
        assert_eq!(decoder.push(b"more").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn dechunk_rejects_usize_max_chunk_size() {
        assert_eq!(
            dechunk(b"ffffffffffffffff\r\nshort").unwrap_err(),
            HttpError::BadChunkedBody(format!(
                "chunk size {} exceeds max {MAX_PAYLOAD}",
                usize::MAX
            ))
        );
    }

    #[test]
    fn dechunk_rejects_first_over_cap_size_before_truncated_chunk() {
        assert_eq!(
            dechunk(b"1000000\r\nshort").unwrap_err(),
            HttpError::BadChunkedBody(format!(
                "chunk size {} exceeds max {MAX_PAYLOAD}",
                MAX_PAYLOAD + 1
            ))
        );
    }

    #[test]
    fn dechunk_accepts_chunk_at_max_payload() {
        let mut raw = format!("{MAX_PAYLOAD:x}\r\n").into_bytes();
        let body_start = raw.len();
        raw.resize(body_start + MAX_PAYLOAD, b'x');
        raw.extend_from_slice(b"\r\n0\r\n\r\n");

        let body = dechunk(&raw).unwrap();

        assert_eq!(body.len(), MAX_PAYLOAD);
        assert_eq!(body[0], b'x');
        assert_eq!(body[MAX_PAYLOAD / 2], b'x');
        assert_eq!(body[MAX_PAYLOAD - 1], b'x');
    }

    #[test]
    fn chunked_decoder_rejects_over_cap_size_and_latches_failure() {
        let mut decoder = ChunkedDecoder::new();

        assert_eq!(
            decoder.push(b"2000000\r\n").unwrap_err(),
            HttpError::BadChunkedBody(format!(
                "chunk size {} exceeds max {MAX_PAYLOAD}",
                0x2000000
            ))
        );
        assert!(!decoder.is_complete());

        let mut trickled = 0;
        for bytes in [&b"a"[..], &b"bc"[..], &b"def"[..]] {
            trickled += bytes.len();
            assert_eq!(
                decoder.push(bytes).unwrap_err(),
                HttpError::BadChunkedBody("decoder previously failed".into())
            );
            assert!(trickled <= 6);
        }
    }

    #[test]
    fn chunked_decoder_allows_total_body_over_per_chunk_cap() {
        const CHUNK_SIZE: usize = 8 * 1024 * 1024;

        let mut decoder = ChunkedDecoder::new();
        let mut body = Vec::new();
        for byte in [b'a', b'b', b'c'] {
            let mut chunk = format!("{CHUNK_SIZE:x}\r\n").into_bytes();
            let data_start = chunk.len();
            chunk.resize(data_start + CHUNK_SIZE, byte);
            chunk.extend_from_slice(b"\r\n");
            body.extend(decoder.push(&chunk).unwrap());
        }
        assert_eq!(decoder.push(b"0\r\n\r\n").unwrap(), Vec::<u8>::new());

        assert_eq!(body.len(), CHUNK_SIZE * 3);
        assert!(body.len() > MAX_PAYLOAD);
        assert_eq!(body[0], b'a');
        assert_eq!(body[CHUNK_SIZE - 1], b'a');
        assert_eq!(body[CHUNK_SIZE], b'b');
        assert_eq!(body[CHUNK_SIZE * 2], b'c');
        assert_eq!(body[CHUNK_SIZE * 3 - 1], b'c');
    }

    #[test]
    fn missing_terminator_is_an_error() {
        assert_eq!(
            parse_response(b"HTTP/1.1 200 OK\r\n").unwrap_err(),
            HttpError::MissingTerminator
        );
    }
}
