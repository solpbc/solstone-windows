// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Structurally text-free redaction summaries.

use std::fmt;
use std::fmt::Write as _;
use std::io::{self, Read};

use sha2::{Digest, Sha256};

fn encode_hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Lowercase 64-character SHA-256 hex digest of an in-memory slice.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    encode_hex_lower(&digest)
}

/// Stream-hash any reader with bounded 64 KiB memory, returning `(lowercase_sha256_hex, bytes_read)`.
pub fn hash_reader_sha256<R: Read>(mut reader: R) -> io::Result<(String, u64)> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total = total
            .checked_add(n as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "file size overflow"))?;
    }
    Ok((encode_hex_lower(&hasher.finalize()), total))
}

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
