// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The host-mode relay: the browser's stdio on one side, the app's local pipe
//! on the other.
//!
//! The relay holds no state and interprets nothing but the shape of a frame:
//! it bounds-checks lengths, forwards extension frames to the app, and forwards
//! the app's replies to the browser. It stops on stdin EOF, on the app going
//! away, and after relaying a `bye` or `unsupported`. Nothing but frames is
//! ever written to stdout, and no page text reaches stderr or a log.

use std::time::Duration;

use native_browser_frame::constants::{CONTROL_MAX, HANDSHAKE_MS_BUDGET};
use native_browser_frame::Direction;
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::argv::Invocation;
use crate::wire::{write_frame, FrameReader};

/// Why the relay ended (logged by code only, for the process exit status).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayEnd {
    /// No app endpoint: the browser was told capture is unavailable.
    AppAbsent,
    /// The browser closed stdin.
    BrowserClosed,
    /// The app closed the pipe.
    AppClosed,
    /// The app said `bye` or `unsupported`, and it was relayed.
    Finished,
    /// No extension frame arrived within the handshake budget.
    HandshakeTimeout,
    /// A frame or I/O fault on either leg.
    Fault,
}

/// The `hello_ack` sent when the app is not running: the gate stays closed.
pub fn unavailable_ack() -> Value {
    json!({
        "type": "hello_ack",
        "capture": "unavailable",
        "delivery": "unknown",
        "freshness_ms": 0,
        "destination_generation": null,
        "period_id": null,
        "custody": {"full": false, "stale": false},
        "version": native_browser_frame::BUNDLE_VERSION,
    })
}

/// Run the relay. `pipe` is `None` when the app's endpoint does not exist.
pub async fn run<I, O, P>(
    invocation: Invocation,
    stdin: I,
    mut stdout: O,
    pipe: Option<P>,
) -> RelayEnd
where
    I: AsyncRead + Unpin,
    O: AsyncWrite + Unpin,
    P: AsyncRead + AsyncWrite + Unpin,
{
    let Some(pipe) = pipe else {
        let Ok(bytes) = native_browser_frame::encode(&unavailable_ack()) else {
            return RelayEnd::Fault;
        };
        let _ = write_frame(&mut stdout, Direction::HostToExtension, &bytes).await;
        return RelayEnd::AppAbsent;
    };
    let (pipe_r, mut pipe_w) = tokio::io::split(pipe);
    let local = json!({
        "type": "local_hello",
        "brand": invocation.brand.token(),
        "mode": invocation.mode.token(),
    });
    let Ok(local) = serde_json::to_vec(&local) else {
        return RelayEnd::Fault;
    };
    if write_frame(&mut pipe_w, Direction::ExtensionToHost, &local)
        .await
        .is_err()
    {
        return RelayEnd::AppClosed;
    }

    let mut from_browser = FrameReader::new(stdin, Direction::ExtensionToHost);
    let mut from_app = FrameReader::new(pipe_r, Direction::HostToExtension);

    // The first extension frame is a small control frame, and it must come
    // within the handshake budget.
    let first = match tokio::time::timeout(
        Duration::from_millis(HANDSHAKE_MS_BUDGET),
        from_browser.next(),
    )
    .await
    {
        Err(_) => return RelayEnd::HandshakeTimeout,
        Ok(Ok(None)) => return RelayEnd::BrowserClosed,
        Ok(Err(_)) => return RelayEnd::Fault,
        Ok(Ok(Some(frame))) => frame,
    };
    if first.len() > CONTROL_MAX {
        return RelayEnd::Fault;
    }
    if write_frame(&mut pipe_w, Direction::ExtensionToHost, &first)
        .await
        .is_err()
    {
        return RelayEnd::AppClosed;
    }

    loop {
        tokio::select! {
            frame = from_browser.next() => match frame {
                Ok(Some(frame)) => {
                    if write_frame(&mut pipe_w, Direction::ExtensionToHost, &frame).await.is_err() {
                        return RelayEnd::AppClosed;
                    }
                }
                Ok(None) => return RelayEnd::BrowserClosed,
                Err(_) => return RelayEnd::Fault,
            },
            frame = from_app.next() => match frame {
                Ok(Some(frame)) => {
                    if write_frame(&mut stdout, Direction::HostToExtension, &frame).await.is_err() {
                        return RelayEnd::BrowserClosed;
                    }
                    if ends_session(&frame) {
                        return RelayEnd::Finished;
                    }
                }
                Ok(None) => return RelayEnd::AppClosed,
                Err(_) => return RelayEnd::Fault,
            },
        }
    }
}

fn ends_session(frame: &[u8]) -> bool {
    serde_json::from_slice::<Value>(frame)
        .ok()
        .and_then(|v| v.get("type").and_then(Value::as_str).map(str::to_string))
        .is_some_and(|t| t == "bye" || t == "unsupported")
}

/// Ask the running app to quiesce for an update, over its own pipe. Returns
/// whether every connected host left within the app's wait.
pub async fn request_quiesce<P>(pipe: P) -> Option<bool>
where
    P: AsyncRead + AsyncWrite + Unpin,
{
    let (r, mut w) = tokio::io::split(pipe);
    let ask = serde_json::to_vec(&json!({"type": "local_quiesce"})).ok()?;
    write_frame(&mut w, Direction::ExtensionToHost, &ask)
        .await
        .ok()?;
    let mut reader = FrameReader::new(r, Direction::HostToExtension);
    let wait = crate::hub::QUIESCE_WAIT + Duration::from_secs(5);
    let frame = tokio::time::timeout(wait, reader.next())
        .await
        .ok()?
        .ok()??;
    let reply: Value = serde_json::from_slice(&frame).ok()?;
    (reply["type"] == "local_quiesced").then(|| reply["closed"].as_bool().unwrap_or(false))
}
