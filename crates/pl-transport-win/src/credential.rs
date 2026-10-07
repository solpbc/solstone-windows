// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The pairing credential and its persistence.
//!
//! Pairing mints a per-device EC P-256 key + CSR locally; the journal signs the
//! CSR and returns the client cert + CA chain. That credential is the durable
//! identity, stored under the per-user data dir so the observer resumes uploading
//! after a restart without re-pairing. The private key never leaves the machine.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use rcgen::{CertificateParams, DnType, KeyPair, PKCS_ECDSA_P256_SHA256};
use serde::{Deserialize, Serialize};
use spl_core::pairlink::Endpoint;

use crate::client::RelayFence;
use crate::TransportError;
use spl_transport::client::{TokenCommit, TokenCommitContext, TokenTransaction};

const CREDENTIAL_WRAP_MARKER: &str = "dpapi:v1:";

#[derive(Debug)]
#[allow(dead_code)] // Refusal and I/O classes are produced by the Windows DPAPI implementation.
pub(crate) enum ProtectionError {
    Refused,
    Failure(TransportError),
}

pub(crate) trait Protector {
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, ProtectionError>;
    fn unprotect(&self, blob: &[u8]) -> Result<Vec<u8>, ProtectionError>;
}

impl<P: Protector + ?Sized> Protector for Box<P> {
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, ProtectionError> {
        (**self).protect(plain)
    }

    fn unprotect(&self, blob: &[u8]) -> Result<Vec<u8>, ProtectionError> {
        (**self).unprotect(blob)
    }
}

fn wrap_secret(protector: &dyn Protector, plain: &str) -> Result<String, TransportError> {
    let blob = protector
        .protect(plain.as_bytes())
        .map_err(|error| match error {
            ProtectionError::Refused => {
                TransportError::Crypto("credential protection refused".into())
            }
            ProtectionError::Failure(error) => error,
        })?;
    Ok(format!(
        "{CREDENTIAL_WRAP_MARKER}{}",
        base64::engine::general_purpose::STANDARD.encode(blob)
    ))
}

fn unwrap_secret(
    protector: &dyn Protector,
    stored: &str,
    client_key: bool,
) -> Result<String, TransportError> {
    match stored.strip_prefix(CREDENTIAL_WRAP_MARKER) {
        Some(b64) => {
            let blob = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|_| TransportError::CredentialMalformed)?;
            let plain = protector.unprotect(&blob).map_err(|error| match error {
                ProtectionError::Refused if client_key => {
                    TransportError::ClientKeyProtectionRefused
                }
                ProtectionError::Refused => TransportError::RelayTokenProtectionRefused,
                ProtectionError::Failure(error) => error,
            })?;
            String::from_utf8(plain).map_err(|_| TransportError::CredentialMalformed)
        }
        None if stored.starts_with("dpapi:") => Err(TransportError::CredentialMalformed),
        None => Ok(stored.to_string()), // legacy plaintext — NEVER call unprotect
    }
}

fn protect_credential(
    protector: &dyn Protector,
    credential: &mut Credential,
) -> Result<(), StorageError> {
    credential.client_key_pem = wrap_secret(protector, &credential.client_key_pem)?;
    if let Some(token) = credential.device_token.take() {
        credential.device_token = Some(wrap_secret(protector, &token)?);
    }
    Ok(())
}

fn unwrap_credential(
    protector: &dyn Protector,
    state_path: &Path,
    credential: &mut Credential,
    relay_token_refused: &mut bool,
) -> Result<(), TransportError> {
    let stored_key = credential.client_key_pem.clone();
    credential.client_key_pem = match unwrap_secret(protector, &stored_key, true) {
        Ok(key) => key,
        Err(TransportError::ClientKeyProtectionRefused) => {
            let evidence =
                extract_protected_blob(&stored_key).ok_or(TransportError::CredentialMalformed)?;
            persist_key_recovery_evidence(state_path, &evidence)
                .map_err(|_| TransportError::CredentialRecoveryRequired)?;
            return Err(TransportError::ClientKeyProtectionRefused);
        }
        Err(error) => return Err(error),
    };
    if let Some(token) = credential.device_token.take() {
        match unwrap_secret(protector, &token, false) {
            Ok(token) => credential.device_token = Some(token),
            Err(TransportError::RelayTokenProtectionRefused) => {
                *relay_token_refused = true;
                credential.device_token_expires_at = None;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn extract_protected_blob(stored: &str) -> Option<Vec<u8>> {
    let encoded = stored.strip_prefix(CREDENTIAL_WRAP_MARKER)?;
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()
}

pub fn key_recovery_evidence_path(state_path: &Path) -> PathBuf {
    state_path.with_file_name("pairing-key-recovery.bin")
}

/// Where a pairing whose client key the platform refused to unprotect is kept
/// once launch has set it aside. The bytes are never deleted or rewritten.
pub fn refused_pairing_path(state_path: &Path) -> PathBuf {
    state_path.with_file_name("pairing-refused.json")
}

fn persist_key_recovery_evidence(state_path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let destination = key_recovery_evidence_path(state_path);
    if destination.exists() {
        return if std::fs::read(&destination)? == bytes {
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "client key recovery evidence conflict",
            ))
        };
    }
    let temp = destination.with_extension("bin.tmp");
    if temp.exists() {
        if std::fs::read(&temp)? != bytes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "client key recovery evidence temporary conflict",
            ));
        }
    } else {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    if std::fs::read(&temp)? != bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "client key recovery evidence readback mismatch",
        ));
    }
    publish_staged_file(&temp, &destination).map_err(|error| match error {
        StorageError::WriteFailed(error) | StorageError::DurabilityUncertain(error) => error,
        StorageError::Transport(error) => std::io::Error::other(error.to_string()),
        StorageError::Crypto(message) => std::io::Error::other(message),
        StorageError::CasMismatch => std::io::Error::other("client key recovery evidence conflict"),
    })?;
    #[cfg(not(windows))]
    sync_published_path(&destination)?;
    if std::fs::read(&destination)? != bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "client key recovery evidence publication mismatch",
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
struct PassthroughProtector;

#[cfg(not(windows))]
impl Protector for PassthroughProtector {
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, ProtectionError> {
        Ok(plain.to_vec())
    }

    fn unprotect(&self, blob: &[u8]) -> Result<Vec<u8>, ProtectionError> {
        Ok(blob.to_vec())
    }
}

#[cfg(not(windows))]
fn os_protector() -> PassthroughProtector {
    PassthroughProtector
}

