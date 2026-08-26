// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The observer wire types — request/response bodies, serde-shaped to match the
//! journal (`solstone` convey) exactly.
//!
//! Endpoint shapes verified against `apps/link/routes.py` (`/pair`). Field names
//! are the journal's JSON keys; anything the client doesn't consume (e.g.
//! `local_endpoints`, `home_attestation`) is
//! optional so a server adding fields never breaks the client.

use serde::{Deserialize, Serialize};

// ── /app/network/pair ────────────────────────────────────────────────────────

/// POST body for `/app/network/pair?token=<nonce>`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairRequest {
    pub csr: String,
    pub device_label: String,
}

/// Success response from `/app/network/pair`. The journal signs our CSR and returns
/// the client cert plus the CA chain to trust.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairResponse {
    pub client_cert: String,
    pub ca_chain: Vec<String>,
    pub instance_id: String,
    pub home_label: String,
    /// `"sha256:<hex>"` of the signed client cert DER — we verify it matches.
    pub fingerprint: String,
    #[serde(default)]
    pub home_attestation: Option<String>,
    /// The journal's own LAN endpoints; unused by the client (we already have
    /// the pair-link candidates) but captured so deserialization never fails.
    #[serde(default)]
    pub local_endpoints: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_response_tolerates_extra_and_missing_fields() {
        let raw = r#"{"client_cert":"PEM","ca_chain":["CA"],"instance_id":"id","home_label":"Home","fingerprint":"sha256:deadbeef"}"#;
        let resp: PairResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(resp.fingerprint, "sha256:deadbeef");
        assert_eq!(resp.ca_chain, vec!["CA".to_string()]);
        assert!(resp.home_attestation.is_none());
    }
}
