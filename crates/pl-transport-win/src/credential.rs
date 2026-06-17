// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The pairing credential and its persistence.
//!
//! Pairing mints a per-device EC P-256 key + CSR locally; the journal signs the
//! CSR and returns the client cert + CA chain. That credential (plus the
//! registered observer handle) is the durable identity, stored under the
//! per-user data dir so the observer resumes uploading after a restart without
//! re-pairing. The private key never leaves the machine.

use observer_pl::pairlink::Endpoint;
use rcgen::{CertificateParams, DnType, KeyPair, PKCS_ECDSA_P256_SHA256};
use serde::{Deserialize, Serialize};

use crate::TransportError;

/// A dialable journal endpoint (serializable form of [`Endpoint`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointAddr {
    pub host: String,
    pub port: u16,
}

impl From<&Endpoint> for EndpointAddr {
    fn from(e: &Endpoint) -> Self {
        Self {
            host: e.host.clone(),
            port: e.port,
        }
    }
}

impl EndpointAddr {
    pub fn to_endpoint(&self) -> Endpoint {
        Endpoint {
            host: self.host.clone(),
            port: self.port,
        }
    }
}

/// The signed pairing identity: client key + cert, the CA chain to trust, the
/// pinned CA-fp prefix, the journal identity, and where to reach it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credential {
    pub client_key_pem: String,
    pub client_cert_pem: String,
    pub ca_chain_pem: Vec<String>,
    pub ca_fp_prefix: Vec<u8>,
    pub instance_id: String,
    pub home_label: String,
    pub endpoints: Vec<EndpointAddr>,
}

/// The full persisted sync identity: the credential plus the registered
/// observer handle (minted by `/app/observer/register`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PairedState {
    pub credential: Option<Credential>,
    pub observer_key: Option<String>,
}

impl PairedState {
    /// Load from a JSON file, returning the default (unpaired) state if absent.
    pub fn load(path: &std::path::Path) -> Result<Self, TransportError> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(TransportError::Io(e)),
        }
    }

    /// Atomically persist to a JSON file (write-temp-then-rename).
    pub fn save(&self, path: &std::path::Path) -> Result<(), TransportError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn is_paired(&self) -> bool {
        self.credential.is_some()
    }
}

/// A freshly-generated device key + the CSR PEM to send to the journal.
pub struct GeneratedKey {
    pub key_pem: String,
    pub csr_pem: String,
}

/// Generate an EC P-256 key and a CSR with `device_label` as the CN. The
/// journal signs the CSR; the key stays local.
pub fn generate_csr(device_label: &str) -> Result<GeneratedKey, TransportError> {
    let key_pair = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)
        .map_err(|e| TransportError::Crypto(format!("keygen: {e}")))?;
    let mut params = CertificateParams::new(Vec::<String>::new())
        .map_err(|e| TransportError::Crypto(format!("csr params: {e}")))?;
    params
        .distinguished_name
        .push(DnType::CommonName, device_label);
    let csr = params
        .serialize_request(&key_pair)
        .map_err(|e| TransportError::Crypto(format!("csr serialize: {e}")))?;
    let csr_pem = csr
        .pem()
        .map_err(|e| TransportError::Crypto(format!("csr pem: {e}")))?;
    Ok(GeneratedKey {
        key_pem: key_pair.serialize_pem(),
        csr_pem,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_csr_is_pem_with_local_key() {
        let g = generate_csr("solstone-windows-test").unwrap();
        assert!(g.csr_pem.contains("BEGIN CERTIFICATE REQUEST"));
        assert!(g.key_pem.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn paired_state_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("plw-cred-{}", std::process::id()));
        let path = dir.join("pairing.json");
        let state = PairedState {
            credential: Some(Credential {
                client_key_pem: "K".into(),
                client_cert_pem: "C".into(),
                ca_chain_pem: vec!["CA".into()],
                ca_fp_prefix: vec![1, 2, 3, 4],
                instance_id: "inst".into(),
                home_label: "Home".into(),
                endpoints: vec![EndpointAddr {
                    host: "10.0.0.5".into(),
                    port: 7657,
                }],
            }),
            observer_key: Some("obs-handle".into()),
        };
        state.save(&path).unwrap();
        let loaded = PairedState::load(&path).unwrap();
        assert!(loaded.is_paired());
        assert_eq!(loaded.observer_key.as_deref(), Some("obs-handle"));
        assert_eq!(loaded.credential.unwrap().endpoints[0].port, 7657);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_file_is_unpaired_default() {
        let path = std::env::temp_dir().join("plw-does-not-exist-xyz.json");
        let _ = std::fs::remove_file(&path);
        let state = PairedState::load(&path).unwrap();
        assert!(!state.is_paired());
    }
}