#[cfg(windows)]
fn os_protector() -> DpapiProtector {
    DpapiProtector
}

#[cfg(test)]
thread_local! {
    /// Makes every platform protector refuse to unprotect, so launch paths can
    /// be exercised against a client-key refusal on any host.
    pub(crate) static REFUSE_UNPROTECT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
struct RefusingProtector;

#[cfg(test)]
impl Protector for RefusingProtector {
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, ProtectionError> {
        Ok(plain.to_vec())
    }

    fn unprotect(&self, _blob: &[u8]) -> Result<Vec<u8>, ProtectionError> {
        Err(ProtectionError::Refused)
    }
}

fn platform_protector() -> Box<dyn Protector> {
    #[cfg(test)]
    if REFUSE_UNPROTECT.with(|flag| flag.get()) {
        return Box::new(RefusingProtector);
    }
    Box::new(os_protector())
}

#[cfg(windows)]
struct DpapiProtector;

#[cfg(windows)]
impl Protector for DpapiProtector {
    #[allow(unsafe_code)]
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, ProtectionError> {
        use std::ffi::c_void;
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{LocalFree, HLOCAL};
        use windows::Win32::Security::Cryptography::{
            CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
        };
        let cb = u32::try_from(plain.len()).map_err(|_| {
            ProtectionError::Failure(TransportError::Crypto(
                "dpapi protect: input too large".into(),
            ))
        })?;
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
            .map_err(|e| {
                ProtectionError::Failure(TransportError::Crypto(format!("dpapi protect: {e}")))
            })?;
            let out =
                std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize).to_vec();
            let _ = LocalFree(HLOCAL(out_blob.pbData.cast::<c_void>()));
            Ok(out)
        }
    }

    #[allow(unsafe_code)]
    fn unprotect(&self, blob: &[u8]) -> Result<Vec<u8>, ProtectionError> {
        use std::ffi::c_void;
        use windows::Win32::Foundation::{LocalFree, HLOCAL};
        use windows::Win32::Security::Cryptography::{
            CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
        };
        let cb = u32::try_from(blob.len()).map_err(|_| {
            ProtectionError::Failure(TransportError::Crypto(
                "dpapi unprotect: input too large".into(),
            ))
        })?;
        let in_blob = CRYPT_INTEGER_BLOB {
            cbData: cb,
            pbData: blob.as_ptr().cast_mut(),
        };
        let mut out_blob = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: core::ptr::null_mut(),
        };
        // SAFETY: as protect(); an invalid or unavailable user key returns Err,
        // never a partial read. out_blob.pbData is LocalFree'd after copy.
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
            .map_err(|error| {
                if error.code().0 == 0x8007_0005_u32 as i32 {
                    ProtectionError::Failure(TransportError::Io(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "DPAPI access denied",
                    )))
                } else {
                    ProtectionError::Refused
                }
            })?;
            let out =
                std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize).to_vec();
            let _ = LocalFree(HLOCAL(out_blob.pbData.cast::<c_void>()));
            Ok(out)
        }
    }
}

pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
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

/// Derive the exact pairing owner generation from the full SHA-256 of the
/// client certificate PEM. Same-home re-pair mints a new certificate, so this
/// value changes on every replacement and can be compared across restarts.
pub fn pairing_generation(client_cert_pem: &str) -> [u8; 32] {
    spl_core::ca::sha256(client_cert_pem.as_bytes())
}

/// The CAS key for pairing state mutations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CasKey {
    pub pairing_generation: [u8; 32],
    pub access_mutation_generation: u64,
}

/// Storage error for pairing state mutations.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("cas mismatch")]
    CasMismatch,
    #[error("write failed: {0}")]
    WriteFailed(std::io::Error),
    #[error("durability uncertain: {0}")]
    DurabilityUncertain(std::io::Error),
    #[error("transport error: {0}")]
    Transport(#[from] TransportError),
    #[error("crypto error: {0}")]
    Crypto(String),
}

impl From<StorageError> for TransportError {
    fn from(err: StorageError) -> Self {
        match err {
            StorageError::Transport(e) => e,
            StorageError::WriteFailed(e) | StorageError::DurabilityUncertain(e) => {
                TransportError::Io(e)
            }
            StorageError::Crypto(msg) => TransportError::Crypto(msg),
            StorageError::CasMismatch => TransportError::Pairing("cas mismatch".to_string()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PairedStateLoad {
    pub state: PairedState,
    pub relay_token_refused: bool,
}

/// Durable outcome of trying to retire a rejected pairing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetirementPhase {
    Prepared,
    Unknown,
    Succeeded,
}

/// A protected, generation-bound rejection of the current pairing that launch
/// can reconcile. While it is present the pairing is fenced from every send.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetirementIntent {
    pub schema: u32,
    pub operation_id: String,
    pub phase: RetirementPhase,
    /// Certificate generation of the exact credential being retired.
    pub owner_generation: [u8; 32],
    pub access_mutation_generation: u64,
    pub client_id: String,
    /// The credential being retired; its secrets are protected like pairing.
    pub credential: Credential,
}

static PAIRING_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn owner_state_write_guard() -> std::sync::MutexGuard<'static, ()> {
    PAIRING_MUTEX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(target_os = "linux")]
pub(crate) fn sync_published_path(path: &Path) -> Result<(), std::io::Error> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::File::open(parent)?.sync_all()
}

#[cfg(windows)]
#[allow(unsafe_code)]
pub(crate) fn publish_staged_file(staged: &Path, destination: &Path) -> Result<(), StorageError> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source: Vec<u16> = staged.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // SAFETY: both paths are NUL-terminated and remain live through the call.
    // The sibling staging file keeps the move on one volume; no copy fallback
    // is allowed. WRITE_THROUGH waits until the file has been moved on disk.
    let result = unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(target.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    result.map_err(|error| {
        let error = std::io::Error::other(error.to_string());
        if staged.exists() {
            StorageError::WriteFailed(error)
        } else {
            StorageError::DurabilityUncertain(error)
        }
    })
}

#[cfg(not(windows))]
pub(crate) fn publish_staged_file(staged: &Path, destination: &Path) -> Result<(), StorageError> {
    std::fs::rename(staged, destination).map_err(StorageError::WriteFailed)
}

#[cfg(not(any(target_os = "linux", windows)))]
pub(crate) fn sync_published_path(_path: &Path) -> Result<(), std::io::Error> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "durable publication sync is unsupported on this platform",
    ))
}

