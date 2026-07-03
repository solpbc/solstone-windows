// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The pairing credential and its persistence.
//!
//! Pairing mints a per-device EC P-256 key + CSR locally; the journal signs the
//! CSR and returns the client cert + CA chain. That credential (plus the
//! registered observer handle) is the durable identity, stored under the
//! per-user data dir so the observer resumes uploading after a restart without
//! re-pairing. The private key never leaves the machine.

use std::path::Path;

use base64::Engine as _;
use observer_pl::pairlink::Endpoint;
use rcgen::{CertificateParams, DnType, KeyPair, PKCS_ECDSA_P256_SHA256};
use serde::{Deserialize, Serialize};

use crate::TransportError;

const CREDENTIAL_WRAP_MARKER: &str = "dpapi:v1:";

trait Protector {
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, TransportError>;
    fn unprotect(&self, blob: &[u8]) -> Result<Vec<u8>, TransportError>;
}

fn wrap_secret(protector: &dyn Protector, plain: &str) -> Result<String, TransportError> {
    let blob = protector.protect(plain.as_bytes())?;
    Ok(format!(
        "{CREDENTIAL_WRAP_MARKER}{}",
        base64::engine::general_purpose::STANDARD.encode(blob)
    ))
}

fn unwrap_secret(protector: &dyn Protector, stored: &str) -> Result<String, TransportError> {
    match stored.strip_prefix(CREDENTIAL_WRAP_MARKER) {
        Some(b64) => {
            let blob = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| {
                    TransportError::Crypto(format!("credential unwrap: bad base64: {e}"))
                })?;
            let plain = protector.unprotect(&blob)?;
            String::from_utf8(plain)
                .map_err(|e| TransportError::Crypto(format!("credential unwrap: bad utf8: {e}")))
        }
        None => Ok(stored.to_string()), // legacy plaintext — NEVER call unprotect
    }
}

#[cfg(not(windows))]
struct PassthroughProtector;

#[cfg(not(windows))]
impl Protector for PassthroughProtector {
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, TransportError> {
        Ok(plain.to_vec())
    }

    fn unprotect(&self, blob: &[u8]) -> Result<Vec<u8>, TransportError> {
        Ok(blob.to_vec())
    }
}

#[cfg(not(windows))]
fn platform_protector() -> PassthroughProtector {
    PassthroughProtector
}

#[cfg(windows)]
fn platform_protector() -> DpapiProtector {
    DpapiProtector
}

#[cfg(windows)]
struct DpapiProtector;

