// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Protocol-v3 ingest envelopes, custody proof, and civil-date helpers.
//!
//! Shared SPL framing, HTTP, relay, and loopback bridge authority lives in
//! `spl-core` and `spl-transport`. This crate retains only Windows product
//! protocol values that are not shared authority.

#![forbid(unsafe_code)]

pub mod civil;
pub mod ingest;

#[cfg(test)]
#[allow(dead_code)]
#[path = "test_compat/bridge.rs"]
mod bridge;
#[cfg(test)]
#[allow(dead_code)]
#[path = "test_compat/frame.rs"]
mod frame;
#[cfg(test)]
#[allow(dead_code)]
#[path = "test_compat/http.rs"]
mod http;
#[cfg(test)]
#[allow(dead_code)]
#[path = "test_compat/jwt.rs"]
mod jwt;
#[cfg(test)]
#[allow(dead_code)]
#[path = "test_compat/mux.rs"]
mod mux;
#[cfg(test)]
#[allow(dead_code)]
#[path = "test_compat/relay.rs"]
mod relay;
#[cfg(test)]
#[allow(dead_code)]
#[path = "test_compat/relay_access.rs"]
mod relay_access;

/// The observer protocol version this client speaks (sent as
/// `X-Solstone-Protocol-Version`).
pub const OBSERVER_PROTOCOL_VERSION: u32 = 3;

/// Reserved caller-auth header. The bridge filters this and `Authorization` so
/// local callers cannot override bridge-owned mTLS request identity.
pub const OBSERVER_HANDLE_HEADER: &str = "X-Solstone-Observer";

/// Capability-cookie name used by the Windows loopback bridge configuration.
pub const CAP_COOKIE_NAME: &str = "__solstone_journal_cap";

/// Prefix for journal cookies rewritten by the Windows loopback bridge.
pub const UPSTREAM_COOKIE_PREFIX: &str = "__solstone_journal_up_";

/// Protocol-version header name.
pub const PROTOCOL_VERSION_HEADER: &str = "X-Solstone-Protocol-Version";

/// Observer endpoint paths (relative to the journal origin).
pub mod paths {
    pub const SELF_SOURCES: &str = "/app/network/api/clients/self/sources";
    pub const KNOWN_SOURCES: &str = "/app/network/api/sources";
    /// Segment upload (multipart).
    pub const INGEST: &str = "/app/devices/ingest";
    /// Root ingest manifest used for protocol-v3 custody proof.
    pub const INGEST_MANIFEST: &str = "/app/devices/ingest/manifest";
    /// Per-day segment list for reconciliation (append `/<YYYYMMDD>`).
    pub const INGEST_SEGMENTS: &str = "/app/devices/ingest/segments";
    /// The vaults this journal has from other devices.
    pub const INGEST_OBSIDIAN_VAULTS: &str = "/app/devices/ingest/obsidian/vaults";
    /// Facets available for watched-folder assignment (protocol v3).
    pub const INGEST_FOLDER_FACETS: &str = "/app/devices/ingest/folder/facets";
    /// Journal-published supported file types for watched folders (protocol v3).
    pub const INGEST_FOLDER_SUPPORTED: &str = "/app/devices/ingest/folder/supported";
    /// Backfill work estimate for a watched folder preview (protocol v3).
    pub const INGEST_FOLDER_ESTIMATE: &str = "/app/devices/ingest/folder/estimate";
}