#[cfg(test)]
thread_local! {
    pub(crate) static FS_FAIL_POINT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    pub(crate) static PAIR_REJECTION_CLEANUP_FAIL_POINT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The full persisted sync identity: the paired mTLS credential.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PairedState {
    pub credential: Option<Credential>,
    #[serde(default)]
    pub access_mutation_generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retirement_intent: Option<RetirementIntent>,
}

impl PairedState {
    /// Load from a JSON file, returning the default (unpaired) state if absent.
    pub fn load(path: &Path) -> Result<Self, TransportError> {
        Self::load_detailed(path).map(|loaded| loaded.state)
    }

    fn load_with(protector: &dyn Protector, path: &Path) -> Result<Self, TransportError> {
        Self::load_detailed_with(protector, path).map(|loaded| loaded.state)
    }

    pub fn load_detailed(path: &Path) -> Result<PairedStateLoad, TransportError> {
        Self::load_detailed_with(&platform_protector(), path)
    }

    /// After a client-key refusal has persisted its recovery evidence, move
    /// the refused pairing aside so the profile reads as cleanly unpaired and
    /// fresh linking can proceed. Returns `false` when the stored client key is
    /// not refused: a token-only refusal keeps the pairing in place, and a
    /// malformed wrapper, I/O or missing-evidence condition stays an error.
    pub fn set_aside_refused_client_key(path: &Path) -> Result<bool, TransportError> {
        Self::set_aside_refused_client_key_with(&platform_protector(), path)
    }

    fn set_aside_refused_client_key_with(
        protector: &dyn Protector,
        path: &Path,
    ) -> Result<bool, TransportError> {
        let _guard = owner_state_write_guard();
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(TransportError::Io(error)),
        };
        let state: Self =
            serde_json::from_slice(&bytes).map_err(|_| TransportError::CredentialMalformed)?;
        let Some(credential) = state.credential.as_ref() else {
            return Ok(false);
        };
        match unwrap_secret(protector, &credential.client_key_pem, true) {
            Err(TransportError::ClientKeyProtectionRefused) => {}
            Err(error) => return Err(error),
            Ok(_) => return Ok(false),
        }
        let evidence = extract_protected_blob(&credential.client_key_pem)
            .ok_or(TransportError::CredentialMalformed)?;
        if std::fs::read(key_recovery_evidence_path(path))
            .map_err(|_| TransportError::CredentialRecoveryRequired)?
            != evidence
        {
            return Err(TransportError::CredentialRecoveryRequired);
        }
        let destination = refused_pairing_path(path);
        match std::fs::read(&destination) {
            Ok(existing) if existing == bytes => {}
            Ok(_) => return Err(TransportError::CredentialRecoveryRequired),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(TransportError::Io(error)),
        }
        publish_staged_file(path, &destination)?;
        #[cfg(not(windows))]
        sync_published_path(&destination).map_err(TransportError::Io)?;
        Ok(true)
    }

    fn load_detailed_with(
        protector: &dyn Protector,
        path: &Path,
    ) -> Result<PairedStateLoad, TransportError> {
        let mut state: Self = match std::fs::read(path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|_| TransportError::CredentialMalformed)?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(PairedStateLoad {
                    state: Self::default(),
                    relay_token_refused: false,
                });
            }
            Err(e) => return Err(TransportError::Io(e)),
        };
        let mut relay_token_refused = false;
        if let Some(cred) = state.credential.as_mut() {
            unwrap_credential(protector, path, cred, &mut relay_token_refused)?;
        }
        if let Some(intent) = state.retirement_intent.as_mut() {
            unwrap_credential(
                protector,
                path,
                &mut intent.credential,
                &mut relay_token_refused,
            )?;
        }
        Ok(PairedStateLoad {
            state,
            relay_token_refused,
        })
    }

    /// Atomically persist to a JSON file (write-temp-then-rename) with parent directory sync.
    pub fn save(&self, path: &Path) -> Result<(), TransportError> {
        self.save_with(&platform_protector(), path)
    }

    pub(crate) fn save_with(
        &self,
        protector: &dyn Protector,
        path: &Path,
    ) -> Result<(), TransportError> {
        let _guard = owner_state_write_guard();
        match Self::save_inner(protector, path, self) {
            Ok(_) => Ok(()),
            Err(StorageError::Transport(e)) => Err(e),
            Err(StorageError::WriteFailed(e)) => Err(TransportError::Io(e)),
            Err(StorageError::DurabilityUncertain(e)) => Err(TransportError::Io(e)),
            Err(StorageError::Crypto(msg)) => Err(TransportError::Crypto(msg)),
            Err(StorageError::CasMismatch) => {
                Err(TransportError::Io(std::io::Error::other("cas mismatch")))
            }
        }
    }

    fn save_inner(
        protector: &dyn Protector,
        path: &Path,
        state: &PairedState,
    ) -> Result<(), StorageError> {
        let mut state = state.clone();
        if let Some(cred) = state.credential.as_mut() {
            protect_credential(protector, cred)?;
        }
        if let Some(intent) = state.retirement_intent.as_mut() {
            protect_credential(protector, &mut intent.credential)?;
        }
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent).map_err(StorageError::WriteFailed)?;
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(&state)
            .map_err(|e| StorageError::Transport(TransportError::from(e)))?;
        let mut file = std::fs::File::create(&tmp).map_err(StorageError::WriteFailed)?;
        file.write_all(&bytes).map_err(StorageError::WriteFailed)?;
        file.sync_all().map_err(StorageError::WriteFailed)?;
        drop(file);

        #[cfg(test)]
        if FS_FAIL_POINT.with(|f| f.get()) == 4 {
            return Err(StorageError::DurabilityUncertain(std::io::Error::other(
                "simulated crash after durable retirement intent staging",
            )));
        }

        #[cfg(test)]
        if FS_FAIL_POINT.with(|f| f.get()) == 1 {
            let _ = std::fs::remove_file(&tmp);
            return Err(StorageError::WriteFailed(std::io::Error::other(
                "simulated pre-rename write failure",
            )));
        }

        publish_staged_file(&tmp, path)?;

        #[cfg(test)]
        if FS_FAIL_POINT.with(|f| f.get()) == 2 {
            return Err(StorageError::DurabilityUncertain(std::io::Error::other(
                "simulated post-rename dirsync failure",
            )));
        }

        #[cfg(not(windows))]
        sync_published_path(path).map_err(StorageError::DurabilityUncertain)?;
        let readback = std::fs::read(path).map_err(StorageError::DurabilityUncertain)?;
        #[cfg(test)]
        let readback = if FS_FAIL_POINT.with(|f| f.get()) == 3 {
            FS_FAIL_POINT.with(|f| f.set(0));
            Vec::new()
        } else {
            readback
        };
        if readback != bytes {
            return Err(StorageError::DurabilityUncertain(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "pairing state readback mismatch",
            )));
        }
        Ok(())
    }

    /// Persist a generation-bound rejection intent before retiring a current
    /// credential. The current credential remains intact and send-fenced until
    /// the remote result is durably terminal.
    pub(crate) fn install_pair_rejection_intent(
        path: &Path,
        expected_owner_generation: [u8; 32],
        expected_access_generation: u64,
        intent: RetirementIntent,
    ) -> Result<(), StorageError> {
        let _guard = owner_state_write_guard();
        let protector = platform_protector();
        let mut state = Self::load_with(&protector, path)?;
        let incumbent = state.credential.as_ref().ok_or(StorageError::CasMismatch)?;
        if state.retirement_intent.is_some()
            || pairing_generation(&incumbent.client_cert_pem) != expected_owner_generation
            || state.access_mutation_generation != expected_access_generation
            || intent.schema != 1
            || intent.owner_generation != expected_owner_generation
            || intent.access_mutation_generation != expected_access_generation
            || pairing_generation(&intent.credential.client_cert_pem) != expected_owner_generation
        {
            return Err(StorageError::CasMismatch);
        }
        state.retirement_intent = Some(intent);
        Self::save_inner(&protector, path, &state)
    }

    /// Finish a rejected pairing only while the attempted (succeeded or
    /// unknown) intent still owns the exact credential and access generation.
    /// The rejected journal's DELETE is best effort and never blocks this. The answer cleanup runs
    /// under the same owner lock before the credential is cleared.
    pub(crate) fn finish_pair_rejection<F>(
        path: &Path,
        operation_id: &str,
        owner_generation: [u8; 32],
        access_generation: u64,
        clear_answer: F,
    ) -> Result<bool, StorageError>
    where
        F: FnOnce() -> Result<(), StorageError>,
    {
        let _guard = owner_state_write_guard();
        let protector = platform_protector();
        let mut state = Self::load_with(&protector, path)?;
        let Some(intent) = state.retirement_intent.as_ref() else {
            return Ok(false);
        };
        let current_generation = state
            .credential
            .as_ref()
            .map(|credential| pairing_generation(&credential.client_cert_pem));
        if intent.schema != 1
            || intent.operation_id != operation_id
            || intent.owner_generation != owner_generation
            || intent.access_mutation_generation != access_generation
            || !matches!(
                intent.phase,
                RetirementPhase::Succeeded | RetirementPhase::Unknown
            )
            || current_generation != Some(owner_generation)
            || state.access_mutation_generation != access_generation
        {
            return Ok(false);
        }
        clear_answer()?;
        #[cfg(test)]
        if PAIR_REJECTION_CLEANUP_FAIL_POINT.with(|fail| fail.replace(false)) {
            return Err(StorageError::WriteFailed(std::io::Error::other(
                "simulated pair rejection cleanup failure",
            )));
        }
        state.credential = None;
        state.retirement_intent = None;
        state.access_mutation_generation = state.access_mutation_generation.wrapping_add(1);
        Self::save_inner(&protector, path, &state)?;
        Ok(true)
    }

    /// Advance an intent only while it and its exact credential and access
    /// generation still own the durable pairing slot.
    pub(crate) fn update_retirement_phase(
        path: &Path,
        operation_id: &str,
        owner_generation: [u8; 32],
        expected_phase: RetirementPhase,
        phase: RetirementPhase,
    ) -> Result<(), StorageError> {
        let _guard = owner_state_write_guard();
        let protector = platform_protector();
        let mut state = Self::load_with(&protector, path)?;
        let Some(intent) = state.retirement_intent.as_ref() else {
            return Err(StorageError::CasMismatch);
        };
        let owner_matches = state.credential.as_ref().is_some_and(|credential| {
            pairing_generation(&credential.client_cert_pem) == intent.owner_generation
                && state.access_mutation_generation == intent.access_mutation_generation
        });
        if intent.schema != 1
            || intent.operation_id != operation_id
            || intent.owner_generation != owner_generation
            || owner_generation != pairing_generation(&intent.credential.client_cert_pem)
            || !owner_matches
            || intent.phase != expected_phase
        {
            return Err(StorageError::CasMismatch);
        }
        state.retirement_intent.as_mut().unwrap().phase = phase;
        Self::save_inner(&protector, path, &state)
    }

    /// Mutate credential fields within an ordered process-wide CAS boundary.
    pub fn mutate<F>(path: &Path, expected: CasKey, f: F) -> Result<u64, StorageError>
    where
        F: FnOnce(&mut Credential) -> Result<(), StorageError>,
    {
        let _guard = owner_state_write_guard();
        let protector = platform_protector();
        let mut state = Self::load_with(&protector, path)?;
        if state.retirement_intent.is_some() {
            return Err(StorageError::CasMismatch);
        }
        let cred = state.credential.as_mut().ok_or(StorageError::CasMismatch)?;
        let actual_pairing_gen = pairing_generation(&cred.client_cert_pem);
        if actual_pairing_gen != expected.pairing_generation
            || state.access_mutation_generation != expected.access_mutation_generation
        {
            return Err(StorageError::CasMismatch);
        }
        f(cred)?;
        state.access_mutation_generation = state.access_mutation_generation.wrapping_add(1);
        Self::save_inner(&protector, path, &state)?;
        Ok(state.access_mutation_generation)
    }

    pub fn is_paired(&self) -> bool {
        self.credential.is_some()
    }
}