#[cfg(windows)]
impl Protector for DpapiProtector {
    #[allow(unsafe_code)]
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, TransportError> {
        use std::ffi::c_void;
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{LocalFree, HLOCAL};
        use windows::Win32::Security::Cryptography::{
            CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
        };
        let cb = u32::try_from(plain.len())
            .map_err(|_| TransportError::Crypto("dpapi protect: input too large".into()))?;
        let in_blob = CRYPT_INTEGER_BLOB {
            cbData: cb,
            pbData: plain.as_ptr().cast_mut(),
        };
        let mut out_blob = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: core::ptr::null_mut(),
        };
        // SAFETY: in_blob describes `plain` for its full length and is only read.
        // out_blob is owned here; on success DPAPI LocalAlloc's out_blob.pbData,
        // which we copy out and LocalFree before returning. No pointer escapes.
        unsafe {
            CryptProtectData(
                &in_blob,
                PCWSTR::null(),
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out_blob,
            )
            .map_err(|e| TransportError::Crypto(format!("dpapi protect: {e}")))?;
            let out =
                std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize).to_vec();
            let _ = LocalFree(HLOCAL(out_blob.pbData.cast::<c_void>()));
            Ok(out)
        }
    }

    #[allow(unsafe_code)]
    fn unprotect(&self, blob: &[u8]) -> Result<Vec<u8>, TransportError> {
        use std::ffi::c_void;
        use windows::Win32::Foundation::{LocalFree, HLOCAL};
        use windows::Win32::Security::Cryptography::{
            CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
        };
        let cb = u32::try_from(blob.len())
            .map_err(|_| TransportError::Crypto("dpapi unprotect: input too large".into()))?;
        let in_blob = CRYPT_INTEGER_BLOB {
            cbData: cb,
            pbData: blob.as_ptr().cast_mut(),
        };
        let mut out_blob = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: core::ptr::null_mut(),
        };
        // SAFETY: as protect(); a wrong-user or corrupt blob returns Err (mapped
        // to Crypto), never a partial read. out_blob.pbData is LocalFree'd after copy.
        unsafe {
            CryptUnprotectData(
                &in_blob,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out_blob,
            )
            .map_err(|e| TransportError::Crypto(format!("dpapi unprotect: {e}")))?;
            let out =
                std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize).to_vec();
            let _ = LocalFree(HLOCAL(out_blob.pbData.cast::<c_void>()));
            Ok(out)
        }
    }
}

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
    pub fn load(path: &Path) -> Result<Self, TransportError> {
        Self::load_with(&platform_protector(), path)
    }

    fn load_with(protector: &dyn Protector, path: &Path) -> Result<Self, TransportError> {
        let mut state: Self = match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(TransportError::Io(e)),
        };
        if let Some(cred) = state.credential.as_mut() {
            let client_key_pem = unwrap_secret(protector, &cred.client_key_pem)?;
            cred.client_key_pem = client_key_pem;
            if let Some(token) = cred.device_token.take() {
                cred.device_token = Some(unwrap_secret(protector, &token)?);
            }
        }
        Ok(state)
    }

    /// Atomically persist to a JSON file (write-temp-then-rename).
    pub fn save(&self, path: &Path) -> Result<(), TransportError> {
        self.save_with(&platform_protector(), path)
    }

    fn save_with(&self, protector: &dyn Protector, path: &Path) -> Result<(), TransportError> {
        let mut state = self.clone();
        if let Some(cred) = state.credential.as_mut() {
            cred.client_key_pem = wrap_secret(protector, &cred.client_key_pem)?;
            if let Some(token) = cred.device_token.take() {
                cred.device_token = Some(wrap_secret(protector, &token)?);
            }
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&state)?)?;
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

    use std::cell::Cell;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[derive(Clone, Copy)]
    enum TestMode {
        Reversible,
        AlwaysFail,
        FailOnNth(usize),
    }

    struct TestProtector {
        mode: TestMode,
        unprotect_calls: Cell<usize>,
    }

    impl TestProtector {
        fn new(mode: TestMode) -> Self {
            Self {
                mode,
                unprotect_calls: Cell::new(0),
            }
        }

        fn reversible() -> Self {
            Self::new(TestMode::Reversible)
        }
    }

    impl Protector for TestProtector {
        fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, TransportError> {
            Ok(xor_5a(plain))
        }

        fn unprotect(&self, blob: &[u8]) -> Result<Vec<u8>, TransportError> {
            let calls = self.unprotect_calls.get() + 1;
            self.unprotect_calls.set(calls);
            match self.mode {
                TestMode::AlwaysFail => Err(TransportError::Crypto("fake unprotect failed".into())),
                TestMode::FailOnNth(n) if calls == n => {
                    Err(TransportError::Crypto("fake unprotect failed".into()))
                }
                TestMode::Reversible | TestMode::FailOnNth(_) => Ok(xor_5a(blob)),
            }
        }
    }

    struct PanicUnprotectProtector;

    impl Protector for PanicUnprotectProtector {
        fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, TransportError> {
            Ok(plain.to_vec())
        }

        fn unprotect(&self, _blob: &[u8]) -> Result<Vec<u8>, TransportError> {
            panic!("unprotect must not be called on legacy plaintext")
        }
    }

    fn xor_5a(bytes: &[u8]) -> Vec<u8> {
        bytes.iter().map(|byte| byte ^ 0x5a).collect()
    }

    fn paired_state_with(client_key_pem: &str, device_token: Option<&str>) -> PairedState {
        PairedState {
            credential: Some(Credential {
                client_key_pem: client_key_pem.into(),
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
                device_token: device_token.map(str::to_string),
                device_token_expires_at: None,
            }),
            observer_key: Some("obs-handle".into()),
            observer_name: Some("winbox".into()),
        }
    }

    fn temp_pairing_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("plw-cred-{name}-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("pairing.json")
    }

    fn write_raw_state(path: &std::path::Path, state: &PairedState) {
        std::fs::write(path, serde_json::to_vec_pretty(state).unwrap()).unwrap();
    }

    fn raw_credential_field(raw: &str, field: &str) -> String {
        let json: serde_json::Value = serde_json::from_str(raw).unwrap();
        json.get("credential")
            .and_then(|credential| credential.get(field))
            .and_then(serde_json::Value::as_str)
            .unwrap()
            .to_string()
    }

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
    fn legacy_plaintext_valid_base64_loads_without_unprotect() {
        let path = temp_pairing_path("legacy-valid-base64");
        let state = paired_state_with("S0tLSw==", Some("dG9rZW4="));
        write_raw_state(&path, &state);

        let loaded = PairedState::load_with(&PanicUnprotectProtector, &path).unwrap();
        let credential = loaded.credential.unwrap();
        assert_eq!(credential.client_key_pem, "S0tLSw==");
        assert_eq!(credential.device_token.as_deref(), Some("dG9rZW4="));

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn public_load_save_migrates_legacy_plaintext_to_marked_disk() {
        let path = temp_pairing_path("public-migrate");
        let state = paired_state_with("LEGACY-KEY-PEM", Some("LEGACY-TOKEN"));
        write_raw_state(&path, &state);

        let loaded = PairedState::load(&path).unwrap();
        assert!(loaded.is_paired());
        let credential = loaded.credential.as_ref().unwrap();
        assert_eq!(credential.client_key_pem, "LEGACY-KEY-PEM");
        assert_eq!(credential.device_token.as_deref(), Some("LEGACY-TOKEN"));

        loaded.save(&path).unwrap();
        let reloaded = PairedState::load(&path).unwrap();
        assert!(reloaded.is_paired());
        let credential = reloaded.credential.unwrap();
        assert_eq!(credential.client_key_pem, "LEGACY-KEY-PEM");
        assert_eq!(credential.device_token.as_deref(), Some("LEGACY-TOKEN"));

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.matches(CREDENTIAL_WRAP_MARKER).count() >= 2);
        assert!(!raw.contains("LEGACY-KEY-PEM"));
        assert!(!raw.contains("LEGACY-TOKEN"));

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn marked_field_unprotect_failure_is_crypto_error() {
        let path = temp_pairing_path("marked-failure");
        let state = paired_state_with(&format!("{CREDENTIAL_WRAP_MARKER}S0s="), None);
        write_raw_state(&path, &state);
        let protector = TestProtector::new(TestMode::AlwaysFail);

        let result = PairedState::load_with(&protector, &path);
        assert!(matches!(result, Err(TransportError::Crypto(_))));

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn second_secret_unprotect_failure_returns_error() {
        let path = temp_pairing_path("second-secret-failure");
        let state = paired_state_with("ROUNDTRIP-KEY-PEM", Some("ROUNDTRIP-TOKEN"));
        let writer = TestProtector::reversible();
        state.save_with(&writer, &path).unwrap();

        let reader = TestProtector::new(TestMode::FailOnNth(2));
        let result = PairedState::load_with(&reader, &path);
        assert!(matches!(result, Err(TransportError::Crypto(_))));
        assert_eq!(reader.unprotect_calls.get(), 2);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reversible_protector_wraps_on_disk_and_round_trips() {
        let path = temp_pairing_path("reversible-round-trip");
        let state = paired_state_with("ROUNDTRIP-KEY-PEM", Some("ROUNDTRIP-TOKEN"));
        let protector = TestProtector::reversible();

        state.save_with(&protector, &path).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw_credential_field(&raw, "client_key_pem").starts_with(CREDENTIAL_WRAP_MARKER));
        assert!(raw_credential_field(&raw, "device_token").starts_with(CREDENTIAL_WRAP_MARKER));
        assert!(!raw.contains("ROUNDTRIP-KEY-PEM"));
        assert!(!raw.contains("ROUNDTRIP-TOKEN"));

        let loaded = PairedState::load_with(&protector, &path).unwrap();
        let credential = loaded.credential.unwrap();
        assert_eq!(credential.client_key_pem, "ROUNDTRIP-KEY-PEM");
        assert_eq!(credential.device_token.as_deref(), Some("ROUNDTRIP-TOKEN"));

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
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

    #[cfg(windows)]
    #[test]
    fn dpapi_save_does_not_leave_plaintext_on_disk() {
        let path = temp_pairing_path("dpapi-disk");
        let key = concat!(
            "-----BEGIN PRIVATE KEY-----\n",
            "WINDOWS-DPAPI-SECRET-KEY-MATERIAL\n",
            "-----END PRIVATE KEY-----"
        );
        let token = "windows-dpapi-secret-token";
        let state = paired_state_with(key, Some(token));

        state.save(&path).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.matches(CREDENTIAL_WRAP_MARKER).count() >= 2);
        assert!(!raw.contains("WINDOWS-DPAPI-SECRET-KEY-MATERIAL"));
        assert!(!raw.contains(token));

        let loaded = PairedState::load(&path).unwrap();
        let credential = loaded.credential.unwrap();
        assert_eq!(credential.client_key_pem, key);
        assert_eq!(credential.device_token.as_deref(), Some(token));

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_protect_unprotect_round_trips() {
        let protector = DpapiProtector;
        let plain = b"secret bytes";

        let protected = protector.protect(plain).unwrap();
        assert_ne!(protected, plain.to_vec());
        let unprotected = protector.unprotect(&protected).unwrap();
        assert_eq!(unprotected, plain.to_vec());
    }
}
