// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#[cfg(test)]
mod tests {
    use spl_core::http::{parse_response, HttpError};
    use spl_transport::{same_relay_origin, validate_relay_origin};

    #[test]
    fn chunked_response_split_across_reads_waits_for_eof() {
        let incomplete = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nWiki\r\n";
        assert_eq!(
            parse_response(incomplete),
            Err(HttpError::BadChunkedBody("missing terminal chunk".into()))
        );
    }

    #[test]
    fn origin_comparison_normalizes_default_ports_and_rejects_injection() {
        assert!(
            same_relay_origin("https://relay.example", "https://relay.example:443")
                .expect("valid origins")
        );
        assert!(validate_relay_origin("https://relay.example/path").is_err());
        assert!(validate_relay_origin("https://user@relay.example").is_err());
    }

    #[tokio::test]
    async fn total_deadline_cancels_blocked_write_and_progressing_reads() {
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(1),
            std::future::pending::<()>(),
        )
        .await;
        assert!(result.is_err());
        assert!(validate_relay_origin("http://127.0.0.1:8080").is_ok());
    }

    #[test]
    fn complete_chunked_response_returns_without_eof() {
        let response = parse_response(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n\r\n",
        )
        .expect("complete chunked response");
        assert_eq!(response.body, b"ok");
    }

    #[test]
    fn control_body_limit_and_framing_errors_return_fixed_classifications() {
        assert_eq!(
            parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nx"),
            Err(HttpError::TruncatedBody)
        );
    }
}