/// Windows durable relay-token publication for the shared transport client.
///
/// Shared refresh owns the publication critical section and cancellation-safe
/// blocking task. This transaction only classifies durable Windows state.
pub(crate) struct WindowsTokenTransaction {
    state_path: Arc<Mutex<Option<PathBuf>>>,
    cas_key: Arc<Mutex<Option<CasKey>>>,
    pairing_generation: [u8; 32],
    relay_fence: Arc<RelayFence>,
    incarnation: u64,
}

impl WindowsTokenTransaction {
    pub(crate) fn new(
        state_path: Arc<Mutex<Option<PathBuf>>>,
        cas_key: Arc<Mutex<Option<CasKey>>>,
        pairing_generation: [u8; 32],
        relay_fence: Arc<RelayFence>,
        incarnation: u64,
    ) -> Self {
        Self {
            state_path,
            cas_key,
            pairing_generation,
            relay_fence,
            incarnation,
        }
    }

    pub(crate) fn set_state_path(&self, path: PathBuf) {
        *self.state_path.lock().unwrap() = Some(path);
    }

    fn unchanged(&self) -> TokenCommit {
        // Shared refresh already owns RelayFence::with_publication. Taking the
        // publication mutex here would self-deadlock; the lifecycle latch is
        // deliberately atomic. Do not advance the incarnation: publication
        // rejection is a relay eligibility fact, not slot retirement.
        self.relay_fence.mark_relay_ineligible(self.incarnation);
        TokenCommit::Unchanged
    }

