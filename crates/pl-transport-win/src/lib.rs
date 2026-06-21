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
pub mod pairing;
pub mod sealed;
pub mod service;
pub mod tls;

use observer_pl::http::HttpError;
use observer_pl::mux::MuxError;
use thiserror::Error;

/// Default upload poll interval when there is nothing to do.
pub const DEFAULT_UPLOAD_INTERVAL_SECS: u64 = 5;

/// Heartbeat cadence — matches the macOS `HeartbeatService` (15s).
pub const HEARTBEAT_INTERVAL_SECS: u64 = 15;

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
    #[error("no reachable journal endpoint")]
    NoEndpoint,
    #[error("not paired")]
    NotPaired,
}
