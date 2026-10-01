// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Health serialization.
//!
//! The [`HealthDump`](observer_model::HealthDump) is rendered to JSON here, once,
//! and that same bytes-shape is what the CLI prints for `--dump-state`, what the
//! localhost `/healthz` endpoint returns, and what rides the `health://changed`
//! event. Keeping the encoding in one pure crate means the three transports can
//! never disagree about what "observing" looks like on the wire.

#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::time::Duration;

use observer_model::{HealthDump, Presence};

/// Render a [`HealthDump`] as the canonical `--dump-state` / `/healthz` JSON
/// (pretty, stable). Returns an error only on a serializer fault.
pub fn to_pretty_json(dump: &HealthDump) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(dump)
}

/// Render a [`HealthDump`] as compact JSON for event payloads.
pub fn to_compact_json(dump: &HealthDump) -> Result<String, serde_json::Error> {
    serde_json::to_string(dump)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthQuery {
    Unavailable,
    Response(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthParseError {
    Empty,
    Malformed,
    NonSuccess,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DumpChoice {
    Live(Box<HealthDump>),
    NotRunning,
    Failed,
}

/// Parse raw HTTP response bytes into a [`HealthDump`].
pub fn parse_health_response(bytes: &[u8]) -> Result<HealthDump, HealthParseError> {
    let text = std::str::from_utf8(bytes).map_err(|_| HealthParseError::Malformed)?;
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or(HealthParseError::Malformed)?;
    let status_line = head.lines().next().ok_or(HealthParseError::Malformed)?;
    let mut parts = status_line.split_whitespace();
    let version = parts.next().ok_or(HealthParseError::Malformed)?;
    if version != "HTTP/1.0" && version != "HTTP/1.1" {
        return Err(HealthParseError::Malformed);
    }
    let code_str = parts.next().ok_or(HealthParseError::Malformed)?;
    if code_str.len() != 3 || !code_str.chars().all(|c| c.is_ascii_digit()) {
        return Err(HealthParseError::Malformed);
    }
    let code: u16 = code_str.parse().map_err(|_| HealthParseError::Malformed)?;
    if !(200..=299).contains(&code) {
        return Err(HealthParseError::NonSuccess);
    }
    if body.is_empty() {
        return Err(HealthParseError::Empty);
    }
    serde_json::from_str::<HealthDump>(body).map_err(|_| HealthParseError::Malformed)
}

/// Query a health endpoint over loopback TCP with timeouts.
pub fn fetch_health_response(
    addr: SocketAddr,
    connect_timeout: Duration,
    io_timeout: Duration,
) -> HealthQuery {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    let mut stream = match TcpStream::connect_timeout(&addr, connect_timeout) {
        Ok(s) => s,
        Err(_) => return HealthQuery::Unavailable,
    };
    if stream.set_read_timeout(Some(io_timeout)).is_err() {
        return HealthQuery::Unavailable;
    }
    if stream.set_write_timeout(Some(io_timeout)).is_err() {
        return HealthQuery::Unavailable;
    }
    if stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return HealthQuery::Unavailable;
    }
    let mut response = Vec::new();
    if stream.read_to_end(&mut response).is_err() {
        return HealthQuery::Unavailable;
    }
    HealthQuery::Response(response)
}

/// Decide the dump choice from the query outcome and process presence.
pub fn decide_dump(query: HealthQuery, presence: Presence) -> DumpChoice {
    match query {
        HealthQuery::Response(bytes) => match parse_health_response(&bytes) {
            Ok(dump) => DumpChoice::Live(Box::new(dump)),
            Err(_) => match presence {
                Presence::Absent => DumpChoice::NotRunning,
                Presence::Present | Presence::Unknown => DumpChoice::Failed,
            },
        },
        HealthQuery::Unavailable => match presence {
            Presence::Absent => DumpChoice::NotRunning,
            Presence::Present | Presence::Unknown => DumpChoice::Failed,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use observer_model::AppPhase;

    fn sample() -> HealthDump {
        HealthDump {
            app_state: AppPhase::Idle,
            sources: vec![],
            frame_rate: None,
            segment_dir: None,
            segment_seconds_remaining: None,
            engine_ready: false,
            version: "0.1.0".into(),
            sync: observer_model::SyncSnapshot::default(),
            screen_encoder: None,
            exclusions: None,
            storage: None,
            pause: None,
            views: Default::default(),
            pump_degraded: false,
            listener_faults: observer_model::ListenerFaults::default(),
        }
    }

    #[test]
    fn round_trips_through_json() {
        let dump = sample();
        let json = to_pretty_json(&dump).unwrap();
        let back: HealthDump = serde_json::from_str(&json).unwrap();
        assert_eq!(dump, back);
    }

    #[test]
    fn app_state_serializes_as_snake_case_token() {
        let json = to_compact_json(&sample()).unwrap();
        assert!(json.contains("\"app_state\":\"idle\""));
    }

    #[test]
    fn parse_health_response_accepts_valid_200() {
        let dump = sample();
        let body = to_pretty_json(&dump).unwrap();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let parsed = parse_health_response(response.as_bytes()).unwrap();
        assert_eq!(parsed, dump);

        // HTTP/1.0 is also accepted
        let response_10 = format!("HTTP/1.0 200 OK\r\n\r\n{}", body);
        let parsed_10 = parse_health_response(response_10.as_bytes()).unwrap();
        assert_eq!(parsed_10, dump);
    }

    #[test]
    fn parse_health_response_rejects_malformed_and_nonsuccess() {
        let dump = sample();
        let body = to_pretty_json(&dump).unwrap();

        // 500 with valid body -> NonSuccess
        let resp_500 = format!("HTTP/1.1 500 Internal Server Error\r\n\r\n{}", body);
        assert_eq!(
            parse_health_response(resp_500.as_bytes()),
            Err(HealthParseError::NonSuccess)
        );

        // 404 -> NonSuccess
        let resp_404 = b"HTTP/1.1 404 Not Found\r\n\r\n";
        assert_eq!(
            parse_health_response(resp_404),
            Err(HealthParseError::NonSuccess)
        );

        // Empty body -> Empty
        let resp_empty = b"HTTP/1.1 200 OK\r\n\r\n";
        assert_eq!(
            parse_health_response(resp_empty),
            Err(HealthParseError::Empty)
        );

        // Missing separator -> Malformed
        let resp_no_sep = b"HTTP/1.1 200 OK";
        assert_eq!(
            parse_health_response(resp_no_sep),
            Err(HealthParseError::Malformed)
        );

        // Truncated JSON -> Malformed
        let resp_trunc = b"HTTP/1.1 200 OK\r\n\r\n{\"app_state\":";
        assert_eq!(
            parse_health_response(resp_trunc),
            Err(HealthParseError::Malformed)
        );

        // Non-object JSON -> Malformed
        let resp_array = b"HTTP/1.1 200 OK\r\n\r\n[1, 2, 3]";
        assert_eq!(
            parse_health_response(resp_array),
            Err(HealthParseError::Malformed)
        );
    }

    #[test]
    fn decide_dump_truth_table() {
        let dump = sample();
        let valid_body = to_pretty_json(&dump).unwrap();
        let valid_resp = format!("HTTP/1.1 200 OK\r\n\r\n{}", valid_body).into_bytes();
        let bad_resp = b"HTTP/1.1 500 Internal Server Error\r\n\r\n{}".to_vec();

        // Live regardless of presence when parse succeeds
        assert_eq!(
            decide_dump(HealthQuery::Response(valid_resp.clone()), Presence::Absent),
            DumpChoice::Live(Box::new(dump.clone()))
        );
        assert_eq!(
            decide_dump(HealthQuery::Response(valid_resp.clone()), Presence::Present),
            DumpChoice::Live(Box::new(dump.clone()))
        );
        assert_eq!(
            decide_dump(HealthQuery::Response(valid_resp), Presence::Unknown),
            DumpChoice::Live(Box::new(dump))
        );

        // Bad response + Absent -> NotRunning
        assert_eq!(
            decide_dump(HealthQuery::Response(bad_resp.clone()), Presence::Absent),
            DumpChoice::NotRunning
        );
        // Bad response + Present -> Failed
        assert_eq!(
            decide_dump(HealthQuery::Response(bad_resp.clone()), Presence::Present),
            DumpChoice::Failed
        );
        // Bad response + Unknown -> Failed
        assert_eq!(
            decide_dump(HealthQuery::Response(bad_resp), Presence::Unknown),
            DumpChoice::Failed
        );

        // Unavailable + Absent -> NotRunning
        assert_eq!(
            decide_dump(HealthQuery::Unavailable, Presence::Absent),
            DumpChoice::NotRunning
        );
        // Unavailable + Present -> Failed
        assert_eq!(
            decide_dump(HealthQuery::Unavailable, Presence::Present),
            DumpChoice::Failed
        );
        // Unavailable + Unknown -> Failed
        assert_eq!(
            decide_dump(HealthQuery::Unavailable, Presence::Unknown),
            DumpChoice::Failed
        );
    }

    #[test]
    fn ephemeral_listener_fetch_health_response() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let connect_timeout = Duration::from_millis(500);
        let io_timeout = Duration::from_millis(500);

        // Success response
        {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let handle = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\n\r\n{\"status\":\"ok\"}")
                    .unwrap();
            });
            let query = fetch_health_response(addr, connect_timeout, io_timeout);
            assert!(matches!(query, HealthQuery::Response(bytes) if bytes.contains(&b'{')));
            handle.join().unwrap();
        }

        // Non-success response
        {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let handle = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                stream
                    .write_all(b"HTTP/1.1 503 Service Unavailable\r\n\r\n")
                    .unwrap();
            });
            let query = fetch_health_response(addr, connect_timeout, io_timeout);
            assert!(
                matches!(query, HealthQuery::Response(bytes) if bytes.starts_with(b"HTTP/1.1 503"))
            );
            handle.join().unwrap();
        }

        // Connection refused -> Unavailable
        {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            drop(listener);
            let query =
                fetch_health_response(addr, Duration::from_millis(100), Duration::from_millis(100));
            assert_eq!(query, HealthQuery::Unavailable);
        }
    }
}
