// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! CA-fingerprint pinning logic.
//!
//! The pair-link carries the first 16 bytes of `SHA-256(CA cert DER)`. At the
//! TLS handshake the client pins it: a presented certificate chain is trusted
//! only if some cert in it has a SHA-256 whose leading bytes equal the pin
//! (`chain_matches_prefix`). This is the same prefix model the Android
//! `chainMatchesPrefix` / iOS `pinMatches` use and the journal's `ca_pin_matches`
//! verifies. The signature-of-the-handshake check (that the peer actually holds
//! the leaf key) is enforced separately by the TLS layer in `pl-transport-win`;
//! pinning the chain plus verifying the leaf signature together defeat a relay
//! that echoes the real CA chain but terminates TLS with its own key.

use sha2::{Digest, Sha256};

/// SHA-256 of `bytes` as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Raw SHA-256 digest of `bytes`.
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// True if `SHA-256(cert_der)` starts with `prefix`. A non-empty prefix longer
/// than the digest can never match (fail-closed).
pub fn cert_matches_prefix(cert_der: &[u8], prefix: &[u8]) -> bool {
    if prefix.is_empty() || prefix.len() > 32 {
        return false;
    }
    sha256(cert_der)[..prefix.len()] == *prefix
}

/// True if any cert in the presented chain matches the pinned `prefix`.
pub fn chain_matches_prefix(chain: &[Vec<u8>], prefix: &[u8]) -> bool {
    chain.iter().any(|cert| cert_matches_prefix(cert, prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_is_known_vector() {
        // SHA-256("") well-known digest.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn cert_matches_its_own_prefix() {
        let cert = b"fake-cert-der-bytes";
        let full = sha256(cert);
        assert!(cert_matches_prefix(cert, &full[..16]));
        assert!(cert_matches_prefix(cert, &full)); // full digest matches too
    }

    #[test]
    fn wrong_prefix_does_not_match() {
        let cert = b"fake-cert-der-bytes";
        assert!(!cert_matches_prefix(cert, &[0x00; 16]));
    }

    #[test]
    fn empty_or_overlong_prefix_fails_closed() {
        let cert = b"x";
        assert!(!cert_matches_prefix(cert, &[]));
        assert!(!cert_matches_prefix(cert, &[0u8; 33]));
    }

    #[test]
    fn chain_match_finds_ca_among_leaves() {
        let leaf = b"leaf".to_vec();
        let ca = b"ca-cert".to_vec();
        let prefix = sha256(&ca)[..16].to_vec();
        let chain = vec![leaf, ca];
        assert!(chain_matches_prefix(&chain, &prefix));
    }
}
