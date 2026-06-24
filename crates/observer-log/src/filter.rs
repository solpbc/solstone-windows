// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! RUST_LOG-style filter resolution.

use tracing_subscriber::EnvFilter;

/// Resolve an EnvFilter from RUST_LOG text, defaulting to info on absence/error.
pub fn resolve_filter(rust_log: Option<&str>) -> EnvFilter {
    match rust_log {
        Some(value) if !value.trim().is_empty() => {
            EnvFilter::try_new(value).unwrap_or_else(|_| EnvFilter::new("info"))
        }
        _ => EnvFilter::new("info"),
    }
}
