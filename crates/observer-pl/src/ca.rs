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
use thiserror::Error;

const OID_EC_PUBLIC_KEY: &[u8] = &[0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
const OID_PRIME256V1: &[u8] = &[0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CaError {
    #[error("malformed certificate DER")]
    MalformedDer,
}

#[derive(Clone, Copy)]
struct DerElement {
    full_start: usize,
    content_start: usize,
    full_end: usize,
}

impl DerElement {
    fn full<'a>(&self, input: &'a [u8]) -> &'a [u8] {
        &input[self.full_start..self.full_end]
    }

    fn content<'a>(&self, input: &'a [u8]) -> &'a [u8] {
        &input[self.content_start..self.full_end]
    }
}

fn expect_element(input: &[u8], tag: u8) -> Result<DerElement, CaError> {
    let mut pos = 0;
    let elem = read_element(input, &mut pos, tag)?;
    if pos == input.len() {
        Ok(elem)
    } else {
        Err(CaError::MalformedDer)
    }
}

fn skip_any(input: &[u8], pos: &mut usize) -> Result<(), CaError> {
    read_element_any(input, pos).map(|_| ())
}

fn read_element(input: &[u8], pos: &mut usize, tag: u8) -> Result<DerElement, CaError> {
    let elem = read_element_any(input, pos)?;
    if input[elem.full_start] == tag {
        Ok(elem)
    } else {
        Err(CaError::MalformedDer)
    }
}

fn read_element_any(input: &[u8], pos: &mut usize) -> Result<DerElement, CaError> {
    let full_start = *pos;
    let tag = *input.get(*pos).ok_or(CaError::MalformedDer)?;
    *pos += 1;
    if tag & 0x1f == 0x1f {
        return Err(CaError::MalformedDer);
    }

    let first_len = *input.get(*pos).ok_or(CaError::MalformedDer)?;
    *pos += 1;
    let len = if first_len & 0x80 == 0 {
        first_len as usize
    } else {
        let count = (first_len & 0x7f) as usize;
        if count == 0 || count > 4 {
            return Err(CaError::MalformedDer);
        }
        let mut len = 0usize;
        for _ in 0..count {
            len = (len << 8) | (*input.get(*pos).ok_or(CaError::MalformedDer)? as usize);
            *pos += 1;
        }
        len
    };

    let content_start = *pos;
    let full_end = content_start
        .checked_add(len)
        .ok_or(CaError::MalformedDer)?;
    if full_end > input.len() {
        return Err(CaError::MalformedDer);
    }
    *pos = full_end;
    Ok(DerElement {
        full_start,
        content_start,
        full_end,
    })
}

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

/// Extract the complete SubjectPublicKeyInfo DER element from an X.509
/// certificate DER blob.
pub fn extract_spki_der(cert_der: &[u8]) -> Result<Vec<u8>, CaError> {
    let cert = expect_element(cert_der, 0x30)?;
    let mut cert_pos = cert.content_start;
    let tbs = read_element(cert_der, &mut cert_pos, 0x30)?;
    let mut tbs_pos = tbs.content_start;
    if cert_der.get(tbs_pos) == Some(&0xa0) {
        skip_any(cert_der, &mut tbs_pos)?;
    }
    read_element(cert_der, &mut tbs_pos, 0x02)?; // serialNumber
    read_element(cert_der, &mut tbs_pos, 0x30)?; // signature
    read_element(cert_der, &mut tbs_pos, 0x30)?; // issuer
    read_element(cert_der, &mut tbs_pos, 0x30)?; // validity
    read_element(cert_der, &mut tbs_pos, 0x30)?; // subject
    let spki = read_element(cert_der, &mut tbs_pos, 0x30)?;
    Ok(spki.full(cert_der).to_vec())
}

/// True if `spki_der` is a canonical EC P-256 SubjectPublicKeyInfo.
pub fn is_ec_p256_spki(spki_der: &[u8]) -> bool {
    let Ok(spki) = expect_element(spki_der, 0x30) else {
        return false;
    };
    let mut spki_pos = spki.content_start;
    let Ok(alg_id) = read_element(spki_der, &mut spki_pos, 0x30) else {
        return false;
    };
    let mut alg_pos = alg_id.content_start;
    let Ok(alg_oid) = read_element(spki_der, &mut alg_pos, 0x06) else {
        return false;
    };
    let Ok(curve_oid) = read_element(spki_der, &mut alg_pos, 0x06) else {
        return false;
    };
    alg_pos == alg_id.full_end
        && alg_oid.full(spki_der) == OID_EC_PUBLIC_KEY
        && curve_oid.full(spki_der) == OID_PRIME256V1
}

