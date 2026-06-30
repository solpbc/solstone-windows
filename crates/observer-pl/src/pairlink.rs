// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Pair-link parsing.
//!
//! A journal QR / pasted pair-link is `https://go.solstone.app/p#<fragment>`,
//! where `<fragment>` is Crockford base32 over a small binary blob. We parse the
//! LAN-direct shapes and the relay form the journal emits:
//!
//! - **v06** (relay pair-window): `0x06 S(8) ca_fp_tag(1)=0x01
//!   ca_fp_spki(16) relay_origin_selector(1) origin?` = 27 B base
//! - **v04** (single IPv4): `0x04 0x01 ip(4) port(2,BE) nonce(16) ca_fp(16)` = 40 B
//! - **v05** (multi IPv4, current): `0x05 0x01 count port(2,BE) ip(4)*count
//!   nonce(16) ca_fp(16)` = 37 + 4*count B
//!
//! Byte layout verified against the journal builder (`solstone/apps/link`) and
//! the iOS `PairURL` / Android `PairLink` parsers. Port 0 means
//! [`DEFAULT_DIRECT_PORT`](crate::DEFAULT_DIRECT_PORT). Loopback candidates are
//! dropped (a remote observer can never reach the journal's own 127/8). The CA
//! fingerprint is the 16-byte SHA-256-of-CA-cert-DER prefix the TLS layer pins.

use thiserror::Error;

use crate::crockford::{self, CrockfordError};
use crate::DEFAULT_DIRECT_PORT;

/// Default relay origin selected by relay-form selector `0x00`.
pub const DEFAULT_RELAY_ORIGIN: &str = "https://link.solstone.app";

/// One dialable journal address from the pair-link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
}

/// A parsed LAN-direct pair-link: where to reach the journal, the one-shot
/// pairing nonce, and the CA-fingerprint prefix to pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairLink {
    /// Candidate journal endpoints, in pair-link order (loopback filtered out).
    pub candidates: Vec<Endpoint>,
    /// The pairing nonce, lowercase hex (32 chars for the 16 raw bytes).
    pub nonce_hex: String,
    /// SHA-256(CA cert DER) prefix — 16 bytes — pinned at the TLS handshake.
    pub ca_fp_prefix: Vec<u8>,
}

/// Parsed pair-link variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedPairLink {
    Direct(PairLink),
    Relay(RelayPairLink),
}

