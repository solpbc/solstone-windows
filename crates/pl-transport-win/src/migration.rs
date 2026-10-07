// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Durable client-side state for the fresh-pair replacement offer: after a
//! fresh pairing the owner may say this device replaces one of the journal's
//! other paired devices, through the journal's device-migration v1 decision
//! endpoint. The exact decision request is saved before it is sent and is only
//! ever replayed byte-for-byte, so an unanswered decision stays unknown until
//! the journal proves its result.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ack::JournalIdentity;
use crate::answer::{answer_path, read_answer};
use crate::credential::{pairing_generation, Credential, PairedState};
use crate::{ObserverClient, TransportError};

pub const MIGRATION_RECORD_SCHEMA: &str = "solstone.windows-device-migration.v1";

/// The owner's answer to the fresh-pair offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Choice {
    NewDevice,
    ReplaceDevice,
}

impl Choice {
    fn as_wire(self) -> &'static str {
        match self {
            Self::NewDevice => "new_device",
            Self::ReplaceDevice => "replace_device",
        }
    }

    fn expected_state(self) -> ServerState {
        match self {
            Self::NewDevice => ServerState::NewDevice,
            Self::ReplaceDevice => ServerState::ReplacedDevice,
        }
    }
}

/// Journal-reported decision state. The full v1 vocabulary is decoded so a
/// state this client never requests is still parsed, then left unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerState {
    None,
    Pending,
    NewDevice,
    SameDevice,
    ReplacedDevice,
}

