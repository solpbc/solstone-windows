// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Closed frontend error payloads.

use serde::{Deserialize, Serialize};

/// Structurally minimal frontend error record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct FrontendErrorRecord {
    pub kind: FrontendErrorKind,
    pub level: FrontendLevel,
    pub origin: FrontendOrigin,
    pub line: u32,
    pub column: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontendErrorKind {
    Error,
    UnhandledRejection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontendLevel {
    Error,
    Warn,
    Info,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontendOrigin {
    Settings,
    About,
    None,
}
