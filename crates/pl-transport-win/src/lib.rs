// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Framed-mTLS PL transport + observer client for the Windows observer.
//!
//! This is the Wave-2 network layer. It drives the pure wire from `observer-pl`
//! over a real TLS socket and implements the observer half of the protocol the
//! journal serves and that iOS/Android already ship:
//!
//! 1. **Pair** ([`pairing`]) — certless TLS to the journal with CA-fp pinning,
//!    POST a freshly-minted CSR to `/app/network/pair`, store the signed client
//!    credential.
//! 2. **Register** ([`client`]) — over mTLS, `/app/observer/register`, learn the
//!    observer handle.
//! 3. **Upload** ([`coordinator`]) — ship sealed segments to
//!    `/app/observer/ingest`, reconcile by sha256, retry with backoff.
//! 4. **Heartbeat** ([`heartbeat`]) — periodic `observe.status` POST so the
//!    journal sees the observer as live.
//!
//! The whole crate is built on rustls (ring) + std sockets, so it compiles and
//! tests on the Linux dev host too — the live cross-repo pair+ingest gate can
//! run against a journal on the dev box, not only on Windows. Pairing/upload
//! state is published into a shared [`observer_model::SyncSnapshot`] the engine
//! folds into the health dump.

#![forbid(unsafe_code)]

pub mod client;
pub mod connection;
pub mod coordinator;
pub mod credential;
pub mod heartbeat;
pub mod journal_bridge;
pub mod pairing;
pub mod relay;
pub(crate) mod relay_http;
pub mod relay_pairing;
pub mod relay_token;
pub mod sealed;
pub mod service;
pub(crate) mod spki_pin;
pub mod tls;

use std::fmt;

use observer_pl::http::HttpError;
use observer_pl::mux::MuxError;
use thiserror::Error;

/// Default upload poll interval when there is nothing to do.
pub const DEFAULT_UPLOAD_INTERVAL_SECS: u64 = 5;

/// Heartbeat cadence — matches the macOS `HeartbeatService` (15s).
pub const HEARTBEAT_INTERVAL_SECS: u64 = 15;

/// Typed relay upgrade/close outcomes. Retryability noted per-variant (doc only;
/// W3 owns the retry policy).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayError {
    /// Upgrade HTTP 503; retryable.
    HomeOffline,
    /// Upgrade HTTP 401 or close 4401; not retryable without W2 token refresh.
    Unauthorized,
    /// Upgrade HTTP 402 or close 4402; not retryable.
    Unpaid,
    /// Upgrade HTTP 404; not retryable.
    UnknownInstance,
    /// Pair-dial HTTP 401; the journal pairing window is closed or expired.
    PairWindowClosed,
    /// Close 1009; retryable.
    Overflow,
    /// Close 1006/1012 or abnormal drop; retryable by reconnecting, not re-pairing.
    Abnormal,
    /// Any other unexpected upgrade HTTP status; not retryable.
    UpgradeRejected,
    /// AC6 inner-handshake/first-byte timeout; retryable.
    Stalled,
}

impl fmt::Display for RelayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            RelayError::HomeOffline => "home offline",
            RelayError::Unauthorized => "unauthorized",
            RelayError::Unpaid => "unpaid",
            RelayError::UnknownInstance => "unknown instance",
            RelayError::PairWindowClosed => {
                "the pairing window is closed or expired — regenerate the link on your journal"
            }
            RelayError::Overflow => "overflow",
            RelayError::Abnormal => "abnormal close",
            RelayError::UpgradeRejected => "upgrade rejected",
            RelayError::Stalled => "stalled",
        };
        f.write_str(message)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayControlEndpoint {
    EnrollDevice,
    TokenRefresh,
}

impl RelayControlEndpoint {
    fn code(self) -> &'static str {
        match self {
            RelayControlEndpoint::EnrollDevice => "enroll_device",
            RelayControlEndpoint::TokenRefresh => "refresh",
        }
    }
}

impl fmt::Display for RelayControlEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

