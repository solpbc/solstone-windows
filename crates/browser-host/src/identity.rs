// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The strict journal identity a custody generation is bound to.
//!
//! One generation per journal: the exact instance id plus the normalized CA
//! chain (each certificate's SHA-256, in chain order). Instance id alone is not
//! enough: a journal replaced under the same id with a new CA is a different
//! destination. A same-journal re-pair mints a new client certificate but keeps
//! both, so it keeps the generation.

use base64::Engine as _;
use sha2::{Digest, Sha256};

const DOMAIN: &str = "solstone-browser-custody-identity-v1";

/// The identity digest for `instance_id` and the CA chain's DER certificates.
pub fn journal_identity(instance_id: &str, ca_chain_der: &[Vec<u8>]) -> String {
    let mut h = Sha256::new();
    h.update(DOMAIN.as_bytes());
    h.update([0u8]);
    h.update(instance_id.as_bytes());
    h.update([0u8]);
    for (i, der) in ca_chain_der.iter().enumerate() {
        if i > 0 {
            h.update([0x1fu8]);
        }
        h.update(hex(&Sha256::digest(der)).as_bytes());
    }
    format!("sha256:{}", hex(&h.finalize()))
}

/// Identity from the credential's PEM chain. `None` when a PEM block does not
/// decode: an unreadable chain never matches anything.
pub fn journal_identity_from_pem(instance_id: &str, ca_chain_pem: &[String]) -> Option<String> {
    let mut ders = Vec::new();
    for pem in ca_chain_pem {
        ders.extend(pem_certificates(pem)?);
    }
    if ders.is_empty() || instance_id.is_empty() {
        return None;
    }
    Some(journal_identity(instance_id, &ders))
}

/// Every `CERTIFICATE` block in `pem`, in order.
fn pem_certificates(pem: &str) -> Option<Vec<Vec<u8>>> {
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";
    let mut out = Vec::new();
    let mut rest = pem;
    while let Some(start) = rest.find(BEGIN) {
        let after = &rest[start + BEGIN.len()..];
        let end = after.find(END)?;
        let body: String = after[..end]
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        out.push(
            base64::engine::general_purpose::STANDARD
                .decode(body)
                .ok()?,
        );
        rest = &after[end + END.len()..];
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[(b >> 4) as usize] as char);
        s.push(DIGITS[(b & 0xf) as usize] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pem(der: &[u8]) -> String {
        format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            base64::engine::general_purpose::STANDARD.encode(der)
        )
    }

    #[test]
    fn same_instance_and_chain_is_the_same_identity() {
        let a = journal_identity_from_pem("inst-1", &[pem(b"root"), pem(b"inter")]).unwrap();
        let b = journal_identity_from_pem("inst-1", &[pem(b"root"), pem(b"inter")]).unwrap();
        assert_eq!(a, b);
        assert!(a.starts_with("sha256:"));
    }

    #[test]
    fn a_changed_ca_or_instance_is_a_different_identity() {
        let base = journal_identity_from_pem("inst-1", &[pem(b"root")]).unwrap();
        assert_ne!(
            base,
            journal_identity_from_pem("inst-1", &[pem(b"root2")]).unwrap()
        );
        assert_ne!(
            base,
            journal_identity_from_pem("inst-2", &[pem(b"root")]).unwrap()
        );
        assert_ne!(
            base,
            journal_identity_from_pem("inst-1", &[pem(b"root"), pem(b"x")]).unwrap()
        );
    }

    #[test]
    fn a_bundle_in_one_pem_string_equals_the_split_chain() {
        let joined = format!("{}{}", pem(b"root"), pem(b"inter"));
        assert_eq!(
            journal_identity_from_pem("i", &[joined]),
            journal_identity_from_pem("i", &[pem(b"root"), pem(b"inter")])
        );
    }

    #[test]
    fn unreadable_or_empty_chains_have_no_identity() {
        assert_eq!(journal_identity_from_pem("i", &[]), None);
        assert_eq!(journal_identity_from_pem("i", &["garbage".into()]), None);
        assert_eq!(journal_identity_from_pem("", &[pem(b"root")]), None);
    }
}