    fn indeterminate(&self) -> TokenCommit {
        // Every transport for this incarnation must remain disabled when the
        // durable token state cannot be established, including retirement.
        self.relay_fence.mark_relay_ineligible(self.incarnation);
        TokenCommit::Indeterminate
    }

    fn committed(&self, generation: u64) -> TokenCommit {
        *self.cas_key.lock().unwrap() = Some(CasKey {
            pairing_generation: self.pairing_generation,
            access_mutation_generation: generation,
        });
        TokenCommit::Committed { generation }
    }

    fn classify_readback(
        &self,
        path: &Path,
        expected_next_generation: u64,
        token: &str,
        expires_at: i64,
    ) -> TokenCommit {
        let state = match PairedState::load(path) {
            Ok(state) => state,
            Err(_) => return self.indeterminate(),
        };
        let Some(credential) = state.credential else {
            return self.unchanged();
        };
        if pairing_generation(&credential.client_cert_pem) != self.pairing_generation {
            return self.unchanged();
        }
        if state.access_mutation_generation == expected_next_generation
            && credential.device_token.as_deref() == Some(token)
            && credential.device_token_expires_at == Some(expires_at)
        {
            self.committed(expected_next_generation)
        } else {
            self.unchanged()
        }
    }
}

impl TokenTransaction for WindowsTokenTransaction {
    fn commit(&self, ctx: TokenCommitContext<'_>) -> TokenCommit {
        if ctx.incarnation != self.incarnation || !self.relay_fence.allows(self.incarnation) {
            // A held transaction from a replaced client cannot change the
            // current fence or durable state, even if the caller presents a
            // stale publication.
            return TokenCommit::Unchanged;
        }
        let Some(path) = self.state_path.lock().unwrap().clone() else {
            return self.unchanged();
        };
        let state = match PairedState::load(&path) {
            Ok(state) => state,
            Err(_) => return self.indeterminate(),
        };
        let Some(credential) = state.credential else {
            return self.unchanged();
        };
        if pairing_generation(&credential.client_cert_pem) != self.pairing_generation {
            return self.unchanged();
        }

        let current_generation = state.access_mutation_generation;
        let expected_next_generation = current_generation.wrapping_add(1);
        let result = PairedState::mutate(
            &path,
            CasKey {
                pairing_generation: self.pairing_generation,
                access_mutation_generation: current_generation,
            },
            |credential| {
                credential.device_token = Some(ctx.token.to_string());
                credential.device_token_expires_at = Some(ctx.expires_at);
                Ok(())
            },
        );

        match result {
            Ok(generation) if generation == expected_next_generation => self.committed(generation),
            Ok(_) | Err(_) => {
                self.classify_readback(&path, expected_next_generation, ctx.token, ctx.expires_at)
            }
        }
    }
}

/// A freshly-generated device key + the CSR PEM to send to the journal.
pub struct GeneratedKey {
    pub key_pem: String,
    pub csr_pem: String,
    pub public_key_spki_der: Vec<u8>,
}