/// Extract the full TBS certificate DER element and certificate signature bytes
/// from an X.509 certificate DER blob.
pub fn extract_tbs_and_signature(cert_der: &[u8]) -> Result<(Vec<u8>, Vec<u8>), CaError> {
    let cert = expect_element(cert_der, 0x30)?;
    let mut pos = cert.content_start;
    let tbs = read_element(cert_der, &mut pos, 0x30)?;
    read_element(cert_der, &mut pos, 0x30)?; // signatureAlgorithm
    let signature = read_element(cert_der, &mut pos, 0x03)?;
    let signature_content = signature.content(cert_der);
    if signature_content.first() != Some(&0) {
        return Err(CaError::MalformedDer);
    }
    if pos != cert.full_end {
        return Err(CaError::MalformedDer);
    }
    Ok((tbs.full(cert_der).to_vec(), signature_content[1..].to_vec()))
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

/// True if `SHA-256(SPKI DER)` starts with `prefix`. Fails closed on malformed
/// certificates, empty prefixes, and overlong prefixes.
pub fn spki_matches_prefix(cert_der: &[u8], prefix: &[u8]) -> bool {
    if prefix.is_empty() || prefix.len() > 32 {
        return false;
    }
    let Ok(spki) = extract_spki_der(cert_der) else {
        return false;
    };
    sha256(&spki)[..prefix.len()] == *prefix
}

/// True if any cert in the presented chain has an SPKI matching `prefix`.
pub fn chain_spki_matches_prefix(chain: &[Vec<u8>], prefix: &[u8]) -> bool {
    chain.iter().any(|cert| spki_matches_prefix(cert, prefix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};

    fn test_cert_der_and_spki() -> (Vec<u8>, Vec<u8>) {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let params = CertificateParams::new(vec!["spl.local".to_string()]).unwrap();
        let cert = params.self_signed(&key).unwrap();
        (cert.der().to_vec(), key.public_key_der())
    }

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

    #[test]
    fn extracts_spki_from_real_certificate() {
        let (cert, expected_spki) = test_cert_der_and_spki();
        let spki = extract_spki_der(&cert).unwrap();
        assert_eq!(spki, expected_spki);
    }

    #[test]
    fn recognizes_ec_p256_spki() {
        let (_, spki) = test_cert_der_and_spki();
        assert!(is_ec_p256_spki(&spki));
    }

    #[test]
    fn ec_p256_spki_gate_fails_closed_on_junk() {
        assert!(!is_ec_p256_spki(b"not der"));
        assert!(!is_ec_p256_spki(&[0x30, 0x59]));
    }

    #[test]
    fn spki_matches_its_own_prefix() {
        let (cert, spki) = test_cert_der_and_spki();
        let prefix = sha256(&spki)[..16].to_vec();
        assert!(spki_matches_prefix(&cert, &prefix));
        assert!(chain_spki_matches_prefix(&[cert], &prefix));
    }

    #[test]
    fn spki_wrong_prefix_does_not_match() {
        let (cert, _) = test_cert_der_and_spki();
        assert!(!spki_matches_prefix(&cert, &[0x00; 16]));
    }

    #[test]
    fn spki_empty_or_overlong_prefix_fails_closed() {
        let (cert, _) = test_cert_der_and_spki();
        assert!(!spki_matches_prefix(&cert, &[]));
        assert!(!spki_matches_prefix(&cert, &[0u8; 33]));
    }

    #[test]
    fn direct_and_spki_pins_do_not_cross_match() {
        let (cert, spki) = test_cert_der_and_spki();
        let cert_der_prefix = sha256(&cert)[..16].to_vec();
        let spki_prefix = sha256(&spki)[..16].to_vec();
        assert!(!cert_matches_prefix(&cert, &spki_prefix));
        assert!(!spki_matches_prefix(&cert, &cert_der_prefix));
    }

    #[test]
    fn extracts_tbs_and_signature_from_real_certificate() {
        let (cert, _) = test_cert_der_and_spki();
        let (tbs, signature) = extract_tbs_and_signature(&cert).unwrap();
        assert_eq!(tbs.first(), Some(&0x30));
        assert!(!signature.is_empty());
    }
}
