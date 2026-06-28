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
//! - [`wire`] — the serde request/response shapes for `/app/network/pair`,
//!   `/app/observer/register`, `/app/observer/ingest`, `/ingest/event`
//!   (heartbeat), and `/ingest/segments/<day>` (reconcile).
//! - [`multipart`] — the ingest multipart body, byte-identical to the macOS /
//!   Android / iOS uploaders (`files` field name; the server reads
//!   `request.files.getlist("files")`).
//! - [`ca`] — CA-fingerprint prefix pinning (SHA-256 of the cert DER, first 16
//!   bytes), the constant the transport's TLS verifier enforces.
//! - [`civil`] — epoch → `YYYYMMDD` / `HHMMSS` for the ingest `day` / `segment`
//!   keys, pure UTC arithmetic (no chrono, no tz database).
//!
//! There is no I/O and no platform dependency here, so the whole wire contract
//! is round-trip unit-tested on any host. The actual mTLS sockets live in the
//! platform-tier `pl-transport-win`.

#![forbid(unsafe_code)]

pub mod ca;
pub mod civil;
pub mod crockford;
pub mod frame;
pub mod http;
pub mod jwt;
pub mod multipart;
pub mod mux;
pub mod pairlink;
pub mod relay;
pub mod wire;

/// Default PL-direct mTLS port, used when a pair-link carries port 0.
pub const DEFAULT_DIRECT_PORT: u16 = 7657;

/// The observer protocol version this client speaks (sent as
/// `X-Solstone-Protocol-Version`). v2 is the `{items,total,protocol_version}`
/// reconcile envelope; v1 was a bare array.
pub const OBSERVER_PROTOCOL_VERSION: u32 = 2;

/// Auth header carrying the observer handle. Preferred over `Authorization`
/// because it survives proxy stripping; the journal checks it first
/// (`_get_auth_key`). Mirrors `OBSERVER_HANDLE_HEADER` in convey.
pub const OBSERVER_HANDLE_HEADER: &str = "X-Solstone-Observer";

/// Protocol-version header name.
pub const PROTOCOL_VERSION_HEADER: &str = "X-Solstone-Protocol-Version";

/// Observer endpoint paths (relative to the journal origin), reused verbatim
/// from the convey blueprint so the Windows client cannot drift.
pub mod paths {
    /// Mobile/observer pairing endpoint. Carries `?token=<nonce_hex>`.
    pub const PAIR: &str = "/app/network/pair";
    /// Self-register an observer after pairing.
    pub const REGISTER: &str = "/app/observer/register";
    /// Segment upload (multipart).
    pub const INGEST: &str = "/app/observer/ingest";
    /// Observer event relay; the heartbeat posts `observe.status` here.
    pub const INGEST_EVENT: &str = "/app/observer/ingest/event";
    /// Per-day segment list for reconciliation (append `/<YYYYMMDD>`).
    pub const INGEST_SEGMENTS: &str = "/app/observer/ingest/segments";
}