impl ServerState {
    fn as_wire(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Pending => "pending",
            Self::NewDevice => "new_device",
            Self::SameDevice => "same_device",
            Self::ReplacedDevice => "replaced_device",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DecisionRequest {
    pub protocol_version: u32,
    pub operation_id: String,
    pub choice: Choice,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces_cid: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationStateResponse {
    pub protocol_version: u32,
    pub rekey_operation_id: Option<String>,
    pub previous_cid: Option<String>,
    pub state: ServerState,
    pub replaced_cid: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionResponse {
    pub protocol_version: u32,
    pub operation_id: String,
    pub state: ServerState,
    pub previous_cid: Option<String>,
    pub cid: String,
    pub replaced_cid: Option<String>,
    pub display_label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationPhase {
    /// The offer is saved and no decision is in flight.
    Offered,
    /// A decision request is saved and its result is not yet known.
    DecisionUnknown,
    /// The journal confirmed the decision.
    Decided,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionReconcile {
    ReplayExactRequest,
    Terminal,
    TargetMissing,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedDecision {
    pub request: DecisionRequest,
    pub request_bytes_base64: String,
    pub result: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationRecord {
    pub schema: String,
    #[serde(default)]
    pub revision: u64,
    /// Generation of the pairing this offer belongs to.
    pub pairing_generation: [u8; 32],
    pub phase: MigrationPhase,
    /// The journal CID of this pairing's client certificate.
    pub cid: String,
    /// Mark-answer binding of this pairing's client certificate.
    pub binding: String,
    pub pending_decision: Option<SavedDecision>,
    #[serde(default)]
    pub terminal_decisions: Vec<SavedDecision>,
    pub server_state: Option<ServerState>,
    pub replaced_cid: Option<String>,
    pub decision_display_label: Option<String>,
    pub offer_pending: bool,
    pub offer_shown: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MigrationView {
    pub phase: MigrationPhase,
    pub revision: u64,
    pub pairing_generation: [u8; 32],
    pub state: Option<ServerState>,
    pub replaced_cid: Option<String>,
    pub decision_choice: Option<Choice>,
    pub decision_result: Option<String>,
    pub offer_available: bool,
    pub offer_binding: Option<String>,
}

pub fn path_for_state(state_path: &Path) -> PathBuf {
    state_path.with_file_name("pairing-migration.json")
}

pub fn load(state_path: &Path) -> Result<Option<MigrationRecord>, TransportError> {
    match std::fs::read(path_for_state(state_path)) {
        Ok(bytes) => {
            let record: MigrationRecord =
                serde_json::from_slice(&bytes).map_err(|_| TransportError::CredentialMalformed)?;
            if record.schema != MIGRATION_RECORD_SCHEMA {
                return Err(TransportError::CredentialMalformed);
            }
            Ok(Some(record))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(TransportError::Io(error)),
    }
}

/// Publish the record only over the exact revision it was read at.
pub fn save(state_path: &Path, record: &mut MigrationRecord) -> Result<(), TransportError> {
    let _guard = crate::credential::owner_state_write_guard();
    if record.schema != MIGRATION_RECORD_SCHEMA {
        return Err(TransportError::CredentialMalformed);
    }
    let path = path_for_state(state_path);
    let current = load(state_path)?;
    let next_revision = match current {
        Some(current) if current.revision == record.revision => current.revision.saturating_add(1),
        None if record.revision == 0 => 1,
        _ => return Err(TransportError::ReplayUnsafe),
    };
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(TransportError::Io)?;
    let mut staged_record = record.clone();
    staged_record.revision = next_revision;
    let bytes = serde_json::to_vec_pretty(&staged_record)?;
    let temp = path.with_extension("json.tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temp)
        .map_err(TransportError::Io)?;
    use std::io::Write;
    file.write_all(&bytes).map_err(TransportError::Io)?;
    file.sync_all().map_err(TransportError::Io)?;
    drop(file);
    if std::fs::read(&temp).map_err(TransportError::Io)? != bytes {
        return Err(TransportError::CredentialRecoveryRequired);
    }
    crate::credential::publish_staged_file(&temp, &path).map_err(TransportError::from)?;
    #[cfg(not(windows))]
    crate::credential::sync_published_path(&path).map_err(TransportError::Io)?;
    if std::fs::read(&path).map_err(TransportError::Io)? != bytes {
        return Err(TransportError::CredentialRecoveryRequired);
    }
    record.revision = next_revision;
    Ok(())
}

/// Return the journal's exact CID for this saved certificate.
pub fn credential_cid(credential: &Credential) -> Result<String, TransportError> {
    let certs = spl_transport::tls::parse_certs(&credential.client_cert_pem)
        .map_err(crate::pairing::map_shared_error)?;
    let cert = certs.first().ok_or(TransportError::CredentialMalformed)?;
    Ok(format!(
        "sha256:{}",
        spl_core::ca::sha256_hex(cert.as_ref())
    ))
}

fn valid_cid(cid: &str) -> bool {
    cid.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// Save the exact decision request before anything is sent. Re-asking the
/// same decision is idempotent; a different one is refused while the saved
/// decision's result is unknown, and allowed only after its target was proven
/// missing.
pub fn persist_decision(
    state_path: &Path,
    record: &mut MigrationRecord,
    choice: Choice,
    replaces_cid: Option<String>,
) -> Result<(), TransportError> {
    if (choice == Choice::ReplaceDevice) != replaces_cid.is_some()
        || replaces_cid.as_deref().is_some_and(|cid| !valid_cid(cid))
        || replaces_cid.as_deref() == Some(record.cid.as_str())
    {
        return Err(TransportError::CredentialMalformed);
    }
    if let Some(saved) = &record.pending_decision {
        if saved.request.choice == choice && saved.request.replaces_cid == replaces_cid {
            return if saved.result == "target_missing" {
                Err(TransportError::ReplayUnsafe)
            } else {
                Ok(())
            };
        }
        if saved.result != "target_missing" {
            return Err(TransportError::ReplayUnsafe);
        }
    }
    if !record.offer_pending
        || record.phase != MigrationPhase::Offered
        || record.server_state.is_some()
        || !valid_cid(&record.cid)
        || record.binding.is_empty()
    {
        return Err(TransportError::ReplayUnsafe);
    }
    if let Some(saved) = record.pending_decision.take() {
        record.terminal_decisions.push(saved);
    }
    let operation_id = decision_operation_id(record, choice, replaces_cid.as_deref());
    let request = DecisionRequest {
        protocol_version: 1,
        operation_id,
        choice,
        replaces_cid,
    };
    let request_bytes = serde_json::to_vec(&request)?;
    record.pending_decision = Some(SavedDecision {
        request,
        request_bytes_base64: base64::engine::general_purpose::STANDARD.encode(request_bytes),
        result: "unknown".to_owned(),
    });
    record.phase = MigrationPhase::DecisionUnknown;
    save(state_path, record)
}

pub fn decision_bytes(saved: &SavedDecision) -> Result<Vec<u8>, TransportError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&saved.request_bytes_base64)
        .map_err(|_| TransportError::CredentialMalformed)?;
    let decoded: DecisionRequest =
        serde_json::from_slice(&bytes).map_err(|_| TransportError::CredentialMalformed)?;
    if decoded != saved.request || decoded.protocol_version != 1 {
        return Err(TransportError::CredentialMalformed);
    }
    Ok(bytes)
}

pub fn mark_decision_terminal(
    state_path: &Path,
    record: &mut MigrationRecord,
    response: &DecisionResponse,
) -> Result<(), TransportError> {
    let saved = record
        .pending_decision
        .as_mut()
        .ok_or(TransportError::CredentialMalformed)?;
    if response.protocol_version != 1
        || response.operation_id != saved.request.operation_id
        || response.state != saved.request.choice.expected_state()
        || response.previous_cid.is_some()
        || response.cid != record.cid
        || !valid_cid(&response.cid)
        || match saved.request.choice {
            Choice::ReplaceDevice => response.replaced_cid != saved.request.replaces_cid,
            Choice::NewDevice => response.replaced_cid.is_some(),
        }
    {
        return Err(TransportError::CredentialMalformed);
    }
    saved.result = response.state.as_wire().to_owned();
    record.server_state = Some(response.state);
    record.replaced_cid = response.replaced_cid.clone();
    record.decision_display_label = Some(response.display_label.clone());
    record.phase = MigrationPhase::Decided;
    record.offer_pending = false;
    save(state_path, record)
}

fn decision_operation_id(
    record: &MigrationRecord,
    choice: Choice,
    replaces_cid: Option<&str>,
) -> String {
    let mut material = b"solstone.windows.device-decision.v1\0".to_vec();
    material.extend_from_slice(&record.pairing_generation);
    material.extend_from_slice(&record.revision.to_be_bytes());
    material.extend_from_slice(choice.as_wire().as_bytes());
    if let Some(cid) = replaces_cid {
        material.extend_from_slice(cid.as_bytes());
    }
    let digest = spl_core::ca::sha256(&material);
    let mut bytes: [u8; 16] = digest[..16].try_into().unwrap();
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}

fn target_missing_reason(status: u16, body: &[u8]) -> bool {
    let value: Value = serde_json::from_slice(body).unwrap_or_default();
    let reason = value
        .get("reason_code")
        .or_else(|| value.get("reason"))
        .and_then(Value::as_str);
    matches!(
        (status, reason),
        (404, Some("paired_device_not_found")) | (409, Some("migration_target_conflict"))
    )
}

fn record_target_missing(
    state_path: &Path,
    record: &mut MigrationRecord,
    status: u16,
    body: &[u8],
) -> Result<bool, TransportError> {
    if !target_missing_reason(status, body) {
        return Ok(false);
    }
    let Some(saved) = record.pending_decision.as_mut() else {
        return Ok(false);
    };
    if saved.request.choice != Choice::ReplaceDevice || saved.result != "unknown" {
        return Ok(false);
    }
    saved.result = "target_missing".to_owned();
    record.phase = MigrationPhase::Offered;
    save(state_path, record)?;
    Ok(true)
}

/// Decide what the journal's migration GET proves about the saved decision.
/// A fresh pair has no earlier lineage, so the GET cannot name the decision's
/// UUID: only an exact replay of the saved request can recover its response.
pub fn reconcile_decision_state(
    record: &MigrationRecord,
    response: &MigrationStateResponse,
) -> Result<DecisionReconcile, TransportError> {
    if response.protocol_version != 1 {
        return Err(TransportError::CredentialMalformed);
    }
    let Some(saved) = record.pending_decision.as_ref() else {
        return Ok(DecisionReconcile::Unknown);
    };
    if saved.result == "target_missing" {
        return Ok(DecisionReconcile::TargetMissing);
    }
    if saved.result != "unknown" {
        return Ok(DecisionReconcile::Terminal);
    }
    let replaced_matches = match saved.request.choice {
        Choice::ReplaceDevice => response.replaced_cid == saved.request.replaces_cid,
        Choice::NewDevice => response.replaced_cid.is_none(),
    };
    if response.rekey_operation_id.is_none()
        && response.previous_cid.is_none()
        && (response.state == ServerState::None
            || (response.state == saved.request.choice.expected_state() && replaced_matches))
    {
        return Ok(DecisionReconcile::ReplayExactRequest);
    }
    Ok(DecisionReconcile::Unknown)
}

pub async fn reconcile_saved_decision(
    state_path: &Path,
    record: &mut MigrationRecord,
    client: &ObserverClient,
) -> Result<DecisionReconcile, TransportError> {
    let Some(saved) = record.pending_decision.as_ref() else {
        return Ok(DecisionReconcile::Unknown);
    };
    if saved.result == "target_missing" {
        return Ok(DecisionReconcile::TargetMissing);
    }
    if saved.result != "unknown" {
        return Ok(DecisionReconcile::Terminal);
    }
    let get = client.get_migration_state().await?;
    if get.status != 200 {
        return Err(TransportError::Rejected {
            status: get.status,
            body: String::from_utf8_lossy(&get.body).into_owned(),
        });
    }
    let server_state: MigrationStateResponse =
        serde_json::from_slice(&get.body).map_err(|_| TransportError::CredentialMalformed)?;
    match reconcile_decision_state(record, &server_state)? {
        DecisionReconcile::ReplayExactRequest => {
            let saved = record
                .pending_decision
                .as_ref()
                .ok_or(TransportError::CredentialMalformed)?;
            let bytes = decision_bytes(saved)?;
            let put = client.put_migration_decision(&bytes).await?;
            if put.status != 200 && put.status != 201 {
                if record_target_missing(state_path, record, put.status, &put.body)? {
                    return Ok(DecisionReconcile::TargetMissing);
                }
                return Err(TransportError::Rejected {
                    status: put.status,
                    body: String::from_utf8_lossy(&put.body).into_owned(),
                });
            }
            let response: DecisionResponse = serde_json::from_slice(&put.body)
                .map_err(|_| TransportError::CredentialMalformed)?;
            mark_decision_terminal(state_path, record, &response)?;
            Ok(DecisionReconcile::Terminal)
        }
        result => Ok(result),
    }
}

/// The owner-facing view. The offer is available only for the current,
/// mark-confirmed pairing it was recorded for, and only until it was answered
/// or dismissed.
pub fn view(state_path: &Path) -> Result<Option<MigrationView>, TransportError> {
    let Some(record) = load(state_path)? else {
        return Ok(None);
    };
    let answer = read_answer(&answer_path(state_path))
        .map_err(TransportError::from)?
        .unwrap_or_default();
    let current_binding = PairedState::load(state_path)?
        .credential
        .map(|credential| JournalIdentity::from_credential(&credential).client_cert_sha256);
    let offer_available = record.offer_pending
        && !record.offer_shown
        && current_binding.as_deref() == Some(record.binding.as_str())
        && answer.rejected.is_empty()
        && !answer.confirmed.is_empty()
        && answer.confirmed == record.binding;
    let decision = record.pending_decision.as_ref();
    Ok(Some(MigrationView {
        phase: record.phase,
        revision: record.revision,
        pairing_generation: record.pairing_generation,
        state: record.server_state,
        replaced_cid: record.replaced_cid,
        decision_choice: decision.map(|decision| decision.request.choice),
        decision_result: decision.map(|decision| decision.result.clone()),
        offer_available,
        offer_binding: Some(record.binding),
    }))
}

/// Record the one-time replacement offer for a freshly paired credential,
/// replacing any record left by an earlier pairing.
pub fn record_fresh_pair_offer(
    state_path: &Path,
    credential: &Credential,
) -> Result<(), TransportError> {
    let previous_revision = load(state_path)?.map_or(0, |record| record.revision);
    let mut record = MigrationRecord {
        schema: MIGRATION_RECORD_SCHEMA.to_owned(),
        revision: previous_revision,
        pairing_generation: pairing_generation(&credential.client_cert_pem),
        phase: MigrationPhase::Offered,
        cid: credential_cid(credential)?,
        binding: JournalIdentity::from_credential(credential).client_cert_sha256,
        pending_decision: None,
        terminal_decisions: Vec::new(),
        server_state: None,
        replaced_cid: None,
        decision_display_label: None,
        offer_pending: true,
        offer_shown: false,
    };
    save(state_path, &mut record)
}

pub fn dismiss_fresh_pair_offer(
    state_path: &Path,
    expected_binding: &str,
    expected_generation: [u8; 32],
    expected_revision: u64,
) -> Result<(), TransportError> {
    let Some(mut record) = load(state_path)? else {
        return Err(TransportError::ReplayUnsafe);
    };
    let paired = PairedState::load(state_path)?;
    let current_binding = paired
        .credential
        .as_ref()
        .map(|credential| JournalIdentity::from_credential(credential).client_cert_sha256);
    if record.revision != expected_revision
        || record.pairing_generation != expected_generation
        || record.binding != expected_binding
        || current_binding.as_deref() != Some(expected_binding)
        || paired.credential.as_ref().is_none_or(|credential| {
            pairing_generation(&credential.client_cert_pem) != expected_generation
        })
    {
        return Err(TransportError::ReplayUnsafe);
    }
    if record.offer_pending {
        record.offer_pending = false;
        record.offer_shown = true;
        save(state_path, &mut record)?;
    } else if !record.offer_shown {
        return Err(TransportError::ReplayUnsafe);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::answer::{write_answer, AnswerState};

    fn temp_state_path(name: &str) -> (PathBuf, PathBuf) {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("{name}-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let state_path = dir.join("pairing.json");
        (dir, state_path)
    }

    fn certified_credential() -> Credential {
        let cert = rcgen::generate_simple_self_signed(vec!["device.test".to_owned()]).unwrap();
        Credential {
            client_key_pem: cert.key_pair.serialize_pem(),
            client_cert_pem: cert.cert.pem(),
            ca_chain_pem: vec!["ca".into()],
            ca_fp_prefix: vec![1, 2, 3, 4],
            instance_id: "instance".into(),
            home_label: "journal".into(),
            endpoints: vec![],
            relay_origin: None,
            device_token: None,
            device_token_expires_at: None,
        }
    }

    fn offered_record(state_path: &Path) -> (Credential, MigrationRecord) {
        let credential = certified_credential();
        record_fresh_pair_offer(state_path, &credential).unwrap();
        (credential, load(state_path).unwrap().unwrap())
    }

    #[test]
    fn fresh_pair_offer_dismissal_is_fenced_to_binding_generation_and_revision() {
        let (dir, state_path) = temp_state_path("migration-offer");
        let credential = certified_credential();
        let binding = JournalIdentity::from_credential(&credential).client_cert_sha256;
        let generation = pairing_generation(&credential.client_cert_pem);
        PairedState {
            credential: Some(credential.clone()),
            ..Default::default()
        }
        .save(&state_path)
        .unwrap();
        record_fresh_pair_offer(&state_path, &credential).unwrap();
        // The offer stays hidden until the owner confirms the journal's mark.
        assert!(!view(&state_path).unwrap().unwrap().offer_available);
        write_answer(
            &answer_path(&state_path),
            &AnswerState {
                confirmed: binding.clone(),
                rejected: String::new(),
            },
        )
        .unwrap();
        let view_before = view(&state_path).unwrap().unwrap();
        assert!(view_before.offer_available);
        assert_eq!(view_before.revision, 1);

        assert!(matches!(
            dismiss_fresh_pair_offer(&state_path, &binding, generation, view_before.revision - 1),
            Err(TransportError::ReplayUnsafe)
        ));
        assert!(matches!(
            dismiss_fresh_pair_offer(
                &state_path,
                &format!("sha256:{}", "c".repeat(64)),
                generation,
                view_before.revision
            ),
            Err(TransportError::ReplayUnsafe)
        ));
        assert!(view(&state_path).unwrap().unwrap().offer_available);

        dismiss_fresh_pair_offer(&state_path, &binding, generation, view_before.revision).unwrap();
        let view_after = view(&state_path).unwrap().unwrap();
        assert!(!view_after.offer_available);
        assert_eq!(view_after.revision, 2);
        dismiss_fresh_pair_offer(&state_path, &binding, generation, view_after.revision).unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    const MIGRATION_SCHEMA_BYTES: &str =
        include_str!("../../../contracts/device-migration/bundle/v1.schema.json");
    const MIGRATION_VECTOR_BYTES: &str =
        include_str!("../../../contracts/device-migration/bundle/v1.vectors.json");
    const MIGRATION_ADOPTION_BYTES: &str =
        include_str!("../../../contracts/device-migration/adoption.json");

    #[test]
    fn pinned_decision_vectors_pass_through_the_typed_codecs() {
        let adoption: Value = serde_json::from_str(MIGRATION_ADOPTION_BYTES).unwrap();
        let pinned = |path: &str| -> String {
            adoption["bundle_files"]
                .as_array()
                .unwrap()
                .iter()
                .find(|file| file["path"] == path)
                .and_then(|file| file["sha256"].as_str())
                .unwrap()
                .to_owned()
        };
        assert_eq!(
            spl_core::ca::sha256_hex(MIGRATION_SCHEMA_BYTES.as_bytes()),
            pinned("v1.schema.json")
        );
        assert_eq!(
            spl_core::ca::sha256_hex(MIGRATION_VECTOR_BYTES.as_bytes()),
            pinned("v1.vectors.json")
        );
        let vectors: Value = serde_json::from_str(MIGRATION_VECTOR_BYTES).unwrap();
        let vectors = &vectors["vectors"];
        for id in adoption["adopted_vector_ids"].as_array().unwrap() {
            assert!(
                !vectors[id.as_str().unwrap()].is_null(),
                "adopted vector {id} is pinned"
            );
        }

        for state in vectors["migration_states"].as_array().unwrap() {
            let decoded: MigrationStateResponse = serde_json::from_value(state.clone()).unwrap();
            assert_eq!(serde_json::to_value(&decoded).unwrap(), *state);
        }
        let replace: DecisionRequest =
            serde_json::from_value(vectors["replace_request"].clone()).unwrap();
        assert_eq!(replace.choice, Choice::ReplaceDevice);
        assert_eq!(
            serde_json::to_value(&replace).unwrap(),
            vectors["replace_request"]
        );
        let decision: DecisionResponse =
            serde_json::from_value(vectors["decision_response"].clone()).unwrap();
        assert_eq!(decision.state, ServerState::ReplacedDevice);
        assert_eq!(
            serde_json::to_value(&decision).unwrap(),
            vectors["decision_response"]
        );

        // Both target-missing reasons are part of the pinned vocabulary, and
        // an ordinary refusal is never mistaken for a missing target.
        let reasons = vectors["reason_codes"].as_array().unwrap();
        for reason in ["paired_device_not_found", "migration_target_conflict"] {
            assert!(reasons.iter().any(|code| code == reason), "{reason}");
        }
        for negative in vectors["negative"].as_array().unwrap() {
            let status = u16::try_from(negative["status"].as_u64().unwrap()).unwrap();
            let body = serde_json::to_vec(&serde_json::json!({
                "reason_code": negative["reason_code"],
            }))
            .unwrap();
            assert!(!target_missing_reason(status, &body));
        }
    }

    #[test]
    fn fresh_pair_offer_replaces_a_stale_record_from_an_earlier_pairing() {
        let (dir, state_path) = temp_state_path("migration-fresh-pair-reset");
        let (_, mut stale) = offered_record(&state_path);
        persist_decision(
            &state_path,
            &mut stale,
            Choice::ReplaceDevice,
            Some(format!("sha256:{}", "b".repeat(64))),
        )
        .unwrap();
        assert_eq!(stale.revision, 2);

        let credential = certified_credential();
        let binding = JournalIdentity::from_credential(&credential).client_cert_sha256;
        record_fresh_pair_offer(&state_path, &credential).unwrap();

        let fresh = load(&state_path).unwrap().unwrap();
        assert_eq!(fresh.revision, 3);
        assert_eq!(fresh.phase, MigrationPhase::Offered);
        assert!(fresh.pending_decision.is_none());
        assert!(fresh.terminal_decisions.is_empty());
        assert_eq!(fresh.cid, credential_cid(&credential).unwrap());
        assert_eq!(
            fresh.pairing_generation,
            pairing_generation(&credential.client_cert_pem)
        );
        assert!(fresh.offer_pending);
        assert!(!fresh.offer_shown);
        assert_eq!(fresh.binding, binding);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn decision_payload_is_durable_and_replayed_exactly_with_an_exact_replacement_cid() {
        let (dir, state_path) = temp_state_path("migration-decision");
        let (_, mut record) = offered_record(&state_path);
        let cid = format!("sha256:{}", "a".repeat(64));
        persist_decision(
            &state_path,
            &mut record,
            Choice::ReplaceDevice,
            Some(cid.clone()),
        )
        .unwrap();
        let saved = record.pending_decision.as_ref().unwrap();
        let bytes = decision_bytes(saved).unwrap();
        assert_eq!(
            serde_json::from_slice::<DecisionRequest>(&bytes)
                .unwrap()
                .replaces_cid,
            Some(cid.clone())
        );
        assert_eq!(saved.result, "unknown");
        assert_eq!(load(&state_path).unwrap().unwrap(), record);
        let request_id = saved.request.operation_id.clone();

        // A GET naming some other lineage proves nothing about this decision.
        let unrelated = MigrationStateResponse {
            protocol_version: 1,
            rekey_operation_id: Some("123e4567-e89b-42d3-a456-426614174001".into()),
            previous_cid: Some(format!("sha256:{}", "d".repeat(64))),
            state: ServerState::ReplacedDevice,
            replaced_cid: Some(cid.clone()),
        };
        assert_eq!(
            reconcile_decision_state(&record, &unrelated).unwrap(),
            DecisionReconcile::Unknown
        );
        assert_eq!(record.pending_decision.as_ref().unwrap().result, "unknown");
        assert!(matches!(
            persist_decision(&state_path, &mut record, Choice::NewDevice, None),
            Err(TransportError::ReplayUnsafe)
        ));
        assert_eq!(
            record
                .pending_decision
                .as_ref()
                .unwrap()
                .request
                .operation_id,
            request_id
        );

        // A response that does not match the saved request is refused.
        let mut wrong = DecisionResponse {
            protocol_version: 1,
            operation_id: request_id.clone(),
            state: ServerState::ReplacedDevice,
            previous_cid: None,
            cid: record.cid.clone(),
            replaced_cid: Some(format!("sha256:{}", "e".repeat(64))),
            display_label: "target".into(),
        };
        assert!(matches!(
            mark_decision_terminal(&state_path, &mut record, &wrong),
            Err(TransportError::CredentialMalformed)
        ));
        wrong.replaced_cid = Some(cid);
        wrong.previous_cid = Some(format!("sha256:{}", "d".repeat(64)));
        assert!(matches!(
            mark_decision_terminal(&state_path, &mut record, &wrong),
            Err(TransportError::CredentialMalformed)
        ));
        assert_eq!(record.pending_decision.as_ref().unwrap().result, "unknown");

        wrong.previous_cid = None;
        mark_decision_terminal(&state_path, &mut record, &wrong).unwrap();
        assert_eq!(
            record.pending_decision.as_ref().unwrap().result,
            "replaced_device"
        );
        assert_eq!(record.phase, MigrationPhase::Decided);
        assert!(!record.offer_pending);
        assert_eq!(
            reconcile_decision_state(&record, &unrelated).unwrap(),
            DecisionReconcile::Terminal
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn fresh_pair_decision_uses_its_own_uuid_and_replays_its_exact_bytes() {
        let (dir, state_path) = temp_state_path("migration-fresh-decision");
        let (credential, mut record) = offered_record(&state_path);
        assert_eq!(record.cid, credential_cid(&credential).unwrap());
        assert_eq!(
            record.binding,
            JournalIdentity::from_credential(&credential).client_cert_sha256
        );
        let own_cid = record.cid.clone();
        assert!(matches!(
            persist_decision(
                &state_path,
                &mut record,
                Choice::ReplaceDevice,
                Some(own_cid.clone())
            ),
            Err(TransportError::CredentialMalformed)
        ));
        assert!(matches!(
            persist_decision(&state_path, &mut record, Choice::NewDevice, Some(own_cid)),
            Err(TransportError::CredentialMalformed)
        ));
        assert!(record.pending_decision.is_none());

        persist_decision(&state_path, &mut record, Choice::NewDevice, None).unwrap();
        let exact_bytes = decision_bytes(record.pending_decision.as_ref().unwrap()).unwrap();
        // Asking the same thing again keeps the saved request.
        persist_decision(&state_path, &mut record, Choice::NewDevice, None).unwrap();
        let no_decision_yet = MigrationStateResponse {
            protocol_version: 1,
            rekey_operation_id: None,
            previous_cid: None,
            state: ServerState::None,
            replaced_cid: None,
        };
        assert_eq!(
            reconcile_decision_state(&record, &no_decision_yet).unwrap(),
            DecisionReconcile::ReplayExactRequest
        );
        let reached = MigrationStateResponse {
            state: ServerState::NewDevice,
            ..no_decision_yet.clone()
        };
        assert_eq!(
            reconcile_decision_state(&record, &reached).unwrap(),
            DecisionReconcile::ReplayExactRequest
        );
        let other_result = MigrationStateResponse {
            state: ServerState::ReplacedDevice,
            replaced_cid: Some(format!("sha256:{}", "f".repeat(64))),
            ..no_decision_yet
        };
        assert_eq!(
            reconcile_decision_state(&record, &other_result).unwrap(),
            DecisionReconcile::Unknown
        );
        let saved = record.pending_decision.as_ref().unwrap();
        assert_eq!(decision_bytes(saved).unwrap(), exact_bytes);
        assert_eq!(saved.request.choice, Choice::NewDevice);
        assert!(saved.request.operation_id.contains('-'));
        assert_eq!(
            decision_bytes(saved).unwrap(),
            serde_json::to_vec(&saved.request).unwrap()
        );
        assert_eq!(saved.result, "unknown");
        assert_eq!(record.phase, MigrationPhase::DecisionUnknown);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn confirmed_stale_target_allows_a_new_durable_replacement_operation() {
        let (dir, state_path) = temp_state_path("migration-target-retry");
        let (_, mut record) = offered_record(&state_path);
        let stale_cid = format!("sha256:{}", "c".repeat(64));
        let replacement_cid = format!("sha256:{}", "e".repeat(64));
        persist_decision(
            &state_path,
            &mut record,
            Choice::ReplaceDevice,
            Some(stale_cid.clone()),
        )
        .unwrap();
        let stale_id = record
            .pending_decision
            .as_ref()
            .unwrap()
            .request
            .operation_id
            .clone();
        assert!(!record_target_missing(
            &state_path,
            &mut record,
            400,
            br#"{"reason_code":"migration_request_invalid"}"#
        )
        .unwrap());
        assert_eq!(record.pending_decision.as_ref().unwrap().result, "unknown");
        let target_not_found = br#"{"reason_code":"paired_device_not_found"}"#;
        assert!(record_target_missing(&state_path, &mut record, 404, target_not_found).unwrap());
        assert_eq!(record.phase, MigrationPhase::Offered);
        assert_eq!(
            record.pending_decision.as_ref().unwrap().result,
            "target_missing"
        );
        assert_eq!(
            load(&state_path)
                .unwrap()
                .unwrap()
                .pending_decision
                .unwrap()
                .result,
            "target_missing"
        );
        // The proven-missing target is never re-sent.
        assert!(matches!(
            persist_decision(
                &state_path,
                &mut record,
                Choice::ReplaceDevice,
                Some(stale_cid)
            ),
            Err(TransportError::ReplayUnsafe)
        ));

        persist_decision(
            &state_path,
            &mut record,
            Choice::ReplaceDevice,
            Some(replacement_cid.clone()),
        )
        .unwrap();
        let current = record.pending_decision.as_ref().unwrap();
        assert_ne!(current.request.operation_id, stale_id);
        assert_eq!(
            current.request.replaces_cid.as_deref(),
            Some(replacement_cid.as_str())
        );
        assert_eq!(
            decision_bytes(current).unwrap(),
            serde_json::to_vec(&current.request).unwrap()
        );
        assert_eq!(record.terminal_decisions.len(), 1);
        assert_eq!(record.terminal_decisions[0].result, "target_missing");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn stale_migration_snapshot_cannot_overwrite_a_newer_durable_revision() {
        let (dir, state_path) = temp_state_path("migration-cas");
        offered_record(&state_path);

        let mut stale = load(&state_path).unwrap().unwrap();
        let mut latest = load(&state_path).unwrap().unwrap();
        latest.offer_pending = false;
        latest.offer_shown = true;
        save(&state_path, &mut latest).unwrap();
        stale.offer_shown = false;

        assert!(matches!(
            save(&state_path, &mut stale),
            Err(TransportError::ReplayUnsafe)
        ));
        let durable = load(&state_path).unwrap().unwrap();
        assert!(durable.offer_shown);
        assert!(!durable.offer_pending);
        assert_eq!(durable.revision, 2);
        let _ = std::fs::remove_dir_all(dir);
    }
}
