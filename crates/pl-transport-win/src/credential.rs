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
    #[serde(default)]
    pub relay_origin: Option<String>,
    #[serde(default)]
    pub device_token: Option<String>,
    #[serde(default)]
    pub device_token_expires_at: Option<i64>,
}

/// The full persisted sync identity: the credential plus the registered
/// observer handle (minted by `/app/observer/register`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PairedState {
    pub credential: Option<Credential>,
    pub observer_key: Option<String>,
    #[serde(default)]
    pub observer_name: Option<String>,
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

pub(crate) fn endpoint_addrs_from_local_endpoints(
    value: Option<&serde_json::Value>,
) -> Vec<EndpointAddr> {
    // Relay pair-response local_endpoints are {ip, port, scope}; scope is kept
    // server-side for now and intentionally not persisted by this lode.
    let Some(serde_json::Value::Array(entries)) = value else {
        return Vec::new();
    };

    entries
        .iter()
        .filter_map(|entry| {
            let object = entry.as_object()?;
            let host = object.get("ip")?.as_str()?;
            let port = object.get("port")?.as_u64()?;
            let port = u16::try_from(port).ok()?;
            if port == 0 {
                return None;
            }
            Some(EndpointAddr {
                host: host.to_string(),
                port,
            })
        })
        .collect()
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
                relay_origin: None,
                device_token: None,
                device_token_expires_at: None,
            }),
            observer_key: Some("obs-handle".into()),
            observer_name: Some("winbox".into()),
        };
        state.save(&path).unwrap();
        let loaded = PairedState::load(&path).unwrap();
        assert!(loaded.is_paired());
        assert_eq!(loaded.observer_key.as_deref(), Some("obs-handle"));
        assert_eq!(loaded.observer_name.as_deref(), Some("winbox"));
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

    #[test]
    fn relay_fields_round_trip() {
        let state = PairedState {
            credential: Some(Credential {
                client_key_pem: "K".into(),
                client_cert_pem: "C".into(),
                ca_chain_pem: vec!["CA".into()],
                ca_fp_prefix: vec![1, 2, 3, 4],
                instance_id: "inst".into(),
                home_label: "Home".into(),
                endpoints: Vec::new(),
                relay_origin: Some("https://link.solstone.app".into()),
                device_token: Some("token".into()),
                device_token_expires_at: Some(123),
            }),
            observer_key: Some("obs-handle".into()),
            observer_name: None,
        };
        let json = serde_json::to_string(&state).unwrap();
        let loaded: PairedState = serde_json::from_str(&json).unwrap();
        let credential = loaded.credential.unwrap();
        assert_eq!(
            credential.relay_origin.as_deref(),
            Some("https://link.solstone.app")
        );
        assert_eq!(credential.device_token.as_deref(), Some("token"));
        assert_eq!(credential.device_token_expires_at, Some(123));
    }

    #[test]
    fn pre_w2_pairing_json_loads_with_default_relay_fields() {
        let json = r#"{
          "credential": {
            "client_key_pem": "K",
            "client_cert_pem": "C",
            "ca_chain_pem": ["CA"],
            "ca_fp_prefix": [1, 2, 3, 4],
            "instance_id": "inst",
            "home_label": "Home",
            "endpoints": [{"host": "10.0.0.5", "port": 7657}]
          },
          "observer_key": "obs-handle",
          "observer_name": "winbox"
        }"#;
        let loaded: PairedState = serde_json::from_str(json).unwrap();
        assert!(loaded.is_paired());
        let credential = loaded.credential.unwrap();
        assert_eq!(credential.relay_origin, None);
        assert_eq!(credential.device_token, None);
        assert_eq!(credential.device_token_expires_at, None);
    }

    #[test]
    fn local_endpoints_helper_maps_valid_entries_and_skips_invalid() {
        let value = serde_json::json!([
            {"ip": "10.0.0.2", "port": 7657, "scope": "lan"},
            {"ip": "10.0.0.3", "port": 0, "scope": "lan"},
            {"ip": "10.0.0.4", "port": 70000, "scope": "lan"},
            {"ip": 42, "port": 7657},
            {"host": "10.0.0.5", "port": 7657},
            "bad"
        ]);
        assert_eq!(
            endpoint_addrs_from_local_endpoints(Some(&value)),
            vec![EndpointAddr {
                host: "10.0.0.2".into(),
                port: 7657
            }]
        );
        assert!(endpoint_addrs_from_local_endpoints(None).is_empty());
        assert!(
            endpoint_addrs_from_local_endpoints(Some(&serde_json::json!({"ip": "10.0.0.2"})))
                .is_empty()
        );
    }
}