/// A parsed relay pair-window link: the 8-byte relay secret, the relay target,
/// and the SPKI fingerprint prefix used later for live-peer binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayPairLink {
    pub s: [u8; 8],
    pub ca_fp_spki: Vec<u8>,
    pub relay_origin: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PairLinkError {
    #[error("pair-link missing the '#<fragment>' part")]
    MissingFragment,
    #[error("pair-link fragment is not valid crockford base32: {0}")]
    Crockford(#[from] CrockfordError),
    #[error("unsupported pair-link version byte: {0:#x}")]
    UnsupportedVersion(u8),
    #[error("unsupported pair-link address type: {0:#x}")]
    UnsupportedAddressType(u8),
    #[error("unsupported relay CA-fingerprint tag: {0:#x}")]
    UnknownCaFpTag(u8),
    #[error("relay origin is not valid UTF-8")]
    BadRelayOrigin,
    #[error("pair-link blob truncated (expected {expected} bytes, got {got})")]
    Truncated { expected: usize, got: usize },
    #[error("pair-link blob length mismatch (expected {expected} bytes, got {got})")]
    LengthMismatch { expected: usize, got: usize },
    #[error("pair-link carried no reachable (non-loopback) candidate")]
    NoReachableCandidate,
}

const ADDR_TYPE_IPV4: u8 = 0x01;
const NONCE_LEN: usize = 16;
const CA_FP_LEN: usize = 16;
const RELAY_CA_FP_TAG_SPKI_SHA256: u8 = 0x01;
const RELAY_WINDOW_BASE_LEN: usize = 27;

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn ipv4_string(octets: &[u8]) -> String {
    format!("{}.{}.{}.{}", octets[0], octets[1], octets[2], octets[3])
}

pub(crate) fn uuid_string(raw: &[u8]) -> String {
    let h = hex_lower(raw);
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

fn is_loopback(octets: &[u8]) -> bool {
    octets[0] == 127
}

fn normalize_port(raw: u16) -> u16 {
    if raw == 0 {
        DEFAULT_DIRECT_PORT
    } else {
        raw
    }
}

/// Parse a full pair-link URL (or a bare fragment).
pub fn parse(link: &str) -> Result<ParsedPairLink, PairLinkError> {
    let fragment = match link.split_once('#') {
        Some((_, frag)) => frag,
        // Allow callers to pass a bare fragment too.
        None if !link.contains("://") && !link.contains('/') => link,
        None => return Err(PairLinkError::MissingFragment),
    };
    let blob = crockford::decode(fragment)?;
    parse_blob(&blob)
}

/// Parse the decoded binary blob.
pub fn parse_blob(blob: &[u8]) -> Result<ParsedPairLink, PairLinkError> {
    let version = *blob.first().ok_or(PairLinkError::Truncated {
        expected: 1,
        got: 0,
    })?;
    match version {
        0x04 => parse_v04(blob).map(ParsedPairLink::Direct),
        0x05 => parse_v05(blob).map(ParsedPairLink::Direct),
        0x06 => parse_v06(blob).map(ParsedPairLink::Relay),
        other => Err(PairLinkError::UnsupportedVersion(other)),
    }
}

fn require(blob: &[u8], end: usize) -> Result<(), PairLinkError> {
    if blob.len() < end {
        Err(PairLinkError::Truncated {
            expected: end,
            got: blob.len(),
        })
    } else {
        Ok(())
    }
}

fn require_exact(blob: &[u8], expected: usize) -> Result<(), PairLinkError> {
    require(blob, expected)?;
    if blob.len() != expected {
        Err(PairLinkError::LengthMismatch {
            expected,
            got: blob.len(),
        })
    } else {
        Ok(())
    }
}

fn parse_v06(blob: &[u8]) -> Result<RelayPairLink, PairLinkError> {
    require(blob, RELAY_WINDOW_BASE_LEN)?;
    let ca_fp_tag = blob[9];
    if ca_fp_tag != RELAY_CA_FP_TAG_SPKI_SHA256 {
        return Err(PairLinkError::UnknownCaFpTag(ca_fp_tag));
    }

    let selector = blob[26] as usize;
    let relay_origin = if selector == 0 {
        require_exact(blob, RELAY_WINDOW_BASE_LEN)?;
        DEFAULT_RELAY_ORIGIN.to_string()
    } else {
        let expected = RELAY_WINDOW_BASE_LEN + selector;
        require_exact(blob, expected)?;
        std::str::from_utf8(&blob[RELAY_WINDOW_BASE_LEN..expected])
            .map_err(|_| PairLinkError::BadRelayOrigin)?
            .to_string()
    };

    Ok(RelayPairLink {
        s: blob[1..9].try_into().expect("slice length is fixed"),
        ca_fp_spki: blob[10..26].to_vec(),
        relay_origin,
    })
}

fn parse_v04(blob: &[u8]) -> Result<PairLink, PairLinkError> {
    const TOTAL: usize = 40;
    require(blob, TOTAL)?;
    if blob[1] != ADDR_TYPE_IPV4 {
        return Err(PairLinkError::UnsupportedAddressType(blob[1]));
    }
    let octets = &blob[2..6];
    let port = normalize_port(u16::from_be_bytes([blob[6], blob[7]]));
    let nonce = &blob[8..8 + NONCE_LEN];
    let ca_fp = &blob[24..24 + CA_FP_LEN];

    let candidates = if is_loopback(octets) {
        Vec::new()
    } else {
        vec![Endpoint {
            host: ipv4_string(octets),
            port,
        }]
    };
    if candidates.is_empty() {
        return Err(PairLinkError::NoReachableCandidate);
    }
    Ok(PairLink {
        candidates,
        nonce_hex: hex_lower(nonce),
        ca_fp_prefix: ca_fp.to_vec(),
    })
}

fn parse_v05(blob: &[u8]) -> Result<PairLink, PairLinkError> {
    require(blob, 3)?;
    if blob[1] != ADDR_TYPE_IPV4 {
        return Err(PairLinkError::UnsupportedAddressType(blob[1]));
    }
    let count = blob[2] as usize;
    require(blob, 5)?;
    let port = normalize_port(u16::from_be_bytes([blob[3], blob[4]]));
    let addrs_start = 5;
    let addrs_end = addrs_start + 4 * count;
    let nonce_end = addrs_end + NONCE_LEN;
    let total = nonce_end + CA_FP_LEN;
    require(blob, total)?;

    let mut candidates = Vec::with_capacity(count);
    for i in 0..count {
        let octets = &blob[addrs_start + 4 * i..addrs_start + 4 * i + 4];
        if is_loopback(octets) {
            continue;
        }
        candidates.push(Endpoint {
            host: ipv4_string(octets),
            port,
        });
    }
    if candidates.is_empty() {
        return Err(PairLinkError::NoReachableCandidate);
    }
    let nonce = &blob[addrs_end..nonce_end];
    let ca_fp = &blob[nonce_end..total];
    Ok(PairLink {
        candidates,
        nonce_hex: hex_lower(nonce),
        ca_fp_prefix: ca_fp.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crockford;

    const RELAY_WELL_KNOWN_FRAGMENT: &str = "0R0J6HB7H6NWVVR1VTPVXVYAZTXBW0938NKRKAYDXW00";
    const RELAY_CUSTOM_FRAGMENT: &str =
        "0R0J6HB7H6NWVVR1VTPVXVYAZTXBW0938NKRKAYDXWAPGX3ME1SKMBSFE9JPRRBS5SJQGRBDE1P6A";

    fn direct(parsed: ParsedPairLink) -> PairLink {
        match parsed {
            ParsedPairLink::Direct(pl) => pl,
            ParsedPairLink::Relay(_) => panic!("expected direct pair-link"),
        }
    }

    fn relay(parsed: ParsedPairLink) -> RelayPairLink {
        match parsed {
            ParsedPairLink::Relay(pl) => pl,
            ParsedPairLink::Direct(_) => panic!("expected relay pair-link"),
        }
    }

    fn nonce16() -> [u8; 16] {
        [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ]
    }
    fn cafp16() -> [u8; 16] {
        [
            0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xab, 0xac, 0xad,
            0xae, 0xaf,
        ]
    }
    fn relay_s() -> [u8; 8] {
        [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]
    }

    fn relay_ca_fp_spki() -> [u8; 16] {
        [
            0xde, 0xad, 0xbe, 0xef, 0xca, 0xfe, 0xba, 0xbe, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
            0xcd, 0xef,
        ]
    }

    fn build_v05(addrs: &[[u8; 4]], port: u16) -> Vec<u8> {
        let mut b = vec![0x05, 0x01, addrs.len() as u8];
        b.extend_from_slice(&port.to_be_bytes());
        for a in addrs {
            b.extend_from_slice(a);
        }
        b.extend_from_slice(&nonce16());
        b.extend_from_slice(&cafp16());
        b
    }

    fn build_v04(addr: [u8; 4], port: u16) -> Vec<u8> {
        let mut b = vec![0x04, 0x01];
        b.extend_from_slice(&addr);
        b.extend_from_slice(&port.to_be_bytes());
        b.extend_from_slice(&nonce16());
        b.extend_from_slice(&cafp16());
        b
    }

    fn build_v06(origin: Option<&str>, ca_fp_tag: u8) -> Vec<u8> {
        let mut b = vec![0x06];
        b.extend_from_slice(&relay_s());
        b.push(ca_fp_tag);
        b.extend_from_slice(&relay_ca_fp_spki());
        match origin {
            Some(origin) => {
                b.push(origin.len() as u8);
                b.extend_from_slice(origin.as_bytes());
            }
            None => b.push(0),
        }
        b
    }

    fn assert_relay_fields(pl: RelayPairLink, relay_origin: &str) {
        assert_eq!(pl.s, relay_s());
        assert_eq!(pl.ca_fp_spki, relay_ca_fp_spki().to_vec());
        assert_eq!(pl.relay_origin, relay_origin);
    }

    #[test]
    fn parses_v05_multi_address() {
        let blob = build_v05(&[[192, 0, 2, 10], [198, 51, 100, 20]], 7657);
        let url = format!("https://go.solstone.app/p#{}", crockford::encode(&blob));
        let pl = direct(parse(&url).unwrap());
        assert_eq!(
            pl.candidates,
            vec![
                Endpoint {
                    host: "192.0.2.10".into(),
                    port: 7657
                },
                Endpoint {
                    host: "198.51.100.20".into(),
                    port: 7657
                },
            ]
        );
        assert_eq!(pl.nonce_hex, "000102030405060708090a0b0c0d0e0f");
        assert_eq!(pl.ca_fp_prefix, cafp16().to_vec());
    }

    #[test]
    fn parses_v04_single_address() {
        let blob = build_v04([10, 0, 0, 5], 7657);
        let pl = direct(parse(&crockford::encode(&blob)).unwrap());
        assert_eq!(pl.candidates.len(), 1);
        assert_eq!(pl.candidates[0].host, "10.0.0.5");
        assert_eq!(pl.ca_fp_prefix.len(), 16);
    }

    #[test]
    fn port_zero_defaults_to_direct_port() {
        let blob = build_v05(&[[10, 0, 0, 5]], 0);
        let pl = direct(parse_blob(&blob).unwrap());
        assert_eq!(pl.candidates[0].port, DEFAULT_DIRECT_PORT);
    }

    #[test]
    fn filters_loopback_candidates() {
        let blob = build_v05(&[[127, 0, 0, 1], [192, 168, 1, 9]], 7657);
        let pl = direct(parse_blob(&blob).unwrap());
        assert_eq!(pl.candidates.len(), 1);
        assert_eq!(pl.candidates[0].host, "192.168.1.9");
    }

    #[test]
    fn all_loopback_is_no_reachable_candidate() {
        let blob = build_v05(&[[127, 0, 0, 1]], 7657);
        assert_eq!(
            parse_blob(&blob).unwrap_err(),
            PairLinkError::NoReachableCandidate
        );
    }

    #[test]
    fn rejects_truncated_blob() {
        let blob = build_v05(&[[10, 0, 0, 1], [10, 0, 0, 2]], 7657);
        let truncated = &blob[..blob.len() - 4];
        assert!(matches!(
            parse_blob(truncated).unwrap_err(),
            PairLinkError::Truncated { .. }
        ));
    }

    #[test]
    fn rejects_unknown_version() {
        assert_eq!(
            parse_blob(&[0x02, 0x01, 0x00]).unwrap_err(),
            PairLinkError::UnsupportedVersion(0x02)
        );
    }

    #[test]
    fn parses_v06_well_known_conformance_fragment() {
        let blob = build_v06(None, RELAY_CA_FP_TAG_SPKI_SHA256);
        assert_eq!(crockford::encode(&blob), RELAY_WELL_KNOWN_FRAGMENT);
        let pl = relay(parse(RELAY_WELL_KNOWN_FRAGMENT).unwrap());
        assert_relay_fields(pl, DEFAULT_RELAY_ORIGIN);
    }

    #[test]
    fn parses_v06_custom_origin_conformance_fragment() {
        let origin = "https://relay.example";
        let blob = build_v06(Some(origin), RELAY_CA_FP_TAG_SPKI_SHA256);
        assert_eq!(crockford::encode(&blob), RELAY_CUSTOM_FRAGMENT);
        let pl = relay(parse(RELAY_CUSTOM_FRAGMENT).unwrap());
        assert_relay_fields(pl, origin);
    }

    #[test]
    fn parses_v06_from_built_blob() {
        let origin = "https://relay.example";
        let pl = relay(parse_blob(&build_v06(Some(origin), RELAY_CA_FP_TAG_SPKI_SHA256)).unwrap());
        assert_relay_fields(pl, origin);
    }

    #[test]
    fn rejects_unknown_relay_ca_fp_tag() {
        assert_eq!(
            parse_blob(&build_v06(None, 0x02)).unwrap_err(),
            PairLinkError::UnknownCaFpTag(0x02)
        );
    }

    #[test]
    fn rejects_v06_truncation_before_selector() {
        let blob = build_v06(None, RELAY_CA_FP_TAG_SPKI_SHA256);
        assert!(matches!(
            parse_blob(&blob[..26]).unwrap_err(),
            PairLinkError::Truncated {
                expected: RELAY_WINDOW_BASE_LEN,
                got: 26
            }
        ));
    }

    #[test]
    fn rejects_v06_custom_origin_truncation() {
        let blob = build_v06(Some("https://relay.example"), RELAY_CA_FP_TAG_SPKI_SHA256);
        assert!(matches!(
            parse_blob(&blob[..blob.len() - 1]).unwrap_err(),
            PairLinkError::Truncated { .. }
        ));
    }

    #[test]
    fn rejects_v06_selector_length_mismatch() {
        let mut blob = build_v06(None, RELAY_CA_FP_TAG_SPKI_SHA256);
        blob.push(0xff);
        assert_eq!(
            parse_blob(&blob).unwrap_err(),
            PairLinkError::LengthMismatch {
                expected: RELAY_WINDOW_BASE_LEN,
                got: RELAY_WINDOW_BASE_LEN + 1
            }
        );
    }

    #[test]
    fn rejects_bad_relay_origin_utf8() {
        let mut blob = build_v06(None, RELAY_CA_FP_TAG_SPKI_SHA256);
        blob[26] = 1;
        blob.push(0xff);
        assert_eq!(
            parse_blob(&blob).unwrap_err(),
            PairLinkError::BadRelayOrigin
        );
    }
}