/// Errors from the transport / observer client.
#[derive(Debug, Error)]
pub enum TransportError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("tls error: {0}")]
    Tls(String),
    #[error("crypto error: {0}")]
    Crypto(String),
    #[error("mux error: {0}")]
    Mux(#[from] MuxError),
    #[error("http error: {0}")]
    Http(#[from] HttpError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("pair-link error: {0}")]
    PairLink(String),
    #[error("pairing failed: {0}")]
    Pairing(String),
    #[error("server rejected request: HTTP {status} {body}")]
    Rejected { status: u16, body: String },
    #[error("relay error: {0}")]
    Relay(RelayError),
    #[error("relay control {endpoint} rejected request: HTTP {status}")]
    RelayControlRejected {
        endpoint: RelayControlEndpoint,
        status: u16,
    },
    #[error("no reachable journal endpoint")]
    NoEndpoint,
    #[error("not paired")]
    NotPaired,
}

pub fn transport_error_code(err: &TransportError) -> String {
    match err {
        TransportError::Io(_) => "io".to_string(),
        TransportError::Tls(_) => "tls".to_string(),
        TransportError::Crypto(_) => "crypto".to_string(),
        TransportError::Mux(_) => "mux".to_string(),
        TransportError::Http(_) => "http".to_string(),
        TransportError::Json(_) => "json".to_string(),
        TransportError::PairLink(_) => "pair_link".to_string(),
        TransportError::Pairing(_) => "pairing".to_string(),
        TransportError::Rejected { status, body: _ } => format!("http_{status}"),
        TransportError::Relay(r) => match r {
            RelayError::HomeOffline => "relay_home_offline",
            RelayError::Unauthorized => "relay_unauthorized",
            RelayError::Unpaid => "relay_unpaid",
            RelayError::UnknownInstance => "relay_unknown_instance",
            RelayError::PairWindowClosed => "relay_pair_window_closed",
            RelayError::Overflow => "relay_overflow",
            RelayError::Abnormal => "relay_abnormal",
            RelayError::UpgradeRejected => "relay_upgrade_rejected",
            RelayError::Stalled => "relay_stalled",
        }
        .to_string(),
        TransportError::RelayControlRejected { endpoint, status } => {
            format!("relay_{}_http_{status}", endpoint.code())
        }
        TransportError::NoEndpoint => "no_endpoint".to_string(),
        TransportError::NotPaired => "not_paired".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_error_code_maps_every_variant_without_inner_detail() {
        let json_error = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        let cases = [
            (
                TransportError::Io(std::io::Error::other("C:\\Users\\me\\seg.mp4")),
                "io",
            ),
            (TransportError::Tls("10.0.0.5:7657".into()), "tls"),
            (TransportError::Crypto("fingerprint abc".into()), "crypto"),
            (TransportError::Mux(MuxError::Incomplete), "mux"),
            (
                TransportError::Http(HttpError::BadStatusLine("HTTP/1.1 SECRET".into())),
                "http",
            ),
            (TransportError::Json(json_error), "json"),
            (TransportError::PairLink("token=abc".into()), "pair_link"),
            (TransportError::Pairing("sha256:abc".into()), "pairing"),
            (
                TransportError::Rejected {
                    status: 503,
                    body: "SECRET https://x/y?token=abc C:\\Users\\me\\seg.mp4".into(),
                },
                "http_503",
            ),
            (
                TransportError::Relay(RelayError::HomeOffline),
                "relay_home_offline",
            ),
            (
                TransportError::Relay(RelayError::Unauthorized),
                "relay_unauthorized",
            ),
            (TransportError::Relay(RelayError::Unpaid), "relay_unpaid"),
            (
                TransportError::Relay(RelayError::UnknownInstance),
                "relay_unknown_instance",
            ),
            (
                TransportError::Relay(RelayError::PairWindowClosed),
                "relay_pair_window_closed",
            ),
            (
                TransportError::Relay(RelayError::Overflow),
                "relay_overflow",
            ),
            (
                TransportError::Relay(RelayError::Abnormal),
                "relay_abnormal",
            ),
            (
                TransportError::Relay(RelayError::UpgradeRejected),
                "relay_upgrade_rejected",
            ),
            (TransportError::Relay(RelayError::Stalled), "relay_stalled"),
            (
                TransportError::RelayControlRejected {
                    endpoint: RelayControlEndpoint::EnrollDevice,
                    status: 409,
                },
                "relay_enroll_device_http_409",
            ),
            (
                TransportError::RelayControlRejected {
                    endpoint: RelayControlEndpoint::TokenRefresh,
                    status: 404,
                },
                "relay_refresh_http_404",
            ),
            (TransportError::NoEndpoint, "no_endpoint"),
            (TransportError::NotPaired, "not_paired"),
        ];

        for (error, expected) in cases {
            let code = transport_error_code(&error);
            assert_eq!(code, expected);
            assert!(!code.contains("SECRET"));
            assert!(!code.contains("token"));
            assert!(!code.contains("Users"));
            assert!(!code.contains("https://"));
            assert!(!code.contains("sha256:"));
            assert!(!code.contains("10.0.0.5"));
        }
    }
}
