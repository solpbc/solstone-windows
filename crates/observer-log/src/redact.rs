// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Structurally text-free redaction summaries.

use std::fmt;
use std::fmt::Write as _;

use sha2::{Digest, Sha256};

/// Redacted representation of a sensitive string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedSecret {
    kind: &'static str,
    len: usize,
    sha256_prefix: String,
}

impl fmt::Display for RedactedSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "kind={} len={} sha256={}",
            self.kind, self.len, self.sha256_prefix
        )
    }
}

/// Summarize a sensitive string without retaining it.
pub fn redact_secret(kind: &'static str, raw: &str) -> RedactedSecret {
    let digest = Sha256::digest(raw.as_bytes());
    let mut sha256_prefix = String::with_capacity(12);
    for byte in &digest[..6] {
        let _ = write!(&mut sha256_prefix, "{byte:02x}");
    }
    RedactedSecret {
        kind,
        len: raw.len(),
        sha256_prefix,
    }
}

/// Pair-link-specific redaction summary.
pub fn redact_pair_link(link: &str) -> RedactedSecret {
    redact_secret("pair-link", link)
}

/// Title-list summary that cannot hold owner title text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TitleSummary {
    count: usize,
}

impl fmt::Display for TitleSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "titles={}", self.count)
    }
}

/// Summarize title count without retaining title text.
pub fn redact_titles<S: AsRef<str>>(titles: &[S]) -> TitleSummary {
    TitleSummary {
        count: titles.len(),
    }
}
