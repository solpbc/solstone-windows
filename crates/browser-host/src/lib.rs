// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The solstone browser extension's native host, Windows side.
//!
//! The extension talks only to its native host over the browser's stdio
//! framing. On Windows the host is this app's own executable started in host
//! mode ([`argv`]); it is a thin relay ([`relay`]) that forwards frames over a
//! same-user named pipe to the running app. The app owns everything else: the
//! session protocol and live capture gate ([`hub`]), durable acceptance in the
//! pending store ([`custody`]), and delivery of finalized periods to the
//! journal as the `browser` source ([`upload`]).
//!
//! The wire contract is the shared `native-browser-frame` crate, vendored from
//! `solstone-browser`. Nothing here retypes it: frames, decode, encode and the
//! registration table come from there.

#![forbid(unsafe_code)]

pub mod argv;
pub mod custody;
pub mod hub;
pub mod identity;
pub mod registration;
pub mod relay;
pub mod upload;
pub mod wire;

pub use native_browser_frame as frame;

/// The journal source every browser period is uploaded under.
pub const SOURCE: &str = "browser";
/// The one file a browser period carries.
pub const PAGES_FILE: &str = "browser_pages.jsonl";
/// Its explicit MIME, matching the macOS app.
pub const PAGES_CONTENT_TYPE: &str = "application/jsonl";
