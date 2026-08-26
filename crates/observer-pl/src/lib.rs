// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The pure observer PL wire protocol.
//!
//! This is the **pure tier** Wave-2 crate: the faithful Rust port of the
//! observer wire contract that iOS (`solstone-swift`) and Android
//! (`solstone-android`) already ship, and that the journal (`solstone` convey)
//! serves. It owns:
//!
//! - [`pairlink`] — parse the `https://go.solstone.app/p#…` QR pair-link
//!   (Crockford base32, v04 single-address + v05 multi-address).
//! - [`frame`] / [`mux`] — the spl multiplex framing (8-byte header, OPEN/DATA/
//!   CLOSE/PING/PONG) and the dialer-side request/response assembler.
//! - [`http`] — HTTP/1.1 request build + response parse, exactly as the Android
//!   `PlHttp` transport frames it (`host: spl.local`, framing-owned headers).
//! - [`wire`] — the pairing request/response shapes.
//! - [`ingest`] — the protocol-v3 ingest envelope, responses, and pure custody
//!   proof.
//! - [`ca`] — CA-fingerprint prefix pinning (SHA-256 of the cert DER, first 16
//!   bytes), the constant the transport's TLS verifier enforces.
//! - [`relay_window`] — relay pair-window RK and journal identity derivations.
//! - [`civil`] — epoch → `YYYYMMDD` / `HHMMSS` for the ingest `day` / `segment`
//!   keys, pure UTC arithmetic (no chrono, no tz database).
//!
//! There is no I/O and no platform dependency here, so the whole wire contract
//! is round-trip unit-tested on any host. The actual mTLS sockets live in the
//! platform-tier `pl-transport-win`.

#![forbid(unsafe_code)]

pub mod bridge;
pub mod ca;
pub mod civil;
pub mod crockford;
pub mod frame;
pub mod http;
pub mod ingest;
pub mod jwt;
pub mod mux;
pub mod pairlink;
pub mod relay;
pub mod relay_window;
pub mod wire;

/// Default PL-direct mTLS port, used when a pair-link carries port 0.
pub const DEFAULT_DIRECT_PORT: u16 = 7657;

/// The observer protocol version this client speaks (sent as
/// `X-Solstone-Protocol-Version`).
pub const OBSERVER_PROTOCOL_VERSION: u32 = 3;

/// Reserved caller-auth header. The bridge filters this and `Authorization` so
/// local callers cannot override bridge-owned mTLS request identity.
pub const OBSERVER_HANDLE_HEADER: &str = "X-Solstone-Observer";

/// Protocol-version header name.
pub const PROTOCOL_VERSION_HEADER: &str = "X-Solstone-Protocol-Version";

/// Observer endpoint paths (relative to the journal origin). PAIR still matches
/// the convey blueprint; the journal ingest endpoints use `/app/devices`.
pub mod paths {
    /// Mobile/observer pairing endpoint. Carries `?token=<pair-token-hex>`.
    pub const PAIR: &str = "/app/network/pair";
    /// Segment upload (multipart).
    pub const INGEST: &str = "/app/devices/ingest";
    /// Root ingest manifest used for protocol-v3 custody proof.
    pub const INGEST_MANIFEST: &str = "/app/devices/ingest/manifest";
    /// Per-day segment list for reconciliation (append `/<YYYYMMDD>`).
    pub const INGEST_SEGMENTS: &str = "/app/devices/ingest/segments";
}