#[cfg(test)]
pub(crate) fn endpoint_addrs_from_local_endpoints(
    value: Option<&serde_json::Value>,
) -> Vec<EndpointAddr> {
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
        public_key_spki_der: key_pair.public_key_der(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use spl_transport::client::{RelayFence as SharedRelayFence, RelayPermit};
    use std::cell::Cell;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[derive(Clone, Copy)]
    enum TestMode {
        Reversible,
        AlwaysFail,
        CryptoFailure,
        IoFailure,
        FailOnNth(usize),
        IoFailureOnNth(usize),
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
        fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, ProtectionError> {
            Ok(xor_5a(plain))
        }

        fn unprotect(&self, blob: &[u8]) -> Result<Vec<u8>, ProtectionError> {
            let calls = self.unprotect_calls.get() + 1;
            self.unprotect_calls.set(calls);
            match self.mode {
                TestMode::AlwaysFail => Err(ProtectionError::Refused),
                TestMode::CryptoFailure => Err(ProtectionError::Failure(TransportError::Crypto(
                    "fake cryptographic failure".into(),
                ))),
                TestMode::IoFailure => Err(ProtectionError::Failure(TransportError::Io(
                    std::io::Error::new(std::io::ErrorKind::PermissionDenied, "fake denied"),
                ))),
                TestMode::FailOnNth(n) if calls == n => Err(ProtectionError::Refused),
                TestMode::IoFailureOnNth(n) if calls == n => {
                    Err(ProtectionError::Failure(TransportError::Io(
                        std::io::Error::new(std::io::ErrorKind::PermissionDenied, "fake denied"),
                    )))
                }
                TestMode::Reversible | TestMode::FailOnNth(_) | TestMode::IoFailureOnNth(_) => {
                    Ok(xor_5a(blob))
                }
            }
        }
    }

    struct PanicUnprotectProtector;

    impl Protector for PanicUnprotectProtector {
        fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, ProtectionError> {
            Ok(plain.to_vec())
        }

        fn unprotect(&self, _blob: &[u8]) -> Result<Vec<u8>, ProtectionError> {
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
            ..Default::default()
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

    fn token_transaction(
        path: &Path,
        state: &PairedState,
    ) -> (WindowsTokenTransaction, Arc<RelayFence>) {
        state.save(path).unwrap();
        let pairing_generation = pairing_generation(
            &state
                .credential
                .as_ref()
                .expect("paired state fixture has a credential")
                .client_cert_pem,
        );
        let fence = Arc::new(RelayFence::new(true));
        (
            WindowsTokenTransaction::new(
                Arc::new(Mutex::new(Some(path.to_path_buf()))),
                Arc::new(Mutex::new(Some(CasKey {
                    pairing_generation,
                    access_mutation_generation: state.access_mutation_generation,
                }))),
                pairing_generation,
                fence.clone(),
                1,
            ),
            fence,
        )
    }

    fn token_context<'a>(token: &'a str, expires_at: i64) -> TokenCommitContext<'a> {
        TokenCommitContext {
            token,
            expires_at,
            previous_token: "old-token",
            incarnation: 1,
        }
    }

    #[test]
    fn token_transaction_commits_successive_durable_generations() {
        let path = temp_pairing_path("token-transaction");
        let mut state = paired_state_with("KEY", Some("old-token"));
        state.credential.as_mut().unwrap().client_cert_pem = "CERT".into();
        state.save(&path).unwrap();

        let fence = Arc::new(RelayFence::new(true));
        let transaction = WindowsTokenTransaction::new(
            Arc::new(Mutex::new(Some(path.clone()))),
            Arc::new(Mutex::new(Some(CasKey {
                pairing_generation: pairing_generation("CERT"),
                access_mutation_generation: 0,
            }))),
            pairing_generation("CERT"),
            fence,
            1,
        );
        let first = transaction.commit(TokenCommitContext {
            token: "first-token",
            expires_at: 100,
            previous_token: "old-token",
            incarnation: 1,
        });
        let second = transaction.commit(TokenCommitContext {
            token: "second-token",
            expires_at: 200,
            previous_token: "first-token",
            incarnation: 1,
        });

        assert_eq!(first, TokenCommit::Committed { generation: 1 });
        assert_eq!(second, TokenCommit::Committed { generation: 2 });
        let persisted = PairedState::load(&path).unwrap();
        assert_eq!(persisted.access_mutation_generation, 2);
        let credential = persisted.credential.unwrap();
        assert_eq!(credential.device_token.as_deref(), Some("second-token"));
        assert_eq!(credential.device_token_expires_at, Some(200));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn token_transaction_reloads_generation_before_each_commit() {
        let path = temp_pairing_path("token-transaction-fresh-generation");
        let mut state = paired_state_with("KEY", Some("old-token"));
        state.credential.as_mut().unwrap().client_cert_pem = "CERT".into();
        let (transaction, _fence) = token_transaction(&path, &state);

        assert_eq!(
            transaction.commit(token_context("first-token", 100)),
            TokenCommit::Committed { generation: 1 }
        );
        let competing_generation = PairedState::mutate(
            &path,
            CasKey {
                pairing_generation: pairing_generation("CERT"),
                access_mutation_generation: 1,
            },
            |credential| {
                credential.device_token = Some("competing-token".into());
                credential.device_token_expires_at = Some(150);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(competing_generation, 2);

        // The transaction does not reuse the first commit's captured access
        // generation: its durable commit is truthfully generation 3.
        assert_eq!(
            transaction.commit(token_context("second-token", 200)),
            TokenCommit::Committed { generation: 3 }
        );
        let persisted = PairedState::load(&path).unwrap();
        assert_eq!(persisted.access_mutation_generation, 3);
        assert_eq!(
            persisted.credential.unwrap().device_token.as_deref(),
            Some("second-token")
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn token_transaction_rejects_confirmed_stale_pairing_and_latches_relay() {
        let path = temp_pairing_path("token-transaction-stale-pairing");
        let mut state = paired_state_with("KEY", Some("old-token"));
        state.credential.as_mut().unwrap().client_cert_pem = "CERT-OLD".into();
        let (transaction, fence) = token_transaction(&path, &state);

        let mut competing = state;
        let credential = competing.credential.as_mut().unwrap();
        credential.client_cert_pem = "CERT-NEW".into();
        credential.device_token = Some("competing-token".into());
        credential.device_token_expires_at = Some(500);
        competing.access_mutation_generation = 9;
        competing.save(&path).unwrap();

        assert_eq!(
            transaction.commit(token_context("fresh-token", 600)),
            TokenCommit::Unchanged
        );
        assert_eq!(
            SharedRelayFence::permit(fence.as_ref(), 1),
            RelayPermit::Disabled
        );
        let persisted = PairedState::load(&path).unwrap();
        assert_eq!(persisted.access_mutation_generation, 9);
        let persisted = persisted.credential.unwrap();
        assert_eq!(persisted.device_token.as_deref(), Some("competing-token"));
        assert_eq!(persisted.device_token_expires_at, Some(500));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn token_transaction_reconciles_uncertain_write_only_for_exact_readback_tuple() {
        let path = temp_pairing_path("token-transaction-uncertain-exact");
        let mut state = paired_state_with("KEY", Some("old-token"));
        state.credential.as_mut().unwrap().client_cert_pem = "CERT".into();
        let (transaction, _fence) = token_transaction(&path, &state);

        FS_FAIL_POINT.with(|fail_point| fail_point.set(2));
        let committed = transaction.commit(token_context("fresh-token", 100));
        FS_FAIL_POINT.with(|fail_point| fail_point.set(0));
        assert_eq!(committed, TokenCommit::Committed { generation: 1 });

        let exact = PairedState::load(&path).unwrap();
        assert_eq!(exact.access_mutation_generation, 1);
        let exact_credential = exact.credential.unwrap();
        assert_eq!(
            exact_credential.device_token.as_deref(),
            Some("fresh-token")
        );
        assert_eq!(exact_credential.device_token_expires_at, Some(100));

        for (name, generation, cert, expiry) in [
            ("expiry", 1, "CERT", 101),
            ("generation", 2, "CERT", 100),
            ("pairing", 1, "CERT-OTHER", 100),
        ] {
            let mismatch_path = temp_pairing_path(&format!("token-transaction-uncertain-{name}"));
            let mut mismatch = paired_state_with("KEY", Some("fresh-token"));
            let credential = mismatch.credential.as_mut().unwrap();
            credential.client_cert_pem = cert.into();
            credential.device_token_expires_at = Some(expiry);
            mismatch.access_mutation_generation = generation;
            let (mismatch_transaction, mismatch_fence) = token_transaction(&mismatch_path, &state);
            mismatch.save(&mismatch_path).unwrap();

            assert_eq!(
                mismatch_transaction.classify_readback(&mismatch_path, 1, "fresh-token", 100),
                TokenCommit::Unchanged,
                "{name} mismatch must not publish a live token"
            );
            assert_eq!(
                SharedRelayFence::permit(mismatch_fence.as_ref(), 1),
                RelayPermit::Disabled
            );
            let _ = std::fs::remove_dir_all(mismatch_path.parent().unwrap());
        }

        let missing = temp_pairing_path("token-transaction-unresolved-readback");
        let (missing_transaction, missing_fence) = token_transaction(&missing, &state);
        std::fs::write(&missing, b"not durable pairing state").unwrap();
        assert_eq!(
            missing_transaction.classify_readback(&missing, 1, "fresh-token", 100),
            TokenCommit::Indeterminate
        );
        assert_eq!(
            SharedRelayFence::permit(missing_fence.as_ref(), 1),
            RelayPermit::Disabled
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let _ = std::fs::remove_dir_all(missing.parent().unwrap());
    }

    #[test]
    fn token_transaction_rejected_write_keeps_old_state_and_latches_relay() {
        let path = temp_pairing_path("token-transaction-write-failure");
        let mut state = paired_state_with("KEY", Some("old-token"));
        state.credential.as_mut().unwrap().client_cert_pem = "CERT".into();
        let (transaction, fence) = token_transaction(&path, &state);

        FS_FAIL_POINT.with(|fail_point| fail_point.set(1));
        let result = transaction.commit(token_context("fresh-token", 100));
        FS_FAIL_POINT.with(|fail_point| fail_point.set(0));
        assert_eq!(result, TokenCommit::Unchanged);
        assert_eq!(
            SharedRelayFence::permit(fence.as_ref(), 1),
            RelayPermit::Disabled
        );
        let persisted = PairedState::load(&path).unwrap();
        assert_eq!(persisted.access_mutation_generation, 0);
        assert_eq!(
            persisted.credential.unwrap().device_token.as_deref(),
            Some("old-token")
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
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
    fn generated_csr_carries_matching_public_key_spki_der() {
        let g = generate_csr("solstone-windows-test").unwrap();
        assert!(g.csr_pem.contains("BEGIN CERTIFICATE REQUEST"));
        assert!(g.key_pem.contains("BEGIN PRIVATE KEY"));
        let key = KeyPair::from_pem(&g.key_pem).unwrap();
        assert_eq!(g.public_key_spki_der, key.public_key_der());
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
            ..Default::default()
        };
        state.save(&path).unwrap();
        let loaded = PairedState::load(&path).unwrap();
        assert!(loaded.is_paired());
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
        let path = temp_pairing_path("marked-crypto-failure");
        let state = paired_state_with(&format!("{CREDENTIAL_WRAP_MARKER}S0s="), None);
        write_raw_state(&path, &state);

        let result = PairedState::load_with(&TestProtector::new(TestMode::CryptoFailure), &path);
        assert!(matches!(result, Err(TransportError::Crypto(_))));
        assert!(!key_recovery_evidence_path(&path).exists());

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn marked_client_key_refusal_writes_exact_recovery_evidence_before_returning() {
        let path = temp_pairing_path("marked-failure");
        let state = paired_state_with(&format!("{CREDENTIAL_WRAP_MARKER}S0s="), None);
        write_raw_state(&path, &state);
        let protector = TestProtector::new(TestMode::AlwaysFail);

        let result = PairedState::load_with(&protector, &path);
        assert!(matches!(
            result,
            Err(TransportError::ClientKeyProtectionRefused)
        ));
        assert_eq!(
            std::fs::read(key_recovery_evidence_path(&path)).unwrap(),
            b"KK"
        );
        assert!(path.is_file());

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn second_secret_unprotect_failure_returns_error() {
        let path = temp_pairing_path("second-secret-io-failure");
        let state = paired_state_with("ROUNDTRIP-KEY-PEM", Some("ROUNDTRIP-TOKEN"));
        let writer = TestProtector::reversible();
        state.save_with(&writer, &path).unwrap();

        let reader = TestProtector::new(TestMode::IoFailureOnNth(2));
        let result = PairedState::load_with(&reader, &path);
        assert!(matches!(
            result,
            Err(TransportError::Io(error))
                if error.kind() == std::io::ErrorKind::PermissionDenied
        ));
        assert_eq!(reader.unprotect_calls.get(), 2);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn optional_token_refusal_preserves_paired_identity_and_disables_relay_token() {
        let path = temp_pairing_path("second-secret-failure");
        let state = paired_state_with("ROUNDTRIP-KEY-PEM", Some("ROUNDTRIP-TOKEN"));
        let writer = TestProtector::reversible();
        state.save_with(&writer, &path).unwrap();

        let reader = TestProtector::new(TestMode::FailOnNth(2));
        let result = PairedState::load_detailed_with(&reader, &path).unwrap();
        assert!(result.state.is_paired());
        assert!(result.relay_token_refused);
        assert_eq!(result.state.credential.unwrap().device_token, None);
        assert_eq!(reader.unprotect_calls.get(), 2);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn set_aside_moves_only_a_refused_client_key_with_matching_evidence() {
        // Token-only refusal with a valid client key never becomes unpaired.
        let path = temp_pairing_path("set-aside-token-only");
        let state = paired_state_with("ROUNDTRIP-KEY-PEM", Some("ROUNDTRIP-TOKEN"));
        state
            .save_with(&TestProtector::reversible(), &path)
            .unwrap();
        let before = std::fs::read(&path).unwrap();
        let reader = TestProtector::new(TestMode::FailOnNth(2));
        assert!(
            PairedState::load_detailed_with(&reader, &path)
                .unwrap()
                .relay_token_refused
        );
        assert!(!PairedState::set_aside_refused_client_key_with(
            &TestProtector::reversible(),
            &path
        )
        .unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!refused_pairing_path(&path).exists());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        // A refused client key without its durable evidence stays an error.
        let path = temp_pairing_path("set-aside-no-evidence");
        let state = paired_state_with(&format!("{CREDENTIAL_WRAP_MARKER}S0s="), None);
        write_raw_state(&path, &state);
        let refusing = TestProtector::new(TestMode::AlwaysFail);
        assert!(matches!(
            PairedState::set_aside_refused_client_key_with(&refusing, &path),
            Err(TransportError::CredentialRecoveryRequired)
        ));
        assert!(path.is_file());

        // Once the load has persisted the evidence, the pairing is set aside.
        assert!(matches!(
            PairedState::load_with(&refusing, &path),
            Err(TransportError::ClientKeyProtectionRefused)
        ));
        let before = std::fs::read(&path).unwrap();
        assert!(PairedState::set_aside_refused_client_key_with(&refusing, &path).unwrap());
        assert!(!path.exists());
        assert_eq!(std::fs::read(refused_pairing_path(&path)).unwrap(), before);
        assert_eq!(
            std::fs::read(key_recovery_evidence_path(&path)).unwrap(),
            b"KK"
        );
        assert!(PairedState::load_with(&refusing, &path)
            .unwrap()
            .credential
            .is_none());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn refused_key_without_durable_evidence_is_an_explicit_recovery_error() {
        let path = temp_pairing_path("evidence-write-failure");
        let state = paired_state_with(&format!("{CREDENTIAL_WRAP_MARKER}S0s="), None);
        write_raw_state(&path, &state);
        std::fs::create_dir(key_recovery_evidence_path(&path)).unwrap();
        let before = std::fs::read(&path).unwrap();

        let result = PairedState::load_with(&TestProtector::new(TestMode::AlwaysFail), &path);
        assert!(matches!(
            result,
            Err(TransportError::CredentialRecoveryRequired)
        ));
        assert_eq!(std::fs::read(&path).unwrap(), before);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn malformed_protected_key_encoding_is_not_a_protection_refusal() {
        let path = temp_pairing_path("malformed-key-encoding");
        let state = paired_state_with(&format!("{CREDENTIAL_WRAP_MARKER}%%%"), None);
        write_raw_state(&path, &state);

        let result = PairedState::load_with(&TestProtector::new(TestMode::AlwaysFail), &path);
        assert!(matches!(result, Err(TransportError::CredentialMalformed)));
        assert!(!key_recovery_evidence_path(&path).exists());

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn unsupported_protection_wrapper_version_is_malformed() {
        let path = temp_pairing_path("unsupported-key-wrapper");
        let state = paired_state_with("dpapi:v2:opaque", None);
        write_raw_state(&path, &state);
        let result = PairedState::load_with(&TestProtector::new(TestMode::AlwaysFail), &path);
        assert!(matches!(result, Err(TransportError::CredentialMalformed)));
        assert!(!key_recovery_evidence_path(&path).exists());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn protector_permission_failure_is_not_reclassified_as_key_refusal() {
        let path = temp_pairing_path("protection-permission-failure");
        let state = paired_state_with(&format!("{CREDENTIAL_WRAP_MARKER}S0s="), None);
        write_raw_state(&path, &state);

        let result = PairedState::load_with(&TestProtector::new(TestMode::IoFailure), &path);
        assert!(matches!(
            result,
            Err(TransportError::Io(error))
                if error.kind() == std::io::ErrorKind::PermissionDenied
        ));
        assert!(!key_recovery_evidence_path(&path).exists());
        assert!(path.is_file());
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
            ..Default::default()
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
        let normalized = serde_json::to_string(&loaded).unwrap();
        assert!(!normalized.contains("observer_key"));
        assert!(!normalized.contains("observer_name"));
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

    #[test]
    fn test_paired_state_mutate_success() {
        let path = temp_pairing_path("mutate-success");
        let initial_state = paired_state_with("test-key", Some("token-1"));
        initial_state.save(&path).unwrap();

        let cert = &initial_state.credential.as_ref().unwrap().client_cert_pem;
        let p_gen = pairing_generation(cert);
        let cas = CasKey {
            pairing_generation: p_gen,
            access_mutation_generation: 0,
        };

        let new_gen = PairedState::mutate(&path, cas, |cred| {
            cred.device_token = Some("token-2".into());
            Ok(())
        })
        .unwrap();

        assert_eq!(new_gen, 1);
        let loaded = PairedState::load(&path).unwrap();
        assert_eq!(loaded.access_mutation_generation, 1);
        assert_eq!(
            loaded.credential.unwrap().device_token.as_deref(),
            Some("token-2")
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn test_paired_state_mutate_cas_mismatch() {
        let path = temp_pairing_path("mutate-cas-mismatch");
        let initial_state = paired_state_with("test-key", Some("token-1"));
        initial_state.save(&path).unwrap();

        let wrong_cas = CasKey {
            pairing_generation: pairing_generation("OTHER CERT"),
            access_mutation_generation: 0,
        };

        let res = PairedState::mutate(&path, wrong_cas, |cred| {
            cred.device_token = Some("token-2".into());
            Ok(())
        });
        assert!(matches!(res, Err(StorageError::CasMismatch)));

        let wrong_gen_cas = CasKey {
            pairing_generation: pairing_generation(
                &initial_state.credential.as_ref().unwrap().client_cert_pem,
            ),
            access_mutation_generation: 5,
        };
        let res2 = PairedState::mutate(&path, wrong_gen_cas, |cred| {
            cred.device_token = Some("token-2".into());
            Ok(())
        });
        assert!(matches!(res2, Err(StorageError::CasMismatch)));

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn paired_mutations_are_fenced_while_retirement_intent_is_pending() {
        let path = temp_pairing_path("mutate-retirement-pending");
        let mut state = paired_state_with("test-key", Some("token-1"));
        let credential = state.credential.as_ref().unwrap();
        let generation = pairing_generation(&credential.client_cert_pem);
        state.retirement_intent = Some(RetirementIntent {
            schema: 1,
            operation_id: "retirement-op".into(),
            phase: RetirementPhase::Prepared,
            owner_generation: generation,
            access_mutation_generation: 0,
            client_id: "sha256:fixture".into(),
            credential: credential.clone(),
        });
        state.save(&path).unwrap();

        let calls = Cell::new(0);
        let result = PairedState::mutate(
            &path,
            CasKey {
                pairing_generation: generation,
                access_mutation_generation: 0,
            },
            |_| {
                calls.set(calls.get() + 1);
                Ok(())
            },
        );
        assert!(matches!(result, Err(StorageError::CasMismatch)));
        assert_eq!(calls.get(), 0);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn test_paired_state_mutate_fail_points() {
        let path = temp_pairing_path("mutate-fail-points");
        let initial_state = paired_state_with("test-key", Some("token-1"));
        initial_state.save(&path).unwrap();

        let cert = &initial_state.credential.as_ref().unwrap().client_cert_pem;
        let cas = CasKey {
            pairing_generation: pairing_generation(cert),
            access_mutation_generation: 0,
        };

        FS_FAIL_POINT.with(|f| f.set(1));
        let res_wf = PairedState::mutate(&path, cas, |cred| {
            cred.device_token = Some("token-wf".into());
            Ok(())
        });
        assert!(matches!(res_wf, Err(StorageError::WriteFailed(_))));

        FS_FAIL_POINT.with(|f| f.set(2));
        let res_du = PairedState::mutate(&path, cas, |cred| {
            cred.device_token = Some("token-du".into());
            Ok(())
        });
        assert!(matches!(res_du, Err(StorageError::DurabilityUncertain(_))));

        FS_FAIL_POINT.with(|f| f.set(0));

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
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
