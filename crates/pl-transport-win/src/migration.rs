// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Durable client-side state for the fresh-pair replacement offer: after a
//! fresh pairing the owner may say this device replaces one of the journal's
//! other paired devices, through the journal's device-migration v1 decision
//! endpoint. A confirmed fresh pair shows the offer only after `ShowOffer`; an
//! unchecked list hides it (including while a read is in flight), and
//! `NoOtherDevice` retires it with no decision. The exact decision request is
//! saved before it is sent and is only ever replayed byte-for-byte, so an
//! unanswered decision stays unknown until the journal proves its result.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ack::JournalIdentity;
use crate::answer::{answer_path, read_answer};
use crate::credential::{pairing_generation, Credential, PairedState};
use crate::{ObserverClient, TransportError};

pub const MIGRATION_RECORD_SCHEMA: &str = "solstone.windows-device-migration.v1";

/// Classification of a fresh-pair offer's device-replacement eligibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FreshPairEligibility {
    /// Not yet classified. `record_fresh_pair_offer` writes this.
    /// Missing on an older file. Hides the offer.
    #[default]
    Unchecked,
    /// Durable show-offer. A non-empty other-CID list, or an unavailable read.
    /// `offer_pending` stays true.
    ShowOffer,
    /// Durable retirement. The list succeeded and had no other exact CID.
    /// No decision is sent.
    NoOtherDevice,
}

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
    #[serde(default)]
    pub eligibility: FreshPairEligibility,
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
/// mark-confirmed pairing it was recorded for, after `ShowOffer` classification,
/// and only until it was answered or dismissed.
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
    let offer_available = record.eligibility == FreshPairEligibility::ShowOffer
        && record.offer_pending
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
        eligibility: FreshPairEligibility::Unchecked,
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

/// True only when an unresolved fresh-pair offer is ready for other-device classification.
pub fn fresh_pair_offer_needs_list(state_path: &Path) -> Result<bool, TransportError> {
    let Some(record) = load(state_path)? else {
        return Ok(false);
    };
    if !record.offer_pending
        || record.offer_shown
        || record.phase != MigrationPhase::Offered
        || record.pending_decision.is_some()
        || record.server_state.is_some()
        || record.eligibility != FreshPairEligibility::Unchecked
    {
        return Ok(false);
    }
    let Some(answer) = read_answer(&answer_path(state_path)).map_err(TransportError::from)? else {
        return Ok(false);
    };
    if !answer.rejected.is_empty()
        || answer.confirmed.is_empty()
        || answer.confirmed != record.binding
    {
        return Ok(false);
    }
    let paired = PairedState::load(state_path)?;
    let Some(credential) = paired.credential.as_ref() else {
        return Ok(false);
    };
    let current_binding = JournalIdentity::from_credential(credential).client_cert_sha256;
    let current_generation = pairing_generation(&credential.client_cert_pem);
    if current_binding != record.binding || current_generation != record.pairing_generation {
        return Ok(false);
    }
    Ok(true)
}

/// Exclude the current device's own CID from a list of authorized devices.
pub fn without_own_cid(
    devices: Vec<crate::device_metadata::PairedDevice>,
    own_cid: &str,
) -> Vec<crate::device_metadata::PairedDevice> {
    devices
        .into_iter()
        .filter(|device| device.cid != own_cid)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedFreshPair {
    pub binding: String,
    pub pairing_generation: [u8; 32],
    pub revision: u64,
    pub cid: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmittedFreshPairRead {
    /// Slot or on-disk credential is not this admission. Do not list and do not commit.
    Superseded,
    Devices(Vec<crate::device_metadata::PairedDevice>),
    /// Device-list transport failure. ShowOffer only if this admission is still current.
    Unavailable,
}

pub fn credential_matches_admitted_fresh_pair(
    credential: &Credential,
    admitted: &AdmittedFreshPair,
) -> Result<bool, TransportError> {
    if JournalIdentity::from_credential(credential).client_cert_sha256 != admitted.binding {
        return Ok(false);
    }
    if pairing_generation(&credential.client_cert_pem) != admitted.pairing_generation {
        return Ok(false);
    }
    if credential_cid(credential)? != admitted.cid {
        return Ok(false);
    }
    Ok(true)
}

fn apply_classified_offer<W>(
    state_path: &Path,
    expected_revision: u64,
    expected_generation: [u8; 32],
    expected_binding: &str,
    expected_cid: &str,
    target_eligibility: FreshPairEligibility,
    target_offer_pending: bool,
    commit: W,
) -> Result<(), TransportError>
where
    W: FnOnce(&mut MigrationRecord) -> Result<(), TransportError>,
{
    if !fresh_pair_offer_needs_list(state_path)? {
        return Ok(());
    }
    let Some(mut record) = load(state_path)? else {
        return Ok(());
    };
    if record.revision != expected_revision
        || record.pairing_generation != expected_generation
        || record.binding != expected_binding
        || record.cid != expected_cid
    {
        return Ok(());
    }

    record.eligibility = target_eligibility;
    record.offer_pending = target_offer_pending;
    match commit(&mut record) {
        Ok(()) => Ok(()),
        Err(TransportError::ReplayUnsafe) => Ok(()),
        Err(error) => Err(error),
    }
}

/// Classify a fresh-pair offer's replacement eligibility based on authorized journal devices.
pub async fn classify_fresh_pair_offer<L, Fut, W>(
    state_path: &Path,
    list_other_devices: L,
    commit: W,
) -> Result<(), TransportError>
where
    L: FnOnce() -> Fut,
    Fut: std::future::Future<
        Output = Result<Vec<crate::device_metadata::PairedDevice>, TransportError>,
    >,
    W: FnOnce(&mut MigrationRecord) -> Result<(), TransportError>,
{
    if !fresh_pair_offer_needs_list(state_path)? {
        return Ok(());
    }
    let Some(pre_snapshot) = load(state_path)? else {
        return Ok(());
    };
    let expected_revision = pre_snapshot.revision;
    let expected_generation = pre_snapshot.pairing_generation;
    let expected_binding = pre_snapshot.binding;
    let expected_cid = pre_snapshot.cid;

    let list_result = list_other_devices().await;
    let (target_eligibility, target_offer_pending) = match list_result {
        Err(_) => (FreshPairEligibility::ShowOffer, true),
        Ok(devices) if devices.is_empty() => (FreshPairEligibility::NoOtherDevice, false),
        Ok(_) => (FreshPairEligibility::ShowOffer, true),
    };
    apply_classified_offer(
        state_path,
        expected_revision,
        expected_generation,
        &expected_binding,
        &expected_cid,
        target_eligibility,
        target_offer_pending,
        commit,
    )
}

/// Classify a fresh-pair offer's replacement eligibility using an admission-fenced preparation seam.
pub async fn classify_admitted_fresh_pair_offer<P, Fut, W>(
    state_path: &Path,
    prepare: P,
    commit: W,
) -> Result<(), TransportError>
where
    P: FnOnce(&AdmittedFreshPair) -> Fut,
    Fut: std::future::Future<Output = Result<AdmittedFreshPairRead, TransportError>>,
    W: FnOnce(&mut MigrationRecord) -> Result<(), TransportError>,
{
    if !fresh_pair_offer_needs_list(state_path)? {
        return Ok(());
    }
    let Some(record) = load(state_path)? else {
        return Ok(());
    };
    let admitted = AdmittedFreshPair {
        binding: record.binding,
        pairing_generation: record.pairing_generation,
        revision: record.revision,
        cid: record.cid,
    };

    let read = prepare(&admitted).await?;
    let (target_eligibility, target_offer_pending) = match read {
        AdmittedFreshPairRead::Superseded => return Ok(()),
        AdmittedFreshPairRead::Unavailable => (FreshPairEligibility::ShowOffer, true),
        AdmittedFreshPairRead::Devices(devices) => {
            let remaining = without_own_cid(devices, &admitted.cid);
            if remaining.is_empty() {
                (FreshPairEligibility::NoOtherDevice, false)
            } else {
                (FreshPairEligibility::ShowOffer, true)
            }
        }
    };

    apply_classified_offer(
        state_path,
        admitted.revision,
        admitted.pairing_generation,
        &admitted.binding,
        &admitted.cid,
        target_eligibility,
        target_offer_pending,
        commit,
    )
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
        // Before answer, the offer is hidden.
        assert!(!view(&state_path).unwrap().unwrap().offer_available);
        write_answer(
            &answer_path(&state_path),
            &AnswerState {
                confirmed: binding.clone(),
                rejected: String::new(),
            },
        )
        .unwrap();
        // After confirmed answer, while Unchecked, the offer is still hidden and phase is Offered.
        let view_unchecked = view(&state_path).unwrap().unwrap();
        assert!(!view_unchecked.offer_available);
        assert_eq!(view_unchecked.phase, MigrationPhase::Offered);

        let mut record = load(&state_path).unwrap().unwrap();
        record.eligibility = FreshPairEligibility::ShowOffer;
        save(&state_path, &mut record).unwrap();

        let view_before = view(&state_path).unwrap().unwrap();
        assert!(view_before.offer_available);
        assert_eq!(view_before.revision, 2);

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
        assert_eq!(view_after.revision, 3);
        let dismissed_record = load(&state_path).unwrap().unwrap();
        assert!(dismissed_record.offer_shown);
        assert!(!dismissed_record.offer_pending);
        assert!(dismissed_record.pending_decision.is_none());
        assert!(dismissed_record.server_state.is_none());

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
        assert_eq!(fresh.eligibility, FreshPairEligibility::Unchecked);
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

    #[tokio::test]
    async fn classify_confirmed_empty_list_retires_offer_with_no_other_device() {
        let (dir, state_path) = temp_state_path("migration-classify-empty");
        let credential = certified_credential();
        let binding = JournalIdentity::from_credential(&credential).client_cert_sha256;
        let own_cid = credential_cid(&credential).unwrap();
        PairedState {
            credential: Some(credential.clone()),
            ..Default::default()
        }
        .save(&state_path)
        .unwrap();
        record_fresh_pair_offer(&state_path, &credential).unwrap();
        write_answer(
            &answer_path(&state_path),
            &AnswerState {
                confirmed: binding.clone(),
                rejected: String::new(),
            },
        )
        .unwrap();

        let json = serde_json::json!({
            "clients": [
                {"cid": own_cid.clone(), "display_label": "This PC", "platform": "windows"}
            ]
        });
        let parsed =
            crate::device_metadata::parse_paired_devices(&serde_json::to_vec(&json).unwrap())
                .unwrap();
        let filtered = without_own_cid(parsed, &own_cid);
        assert!(filtered.is_empty());

        classify_fresh_pair_offer(
            &state_path,
            || async { Ok(filtered) },
            |record| save(&state_path, record),
        )
        .await
        .unwrap();

        let record = load(&state_path).unwrap().unwrap();
        assert_eq!(record.eligibility, FreshPairEligibility::NoOtherDevice);
        assert!(!record.offer_pending);
        assert!(!record.offer_shown);
        assert!(record.server_state.is_none());
        assert!(record.pending_decision.is_none());
        assert_eq!(record.phase, MigrationPhase::Offered);
        assert_eq!(record.revision, 2);
        assert!(!view(&state_path).unwrap().unwrap().offer_available);

        // Relaunch case: a second classify is a no-op even if a device would appear.
        classify_fresh_pair_offer(
            &state_path,
            || async { panic!("should not be called on retired offer") },
            |record| save(&state_path, record),
        )
        .await
        .unwrap();

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn classify_confirmed_non_empty_list_shows_offer_and_persists_decision_or_dismisses() {
        let (dir, state_path) = temp_state_path("migration-classify-show");
        let credential = certified_credential();
        let binding = JournalIdentity::from_credential(&credential).client_cert_sha256;
        let other_cid = format!("sha256:{}", "b".repeat(64));
        PairedState {
            credential: Some(credential.clone()),
            ..Default::default()
        }
        .save(&state_path)
        .unwrap();
        record_fresh_pair_offer(&state_path, &credential).unwrap();
        write_answer(
            &answer_path(&state_path),
            &AnswerState {
                confirmed: binding.clone(),
                rejected: String::new(),
            },
        )
        .unwrap();

        classify_fresh_pair_offer(
            &state_path,
            || async {
                Ok(vec![crate::device_metadata::PairedDevice {
                    cid: other_cid.clone(),
                    display_label: "Other Device".into(),
                }])
            },
            |record| save(&state_path, record),
        )
        .await
        .unwrap();

        let mut record = load(&state_path).unwrap().unwrap();
        assert_eq!(record.eligibility, FreshPairEligibility::ShowOffer);
        assert!(record.offer_pending);
        assert!(!record.offer_shown);
        assert_eq!(record.phase, MigrationPhase::Offered);
        assert!(view(&state_path).unwrap().unwrap().offer_available);

        persist_decision(&state_path, &mut record, Choice::NewDevice, None).unwrap();
        assert_eq!(record.phase, MigrationPhase::DecisionUnknown);

        // Separate case: ShowOffer followed by dismissal
        let (dir2, state_path2) = temp_state_path("migration-classify-show-dismiss");
        let credential2 = certified_credential();
        let binding2 = JournalIdentity::from_credential(&credential2).client_cert_sha256;
        let generation2 = pairing_generation(&credential2.client_cert_pem);
        PairedState {
            credential: Some(credential2.clone()),
            ..Default::default()
        }
        .save(&state_path2)
        .unwrap();
        record_fresh_pair_offer(&state_path2, &credential2).unwrap();
        write_answer(
            &answer_path(&state_path2),
            &AnswerState {
                confirmed: binding2.clone(),
                rejected: String::new(),
            },
        )
        .unwrap();
        classify_fresh_pair_offer(
            &state_path2,
            || async {
                Ok(vec![crate::device_metadata::PairedDevice {
                    cid: other_cid.clone(),
                    display_label: "Other Device".into(),
                }])
            },
            |record| save(&state_path2, record),
        )
        .await
        .unwrap();
        let view2 = view(&state_path2).unwrap().unwrap();
        assert!(view2.offer_available);
        dismiss_fresh_pair_offer(&state_path2, &binding2, generation2, view2.revision).unwrap();
        let dismissed = load(&state_path2).unwrap().unwrap();
        assert!(!dismissed.offer_pending);
        assert!(dismissed.offer_shown);
        assert!(dismissed.pending_decision.is_none());
        assert!(dismissed.server_state.is_none());
        assert!(!view(&state_path2).unwrap().unwrap().offer_available);

        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(dir2);
    }

    #[tokio::test]
    async fn classify_after_show_offer_does_not_call_list_again() {
        let (dir, state_path) = temp_state_path("migration-classify-idempotent");
        let credential = certified_credential();
        let binding = JournalIdentity::from_credential(&credential).client_cert_sha256;
        PairedState {
            credential: Some(credential.clone()),
            ..Default::default()
        }
        .save(&state_path)
        .unwrap();
        record_fresh_pair_offer(&state_path, &credential).unwrap();
        write_answer(
            &answer_path(&state_path),
            &AnswerState {
                confirmed: binding.clone(),
                rejected: String::new(),
            },
        )
        .unwrap();

        let mut record = load(&state_path).unwrap().unwrap();
        record.eligibility = FreshPairEligibility::ShowOffer;
        save(&state_path, &mut record).unwrap();

        classify_fresh_pair_offer(
            &state_path,
            || async { panic!("must not call list when already ShowOffer") },
            |record| save(&state_path, record),
        )
        .await
        .unwrap();

        let record = load(&state_path).unwrap().unwrap();
        assert_eq!(record.eligibility, FreshPairEligibility::ShowOffer);
        assert!(record.offer_pending);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn classify_list_errors_save_show_offer_and_return_ok() {
        let error_cases = [
            TransportError::Rejected {
                status: 503,
                body: "server error".into(),
            },
            TransportError::CredentialMalformed,
            TransportError::Io(std::io::Error::other("offline")),
        ];

        for error in error_cases {
            let (dir, state_path) = temp_state_path("migration-classify-err");
            let credential = certified_credential();
            let binding = JournalIdentity::from_credential(&credential).client_cert_sha256;
            PairedState {
                credential: Some(credential.clone()),
                ..Default::default()
            }
            .save(&state_path)
            .unwrap();
            record_fresh_pair_offer(&state_path, &credential).unwrap();
            write_answer(
                &answer_path(&state_path),
                &AnswerState {
                    confirmed: binding.clone(),
                    rejected: String::new(),
                },
            )
            .unwrap();

            let err_to_return = match &error {
                TransportError::Rejected { status, body } => TransportError::Rejected {
                    status: *status,
                    body: body.clone(),
                },
                TransportError::CredentialMalformed => TransportError::CredentialMalformed,
                TransportError::Io(_) => TransportError::Io(std::io::Error::other("offline")),
                _ => unreachable!(),
            };

            classify_fresh_pair_offer(
                &state_path,
                || async move { Err(err_to_return) },
                |record| save(&state_path, record),
            )
            .await
            .unwrap();

            let record = load(&state_path).unwrap().unwrap();
            assert_eq!(record.eligibility, FreshPairEligibility::ShowOffer);
            assert!(record.offer_pending);
            assert_eq!(record.phase, MigrationPhase::Offered);
            assert!(view(&state_path).unwrap().unwrap().offer_available);

            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[tokio::test]
    async fn classify_commit_errors_leave_disk_unchecked() {
        let (dir, state_path) = temp_state_path("migration-classify-commit-err");
        let credential = certified_credential();
        let binding = JournalIdentity::from_credential(&credential).client_cert_sha256;
        PairedState {
            credential: Some(credential.clone()),
            ..Default::default()
        }
        .save(&state_path)
        .unwrap();
        record_fresh_pair_offer(&state_path, &credential).unwrap();
        write_answer(
            &answer_path(&state_path),
            &AnswerState {
                confirmed: binding.clone(),
                rejected: String::new(),
            },
        )
        .unwrap();

        // 1. Commit returns TransportError::Io -> returns error, disk stays Unchecked.
        let err = classify_fresh_pair_offer(
            &state_path,
            || async { Ok(vec![]) },
            |_record| Err(TransportError::Io(std::io::Error::other("injected"))),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, TransportError::Io(_)));

        let record = load(&state_path).unwrap().unwrap();
        assert_eq!(record.eligibility, FreshPairEligibility::Unchecked);
        assert!(record.offer_pending);
        assert_eq!(record.phase, MigrationPhase::Offered);
        assert!(record.pending_decision.is_none());
        assert!(!view(&state_path).unwrap().unwrap().offer_available);

        // 2. Commit returns TransportError::ReplayUnsafe -> returns Ok(()), disk stays Unchecked.
        classify_fresh_pair_offer(
            &state_path,
            || async { Ok(vec![]) },
            |_record| Err(TransportError::ReplayUnsafe),
        )
        .await
        .unwrap();

        let record = load(&state_path).unwrap().unwrap();
        assert_eq!(record.eligibility, FreshPairEligibility::Unchecked);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn classify_no_migration_file_or_legacy_or_dismissed_json() {
        let (dir, state_path) = temp_state_path("migration-classify-legacy");
        let credential = certified_credential();
        let binding = JournalIdentity::from_credential(&credential).client_cert_sha256;
        PairedState {
            credential: Some(credential.clone()),
            ..Default::default()
        }
        .save(&state_path)
        .unwrap();
        write_answer(
            &answer_path(&state_path),
            &AnswerState {
                confirmed: binding.clone(),
                rejected: String::new(),
            },
        )
        .unwrap();

        // No migration file on disk: classify returns Ok(()) and creates no file.
        classify_fresh_pair_offer(
            &state_path,
            || async { panic!("must not call list when no migration file") },
            |record| save(&state_path, record),
        )
        .await
        .unwrap();
        assert!(load(&state_path).unwrap().is_none());

        // Dismissed record with eligibility removed from JSON loads as Unchecked and is not classified.
        let legacy_dismissed = serde_json::json!({
            "schema": MIGRATION_RECORD_SCHEMA,
            "revision": 2,
            "pairing_generation": pairing_generation(&credential.client_cert_pem),
            "phase": "offered",
            "cid": credential_cid(&credential).unwrap(),
            "binding": binding.clone(),
            "pending_decision": null,
            "terminal_decisions": [],
            "server_state": null,
            "replaced_cid": null,
            "decision_display_label": null,
            "offer_pending": false,
            "offer_shown": true
        });
        std::fs::write(
            path_for_state(&state_path),
            serde_json::to_vec(&legacy_dismissed).unwrap(),
        )
        .unwrap();
        let loaded = load(&state_path).unwrap().unwrap();
        assert_eq!(loaded.eligibility, FreshPairEligibility::Unchecked);
        classify_fresh_pair_offer(
            &state_path,
            || async { panic!("must not call list for dismissed record") },
            |record| save(&state_path, record),
        )
        .await
        .unwrap();

        // Unresolved offer with eligibility removed loads as Unchecked and is eligible for list read.
        let legacy_unresolved = serde_json::json!({
            "schema": MIGRATION_RECORD_SCHEMA,
            "revision": 3,
            "pairing_generation": pairing_generation(&credential.client_cert_pem),
            "phase": "offered",
            "cid": credential_cid(&credential).unwrap(),
            "binding": binding.clone(),
            "pending_decision": null,
            "terminal_decisions": [],
            "server_state": null,
            "replaced_cid": null,
            "decision_display_label": null,
            "offer_pending": true,
            "offer_shown": false
        });
        std::fs::write(
            path_for_state(&state_path),
            serde_json::to_vec(&legacy_unresolved).unwrap(),
        )
        .unwrap();
        assert!(fresh_pair_offer_needs_list(&state_path).unwrap());
        classify_fresh_pair_offer(
            &state_path,
            || async { Ok(vec![]) },
            |record| save(&state_path, record),
        )
        .await
        .unwrap();
        let classified = load(&state_path).unwrap().unwrap();
        assert_eq!(classified.eligibility, FreshPairEligibility::NoOtherDevice);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn classify_does_not_hold_write_guard_during_list_await() {
        // Helper to run a suspended classification and assert view is hidden while waiting on list future
        async fn run_suspended_classification(
            name: &str,
            check_guard: bool,
            release_result: Result<Vec<crate::device_metadata::PairedDevice>, TransportError>,
        ) -> (PathBuf, PathBuf) {
            let (dir, state_path) = temp_state_path(name);
            let credential = certified_credential();
            let binding = JournalIdentity::from_credential(&credential).client_cert_sha256;
            PairedState {
                credential: Some(credential.clone()),
                ..Default::default()
            }
            .save(&state_path)
            .unwrap();
            record_fresh_pair_offer(&state_path, &credential).unwrap();
            write_answer(
                &answer_path(&state_path),
                &AnswerState {
                    confirmed: binding.clone(),
                    rejected: String::new(),
                },
            )
            .unwrap();

            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = tokio::sync::oneshot::channel();
            let state_path_clone = state_path.clone();
            let task = tokio::spawn(async move {
                classify_fresh_pair_offer(
                    &state_path_clone,
                    || async move {
                        started_tx.send(()).unwrap();
                        release_rx.await.unwrap()
                    },
                    |record| save(&state_path_clone, record),
                )
                .await
            });

            // Wait until task is inside list closure
            started_rx.await.unwrap();

            // Assert view while list is pending: offer_available false, phase Offered, decision_result None.
            let view_while_pending = view(&state_path).unwrap().unwrap();
            assert!(!view_while_pending.offer_available);
            assert_eq!(view_while_pending.phase, MigrationPhase::Offered);
            assert!(view_while_pending.decision_result.is_none());

            if check_guard {
                let start = std::time::Instant::now();
                let mut acquired = false;
                while start.elapsed() < std::time::Duration::from_secs(2) {
                    if let Some(_guard) = crate::credential::try_owner_state_write_guard() {
                        acquired = true;
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                assert!(
                    acquired,
                    "must not hold owner_state_write_guard during list await"
                );
            }

            release_tx.send(release_result).unwrap();
            task.await.unwrap().unwrap();
            (dir, state_path)
        }

        // 1. Release Ok(vec![]) -> NoOtherDevice
        {
            let (dir, state_path) =
                run_suspended_classification("migration-lock-empty", true, Ok(vec![])).await;
            let record = load(&state_path).unwrap().unwrap();
            assert_eq!(record.eligibility, FreshPairEligibility::NoOtherDevice);
            assert!(!record.offer_pending);
            assert!(!record.offer_shown);
            assert!(record.server_state.is_none());
            assert!(record.pending_decision.is_none());
            assert_eq!(record.phase, MigrationPhase::Offered);
            assert!(!view(&state_path).unwrap().unwrap().offer_available);
            let _ = std::fs::remove_dir_all(dir);
        }

        // 2. Release Ok(vec![one other]) -> ShowOffer
        {
            let other_device = crate::device_metadata::PairedDevice {
                cid: format!("sha256:{}", "b".repeat(64)),
                display_label: "Other Device".into(),
            };
            let (dir, state_path) =
                run_suspended_classification("migration-lock-other", false, Ok(vec![other_device]))
                    .await;
            let record = load(&state_path).unwrap().unwrap();
            assert_eq!(record.eligibility, FreshPairEligibility::ShowOffer);
            assert!(record.offer_pending);
            assert!(!record.offer_shown);
            assert_eq!(record.phase, MigrationPhase::Offered);
            assert!(view(&state_path).unwrap().unwrap().offer_available);
            let _ = std::fs::remove_dir_all(dir);
        }

        // 3. Release Err(Rejected) -> ShowOffer (not NoOtherDevice)
        {
            let err = TransportError::Rejected {
                status: 503,
                body: "unavailable".into(),
            };
            let (dir, state_path) =
                run_suspended_classification("migration-lock-err", false, Err(err)).await;
            let record = load(&state_path).unwrap().unwrap();
            assert_eq!(record.eligibility, FreshPairEligibility::ShowOffer);
            assert_ne!(record.eligibility, FreshPairEligibility::NoOtherDevice);
            assert!(record.offer_pending);
            assert!(!record.offer_shown);
            assert_eq!(record.phase, MigrationPhase::Offered);
            assert!(view(&state_path).unwrap().unwrap().offer_available);
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[tokio::test]
    async fn classify_parsed_devices_with_extra_platform_field_and_other_cid_shows_offer() {
        let (dir, state_path) = temp_state_path("migration-classify-platform-extra");
        let credential = certified_credential();
        let binding = JournalIdentity::from_credential(&credential).client_cert_sha256;
        let own_cid = credential_cid(&credential).unwrap();
        let other_cid = format!("sha256:{}", "c".repeat(64));
        PairedState {
            credential: Some(credential.clone()),
            ..Default::default()
        }
        .save(&state_path)
        .unwrap();
        record_fresh_pair_offer(&state_path, &credential).unwrap();
        write_answer(
            &answer_path(&state_path),
            &AnswerState {
                confirmed: binding.clone(),
                rejected: String::new(),
            },
        )
        .unwrap();

        let json = serde_json::json!({
            "clients": [
                {"cid": own_cid.clone(), "display_label": "This PC", "platform": "windows"},
                {"cid": other_cid.clone(), "display_label": "Other PC", "platform": "linux"}
            ]
        });
        let parsed =
            crate::device_metadata::parse_paired_devices(&serde_json::to_vec(&json).unwrap())
                .unwrap();
        let filtered = without_own_cid(parsed, &own_cid);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].cid, other_cid);

        classify_fresh_pair_offer(
            &state_path,
            || async { Ok(filtered) },
            |record| save(&state_path, record),
        )
        .await
        .unwrap();

        let record = load(&state_path).unwrap().unwrap();
        assert_eq!(record.eligibility, FreshPairEligibility::ShowOffer);
        assert!(record.offer_pending);
        assert!(!record.offer_shown);
        assert_eq!(record.phase, MigrationPhase::Offered);
        assert!(view(&state_path).unwrap().unwrap().offer_available);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn classify_concurrent_change_drops_result_without_committing() {
        // (a) dismiss_fresh_pair_offer on that offer
        {
            let (dir, state_path) = temp_state_path("migration-concurrent-dismiss");
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
            write_answer(
                &answer_path(&state_path),
                &AnswerState {
                    confirmed: binding.clone(),
                    rejected: String::new(),
                },
            )
            .unwrap();

            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let (tx, rx) = tokio::sync::oneshot::channel();
            let state_path_clone = state_path.clone();
            let task = tokio::spawn(async move {
                classify_fresh_pair_offer(
                    &state_path_clone,
                    || async move {
                        started_tx.send(()).unwrap();
                        rx.await.unwrap()
                    },
                    |record| save(&state_path_clone, record),
                )
                .await
            });

            started_rx.await.unwrap();
            dismiss_fresh_pair_offer(&state_path, &binding, generation, 1).unwrap();
            tx.send(Ok(vec![])).unwrap();
            task.await.unwrap().unwrap();

            let record = load(&state_path).unwrap().unwrap();
            assert_eq!(record.eligibility, FreshPairEligibility::Unchecked);
            assert!(record.offer_shown);
            assert!(!record.offer_pending);
            let _ = std::fs::remove_dir_all(dir);
        }

        // (b) persist_decision (phase becomes DecisionUnknown)
        {
            let (dir, state_path) = temp_state_path("migration-concurrent-decide");
            let credential = certified_credential();
            let binding = JournalIdentity::from_credential(&credential).client_cert_sha256;
            PairedState {
                credential: Some(credential.clone()),
                ..Default::default()
            }
            .save(&state_path)
            .unwrap();
            record_fresh_pair_offer(&state_path, &credential).unwrap();
            write_answer(
                &answer_path(&state_path),
                &AnswerState {
                    confirmed: binding.clone(),
                    rejected: String::new(),
                },
            )
            .unwrap();

            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let (tx, rx) = tokio::sync::oneshot::channel();
            let state_path_clone = state_path.clone();
            let task = tokio::spawn(async move {
                classify_fresh_pair_offer(
                    &state_path_clone,
                    || async move {
                        started_tx.send(()).unwrap();
                        rx.await.unwrap()
                    },
                    |record| save(&state_path_clone, record),
                )
                .await
            });

            started_rx.await.unwrap();
            let mut record = load(&state_path).unwrap().unwrap();
            persist_decision(&state_path, &mut record, Choice::NewDevice, None).unwrap();
            tx.send(Ok(vec![])).unwrap();
            task.await.unwrap().unwrap();

            let record = load(&state_path).unwrap().unwrap();
            assert_eq!(record.eligibility, FreshPairEligibility::Unchecked);
            assert_eq!(record.phase, MigrationPhase::DecisionUnknown);
            let _ = std::fs::remove_dir_all(dir);
        }

        // (c) answer.rejected set to the binding
        {
            let (dir, state_path) = temp_state_path("migration-concurrent-reject");
            let credential = certified_credential();
            let binding = JournalIdentity::from_credential(&credential).client_cert_sha256;
            PairedState {
                credential: Some(credential.clone()),
                ..Default::default()
            }
            .save(&state_path)
            .unwrap();
            record_fresh_pair_offer(&state_path, &credential).unwrap();
            write_answer(
                &answer_path(&state_path),
                &AnswerState {
                    confirmed: binding.clone(),
                    rejected: String::new(),
                },
            )
            .unwrap();

            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let (tx, rx) = tokio::sync::oneshot::channel();
            let state_path_clone = state_path.clone();
            let task = tokio::spawn(async move {
                classify_fresh_pair_offer(
                    &state_path_clone,
                    || async move {
                        started_tx.send(()).unwrap();
                        rx.await.unwrap()
                    },
                    |record| save(&state_path_clone, record),
                )
                .await
            });

            started_rx.await.unwrap();
            write_answer(
                &answer_path(&state_path),
                &AnswerState {
                    confirmed: String::new(),
                    rejected: binding.clone(),
                },
            )
            .unwrap();
            tx.send(Ok(vec![])).unwrap();
            task.await.unwrap().unwrap();

            let record = load(&state_path).unwrap().unwrap();
            assert_eq!(record.eligibility, FreshPairEligibility::Unchecked);
            let _ = std::fs::remove_dir_all(dir);
        }

        // (d) record_fresh_pair_offer for a different credential saved as current PairedState
        {
            let (dir, state_path) = temp_state_path("migration-concurrent-repair");
            let credential1 = certified_credential();
            let binding1 = JournalIdentity::from_credential(&credential1).client_cert_sha256;
            PairedState {
                credential: Some(credential1.clone()),
                ..Default::default()
            }
            .save(&state_path)
            .unwrap();
            record_fresh_pair_offer(&state_path, &credential1).unwrap();
            write_answer(
                &answer_path(&state_path),
                &AnswerState {
                    confirmed: binding1.clone(),
                    rejected: String::new(),
                },
            )
            .unwrap();

            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let (tx, rx) = tokio::sync::oneshot::channel();
            let state_path_clone = state_path.clone();
            let task = tokio::spawn(async move {
                classify_fresh_pair_offer(
                    &state_path_clone,
                    || async move {
                        started_tx.send(()).unwrap();
                        rx.await.unwrap()
                    },
                    |record| save(&state_path_clone, record),
                )
                .await
            });

            started_rx.await.unwrap();
            // Re-pair with credential2
            let credential2 = certified_credential();
            let binding2 = JournalIdentity::from_credential(&credential2).client_cert_sha256;
            PairedState {
                credential: Some(credential2.clone()),
                ..Default::default()
            }
            .save(&state_path)
            .unwrap();
            record_fresh_pair_offer(&state_path, &credential2).unwrap();
            write_answer(
                &answer_path(&state_path),
                &AnswerState {
                    confirmed: binding2.clone(),
                    rejected: String::new(),
                },
            )
            .unwrap();

            tx.send(Ok(vec![])).unwrap();
            task.await.unwrap().unwrap();

            let record = load(&state_path).unwrap().unwrap();
            assert_eq!(record.binding, binding2);
            assert_eq!(record.eligibility, FreshPairEligibility::Unchecked);
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[tokio::test]
    async fn classify_no_answer_or_rejected_answer_does_not_call_list() {
        // (a) No answer file on disk
        let (dir, state_path) = temp_state_path("migration-no-answer");
        let credential = certified_credential();
        PairedState {
            credential: Some(credential.clone()),
            ..Default::default()
        }
        .save(&state_path)
        .unwrap();
        record_fresh_pair_offer(&state_path, &credential).unwrap();

        assert!(!fresh_pair_offer_needs_list(&state_path).unwrap());
        classify_fresh_pair_offer(
            &state_path,
            || async { panic!("must not call list when no answer") },
            |record| save(&state_path, record),
        )
        .await
        .unwrap();
        let record = load(&state_path).unwrap().unwrap();
        assert_eq!(record.eligibility, FreshPairEligibility::Unchecked);

        // (b) Rejected answer on disk
        let binding = JournalIdentity::from_credential(&credential).client_cert_sha256;
        write_answer(
            &answer_path(&state_path),
            &AnswerState {
                confirmed: String::new(),
                rejected: binding,
            },
        )
        .unwrap();
        assert!(!fresh_pair_offer_needs_list(&state_path).unwrap());
        classify_fresh_pair_offer(
            &state_path,
            || async { panic!("must not call list when answer rejected") },
            |record| save(&state_path, record),
        )
        .await
        .unwrap();
        let record = load(&state_path).unwrap().unwrap();
        assert_eq!(record.eligibility, FreshPairEligibility::Unchecked);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn without_own_cid_filters_exact_cid() {
        let own = format!("sha256:{}", "a".repeat(64));
        let other1 = format!("sha256:{}", "b".repeat(64));
        let other2 = format!("sha256:{}", "c".repeat(64));

        let devices = vec![
            crate::device_metadata::PairedDevice {
                cid: own.clone(),
                display_label: "Me".into(),
            },
            crate::device_metadata::PairedDevice {
                cid: other1.clone(),
                display_label: "Device 1".into(),
            },
            crate::device_metadata::PairedDevice {
                cid: other2.clone(),
                display_label: "Device 2".into(),
            },
        ];

        let filtered = without_own_cid(devices, &own);
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].cid, other1);
        assert_eq!(filtered[1].cid, other2);

        let only_own = vec![crate::device_metadata::PairedDevice {
            cid: own.clone(),
            display_label: "Me".into(),
        }];
        assert!(without_own_cid(only_own, &own).is_empty());
    }

    #[tokio::test]
    async fn classify_admitted_stale_generation_during_prepare_drops_empty_list() {
        let (dir, state_path) = temp_state_path("migration-fenced-stale-empty");
        let credential1 = certified_credential();
        let binding1 = JournalIdentity::from_credential(&credential1).client_cert_sha256;
        PairedState {
            credential: Some(credential1.clone()),
            ..Default::default()
        }
        .save(&state_path)
        .unwrap();
        record_fresh_pair_offer(&state_path, &credential1).unwrap();
        write_answer(
            &answer_path(&state_path),
            &AnswerState {
                confirmed: binding1.clone(),
                rejected: String::new(),
            },
        )
        .unwrap();

        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let state_path_clone = state_path.clone();
        let task = tokio::spawn(async move {
            classify_admitted_fresh_pair_offer(
                &state_path_clone,
                |_admitted| async move {
                    started_tx.send(()).unwrap();
                    release_rx.await.unwrap();
                    Ok(AdmittedFreshPairRead::Devices(vec![]))
                },
                |record| save(&state_path_clone, record),
            )
            .await
        });

        started_rx.await.unwrap();

        // Re-pair with credential2
        let credential2 = certified_credential();
        let binding2 = JournalIdentity::from_credential(&credential2).client_cert_sha256;
        let generation2 = pairing_generation(&credential2.client_cert_pem);
        let cid2 = credential_cid(&credential2).unwrap();
        PairedState {
            credential: Some(credential2.clone()),
            ..Default::default()
        }
        .save(&state_path)
        .unwrap();
        record_fresh_pair_offer(&state_path, &credential2).unwrap();
        write_answer(
            &answer_path(&state_path),
            &AnswerState {
                confirmed: binding2.clone(),
                rejected: String::new(),
            },
        )
        .unwrap();

        let expected_record = load(&state_path).unwrap().unwrap();
        assert_eq!(expected_record.binding, binding2);
        assert_eq!(expected_record.pairing_generation, generation2);
        assert_eq!(expected_record.cid, cid2);
        let expected_rev = expected_record.revision;

        release_tx.send(()).unwrap();
        task.await.unwrap().unwrap();

        let record_after = load(&state_path).unwrap().unwrap();
        assert_eq!(record_after.eligibility, FreshPairEligibility::Unchecked);
        assert!(record_after.offer_pending);
        assert!(!record_after.offer_shown);
        assert_eq!(record_after.phase, MigrationPhase::Offered);
        assert!(record_after.pending_decision.is_none());
        assert!(record_after.server_state.is_none());
        assert_eq!(record_after.revision, expected_rev);
        assert_eq!(record_after.binding, binding2);
        assert_eq!(record_after.pairing_generation, generation2);
        assert_eq!(record_after.cid, cid2);
        assert!(!view(&state_path).unwrap().unwrap().offer_available);
        assert!(fresh_pair_offer_needs_list(&state_path).unwrap());

        // Subsequent call on new generation
        let other_cid = format!("sha256:{}", "b".repeat(64));
        classify_admitted_fresh_pair_offer(
            &state_path,
            |_admitted| async move {
                Ok(AdmittedFreshPairRead::Devices(vec![
                    crate::device_metadata::PairedDevice {
                        cid: other_cid,
                        display_label: "Other".into(),
                    },
                ]))
            },
            |record| save(&state_path, record),
        )
        .await
        .unwrap();

        let record_shown = load(&state_path).unwrap().unwrap();
        assert_eq!(record_shown.eligibility, FreshPairEligibility::ShowOffer);
        assert!(record_shown.offer_pending);
        assert!(!record_shown.offer_shown);
        assert_eq!(record_shown.phase, MigrationPhase::Offered);
        assert!(record_shown.pending_decision.is_none());
        assert!(record_shown.server_state.is_none());
        assert_eq!(record_shown.binding, binding2);
        assert!(view(&state_path).unwrap().unwrap().offer_available);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn classify_admitted_stale_generation_during_prepare_drops_unavailable() {
        let (dir, state_path) = temp_state_path("migration-fenced-stale-unavail");
        let credential1 = certified_credential();
        let binding1 = JournalIdentity::from_credential(&credential1).client_cert_sha256;
        PairedState {
            credential: Some(credential1.clone()),
            ..Default::default()
        }
        .save(&state_path)
        .unwrap();
        record_fresh_pair_offer(&state_path, &credential1).unwrap();
        write_answer(
            &answer_path(&state_path),
            &AnswerState {
                confirmed: binding1.clone(),
                rejected: String::new(),
            },
        )
        .unwrap();

        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let state_path_clone = state_path.clone();
        let task = tokio::spawn(async move {
            classify_admitted_fresh_pair_offer(
                &state_path_clone,
                |_admitted| async move {
                    started_tx.send(()).unwrap();
                    release_rx.await.unwrap();
                    Ok(AdmittedFreshPairRead::Unavailable)
                },
                |record| save(&state_path_clone, record),
            )
            .await
        });

        started_rx.await.unwrap();

        // Re-pair with credential2
        let credential2 = certified_credential();
        let binding2 = JournalIdentity::from_credential(&credential2).client_cert_sha256;
        let generation2 = pairing_generation(&credential2.client_cert_pem);
        let cid2 = credential_cid(&credential2).unwrap();
        PairedState {
            credential: Some(credential2.clone()),
            ..Default::default()
        }
        .save(&state_path)
        .unwrap();
        record_fresh_pair_offer(&state_path, &credential2).unwrap();
        write_answer(
            &answer_path(&state_path),
            &AnswerState {
                confirmed: binding2.clone(),
                rejected: String::new(),
            },
        )
        .unwrap();

        let expected_record = load(&state_path).unwrap().unwrap();
        let expected_rev = expected_record.revision;

        release_tx.send(()).unwrap();
        task.await.unwrap().unwrap();

        let record_after = load(&state_path).unwrap().unwrap();
        assert_eq!(record_after.eligibility, FreshPairEligibility::Unchecked);
        assert!(record_after.offer_pending);
        assert!(!record_after.offer_shown);
        assert_eq!(record_after.phase, MigrationPhase::Offered);
        assert!(record_after.pending_decision.is_none());
        assert!(record_after.server_state.is_none());
        assert_eq!(record_after.revision, expected_rev);
        assert_eq!(record_after.binding, binding2);
        assert_eq!(record_after.pairing_generation, generation2);
        assert_eq!(record_after.cid, cid2);
        assert!(!view(&state_path).unwrap().unwrap().offer_available);
        assert!(fresh_pair_offer_needs_list(&state_path).unwrap());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn classify_admitted_unchanged_generation_outcomes() {
        // 1. Devices(vec![]) -> NoOtherDevice
        {
            let (dir, state_path) = temp_state_path("migration-fenced-empty");
            let credential = certified_credential();
            let binding = JournalIdentity::from_credential(&credential).client_cert_sha256;
            PairedState {
                credential: Some(credential.clone()),
                ..Default::default()
            }
            .save(&state_path)
            .unwrap();
            record_fresh_pair_offer(&state_path, &credential).unwrap();
            write_answer(
                &answer_path(&state_path),
                &AnswerState {
                    confirmed: binding.clone(),
                    rejected: String::new(),
                },
            )
            .unwrap();

            classify_admitted_fresh_pair_offer(
                &state_path,
                |_admitted| async { Ok(AdmittedFreshPairRead::Devices(vec![])) },
                |record| save(&state_path, record),
            )
            .await
            .unwrap();

            let record = load(&state_path).unwrap().unwrap();
            assert_eq!(record.eligibility, FreshPairEligibility::NoOtherDevice);
            assert!(!record.offer_pending);
            assert!(!record.offer_shown);
            assert_eq!(record.phase, MigrationPhase::Offered);
            assert!(record.pending_decision.is_none());
            assert!(record.server_state.is_none());
            assert_eq!(record.revision, 2);
            assert!(!view(&state_path).unwrap().unwrap().offer_available);
            let _ = std::fs::remove_dir_all(dir);
        }

        // 2. Devices containing one device with admitted.cid (self-only) -> NoOtherDevice
        {
            let (dir, state_path) = temp_state_path("migration-fenced-self-only");
            let credential = certified_credential();
            let binding = JournalIdentity::from_credential(&credential).client_cert_sha256;
            PairedState {
                credential: Some(credential.clone()),
                ..Default::default()
            }
            .save(&state_path)
            .unwrap();
            record_fresh_pair_offer(&state_path, &credential).unwrap();
            write_answer(
                &answer_path(&state_path),
                &AnswerState {
                    confirmed: binding.clone(),
                    rejected: String::new(),
                },
            )
            .unwrap();

            classify_admitted_fresh_pair_offer(
                &state_path,
                |admitted| {
                    let own_cid = admitted.cid.clone();
                    async move {
                        Ok(AdmittedFreshPairRead::Devices(vec![
                            crate::device_metadata::PairedDevice {
                                cid: own_cid,
                                display_label: "This PC".into(),
                            },
                        ]))
                    }
                },
                |record| save(&state_path, record),
            )
            .await
            .unwrap();

            let record = load(&state_path).unwrap().unwrap();
            assert_eq!(record.eligibility, FreshPairEligibility::NoOtherDevice);
            assert!(!record.offer_pending);
            assert!(!record.offer_shown);
            assert_eq!(record.phase, MigrationPhase::Offered);
            assert!(record.pending_decision.is_none());
            assert!(record.server_state.is_none());
            assert_eq!(record.revision, 2);
            assert!(!view(&state_path).unwrap().unwrap().offer_available);
            let _ = std::fs::remove_dir_all(dir);
        }

        // 3. Devices containing different exact CID -> ShowOffer
        {
            let (dir, state_path) = temp_state_path("migration-fenced-other-cid");
            let credential = certified_credential();
            let binding = JournalIdentity::from_credential(&credential).client_cert_sha256;
            let other_cid = format!("sha256:{}", "c".repeat(64));
            PairedState {
                credential: Some(credential.clone()),
                ..Default::default()
            }
            .save(&state_path)
            .unwrap();
            record_fresh_pair_offer(&state_path, &credential).unwrap();
            write_answer(
                &answer_path(&state_path),
                &AnswerState {
                    confirmed: binding.clone(),
                    rejected: String::new(),
                },
            )
            .unwrap();

            classify_admitted_fresh_pair_offer(
                &state_path,
                |_admitted| async move {
                    Ok(AdmittedFreshPairRead::Devices(vec![
                        crate::device_metadata::PairedDevice {
                            cid: other_cid,
                            display_label: "Other Device".into(),
                        },
                    ]))
                },
                |record| save(&state_path, record),
            )
            .await
            .unwrap();

            let record = load(&state_path).unwrap().unwrap();
            assert_eq!(record.eligibility, FreshPairEligibility::ShowOffer);
            assert!(record.offer_pending);
            assert!(!record.offer_shown);
            assert_eq!(record.phase, MigrationPhase::Offered);
            assert!(record.pending_decision.is_none());
            assert!(record.server_state.is_none());
            assert_eq!(record.revision, 2);
            assert!(view(&state_path).unwrap().unwrap().offer_available);
            let _ = std::fs::remove_dir_all(dir);
        }

        // 4. Unavailable -> ShowOffer
        {
            let (dir, state_path) = temp_state_path("migration-fenced-unavail");
            let credential = certified_credential();
            let binding = JournalIdentity::from_credential(&credential).client_cert_sha256;
            PairedState {
                credential: Some(credential.clone()),
                ..Default::default()
            }
            .save(&state_path)
            .unwrap();
            record_fresh_pair_offer(&state_path, &credential).unwrap();
            write_answer(
                &answer_path(&state_path),
                &AnswerState {
                    confirmed: binding.clone(),
                    rejected: String::new(),
                },
            )
            .unwrap();

            classify_admitted_fresh_pair_offer(
                &state_path,
                |_admitted| async { Ok(AdmittedFreshPairRead::Unavailable) },
                |record| save(&state_path, record),
            )
            .await
            .unwrap();

            let record = load(&state_path).unwrap().unwrap();
            assert_eq!(record.eligibility, FreshPairEligibility::ShowOffer);
            assert!(record.offer_pending);
            assert!(!record.offer_shown);
            assert_eq!(record.phase, MigrationPhase::Offered);
            assert!(record.pending_decision.is_none());
            assert!(record.server_state.is_none());
            assert_eq!(record.revision, 2);
            assert!(view(&state_path).unwrap().unwrap().offer_available);
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[tokio::test]
    async fn classify_admitted_suspended_prepare_unchanged_generation_still_classifies() {
        let (dir, state_path) = temp_state_path("migration-fenced-suspended-unchanged");
        let credential = certified_credential();
        let binding = JournalIdentity::from_credential(&credential).client_cert_sha256;
        let other_cid = format!("sha256:{}", "b".repeat(64));
        PairedState {
            credential: Some(credential.clone()),
            ..Default::default()
        }
        .save(&state_path)
        .unwrap();
        record_fresh_pair_offer(&state_path, &credential).unwrap();
        write_answer(
            &answer_path(&state_path),
            &AnswerState {
                confirmed: binding.clone(),
                rejected: String::new(),
            },
        )
        .unwrap();

        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let state_path_clone = state_path.clone();
        let task = tokio::spawn(async move {
            classify_admitted_fresh_pair_offer(
                &state_path_clone,
                |_admitted| async move {
                    started_tx.send(()).unwrap();
                    release_rx.await.unwrap();
                    Ok(AdmittedFreshPairRead::Devices(vec![
                        crate::device_metadata::PairedDevice {
                            cid: other_cid,
                            display_label: "Other".into(),
                        },
                    ]))
                },
                |record| save(&state_path_clone, record),
            )
            .await
        });

        started_rx.await.unwrap();
        // While suspended, offer is not available
        assert!(!view(&state_path).unwrap().unwrap().offer_available);

        release_tx.send(()).unwrap();
        task.await.unwrap().unwrap();

        let record = load(&state_path).unwrap().unwrap();
        assert_eq!(record.eligibility, FreshPairEligibility::ShowOffer);
        assert!(record.offer_pending);
        assert!(!record.offer_shown);
        assert_eq!(record.phase, MigrationPhase::Offered);
        assert!(record.pending_decision.is_none());
        assert!(record.server_state.is_none());
        assert_eq!(record.revision, 2);
        assert!(view(&state_path).unwrap().unwrap().offer_available);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn classify_admitted_prepare_error_leaves_unchecked() {
        let (dir, state_path) = temp_state_path("migration-fenced-prep-err");
        let credential = certified_credential();
        let binding = JournalIdentity::from_credential(&credential).client_cert_sha256;
        PairedState {
            credential: Some(credential.clone()),
            ..Default::default()
        }
        .save(&state_path)
        .unwrap();
        record_fresh_pair_offer(&state_path, &credential).unwrap();
        write_answer(
            &answer_path(&state_path),
            &AnswerState {
                confirmed: binding.clone(),
                rejected: String::new(),
            },
        )
        .unwrap();

        let err = classify_admitted_fresh_pair_offer(
            &state_path,
            |_admitted| async { Err(TransportError::NotPaired) },
            |record| save(&state_path, record),
        )
        .await
        .unwrap_err();

        assert!(matches!(err, TransportError::NotPaired));

        let record = load(&state_path).unwrap().unwrap();
        assert_eq!(record.eligibility, FreshPairEligibility::Unchecked);
        assert!(record.offer_pending);
        assert!(!record.offer_shown);
        assert_eq!(record.phase, MigrationPhase::Offered);
        assert!(record.pending_decision.is_none());
        assert!(record.server_state.is_none());
        assert_eq!(record.revision, 1);
        assert!(!view(&state_path).unwrap().unwrap().offer_available);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn classify_admitted_commit_error_leaves_unchecked() {
        let (dir, state_path) = temp_state_path("migration-fenced-commit-err");
        let credential = certified_credential();
        let binding = JournalIdentity::from_credential(&credential).client_cert_sha256;
        let other_cid = format!("sha256:{}", "b".repeat(64));
        PairedState {
            credential: Some(credential.clone()),
            ..Default::default()
        }
        .save(&state_path)
        .unwrap();
        record_fresh_pair_offer(&state_path, &credential).unwrap();
        write_answer(
            &answer_path(&state_path),
            &AnswerState {
                confirmed: binding.clone(),
                rejected: String::new(),
            },
        )
        .unwrap();

        // 1. Commit returns TransportError::Io -> returns error, disk stays Unchecked.
        let err = classify_admitted_fresh_pair_offer(
            &state_path,
            |_admitted| {
                let cid = other_cid.clone();
                async move {
                    Ok(AdmittedFreshPairRead::Devices(vec![
                        crate::device_metadata::PairedDevice {
                            cid,
                            display_label: "Other".into(),
                        },
                    ]))
                }
            },
            |_record| Err(TransportError::Io(std::io::Error::other("injected"))),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, TransportError::Io(_)));

        let record = load(&state_path).unwrap().unwrap();
        assert_eq!(record.eligibility, FreshPairEligibility::Unchecked);
        assert!(record.offer_pending);
        assert_eq!(record.phase, MigrationPhase::Offered);
        assert!(record.pending_decision.is_none());
        assert_eq!(record.revision, 1);
        assert!(!view(&state_path).unwrap().unwrap().offer_available);

        // 2. Commit returns TransportError::ReplayUnsafe -> returns Ok(()), disk stays Unchecked.
        classify_admitted_fresh_pair_offer(
            &state_path,
            |_admitted| async move {
                Ok(AdmittedFreshPairRead::Devices(vec![
                    crate::device_metadata::PairedDevice {
                        cid: other_cid,
                        display_label: "Other".into(),
                    },
                ]))
            },
            |_record| Err(TransportError::ReplayUnsafe),
        )
        .await
        .unwrap();

        let record = load(&state_path).unwrap().unwrap();
        assert_eq!(record.eligibility, FreshPairEligibility::Unchecked);
        assert_eq!(record.revision, 1);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn classify_admitted_rejected_or_missing_confirmation_does_not_prepare() {
        use std::sync::atomic::{AtomicBool, Ordering};

        // (a) Rejected answer
        {
            let (dir, state_path) = temp_state_path("migration-fenced-rejected");
            let credential = certified_credential();
            let binding = JournalIdentity::from_credential(&credential).client_cert_sha256;
            PairedState {
                credential: Some(credential.clone()),
                ..Default::default()
            }
            .save(&state_path)
            .unwrap();
            record_fresh_pair_offer(&state_path, &credential).unwrap();
            write_answer(
                &answer_path(&state_path),
                &AnswerState {
                    confirmed: String::new(),
                    rejected: binding,
                },
            )
            .unwrap();

            let polled = std::sync::Arc::new(AtomicBool::new(false));
            let polled_clone = polled.clone();
            classify_admitted_fresh_pair_offer(
                &state_path,
                |_admitted| {
                    polled_clone.store(true, Ordering::SeqCst);
                    async { Ok(AdmittedFreshPairRead::Devices(vec![])) }
                },
                |record| save(&state_path, record),
            )
            .await
            .unwrap();

            assert!(!polled.load(Ordering::SeqCst));
            let record = load(&state_path).unwrap().unwrap();
            assert_eq!(record.eligibility, FreshPairEligibility::Unchecked);
            assert_eq!(record.revision, 1);
            let _ = std::fs::remove_dir_all(dir);
        }

        // (b) Missing confirmation answer
        {
            let (dir, state_path) = temp_state_path("migration-fenced-no-answer");
            let credential = certified_credential();
            PairedState {
                credential: Some(credential.clone()),
                ..Default::default()
            }
            .save(&state_path)
            .unwrap();
            record_fresh_pair_offer(&state_path, &credential).unwrap();

            let polled = std::sync::Arc::new(AtomicBool::new(false));
            let polled_clone = polled.clone();
            classify_admitted_fresh_pair_offer(
                &state_path,
                |_admitted| {
                    polled_clone.store(true, Ordering::SeqCst);
                    async { Ok(AdmittedFreshPairRead::Devices(vec![])) }
                },
                |record| save(&state_path, record),
            )
            .await
            .unwrap();

            assert!(!polled.load(Ordering::SeqCst));
            let record = load(&state_path).unwrap().unwrap();
            assert_eq!(record.eligibility, FreshPairEligibility::Unchecked);
            assert_eq!(record.revision, 1);
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[tokio::test]
    async fn classify_admitted_prepare_await_does_not_hold_owner_write_guard() {
        let (dir, state_path) = temp_state_path("migration-fenced-guard");
        let credential = certified_credential();
        let binding = JournalIdentity::from_credential(&credential).client_cert_sha256;
        PairedState {
            credential: Some(credential.clone()),
            ..Default::default()
        }
        .save(&state_path)
        .unwrap();
        record_fresh_pair_offer(&state_path, &credential).unwrap();
        write_answer(
            &answer_path(&state_path),
            &AnswerState {
                confirmed: binding.clone(),
                rejected: String::new(),
            },
        )
        .unwrap();

        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let state_path_clone = state_path.clone();
        let task = tokio::spawn(async move {
            classify_admitted_fresh_pair_offer(
                &state_path_clone,
                |_admitted| async move {
                    started_tx.send(()).unwrap();
                    release_rx.await.unwrap();
                    Ok(AdmittedFreshPairRead::Unavailable)
                },
                |record| save(&state_path_clone, record),
            )
            .await
        });

        started_rx.await.unwrap();

        let start = std::time::Instant::now();
        let mut acquired = false;
        while start.elapsed() < std::time::Duration::from_secs(2) {
            if let Some(_guard) = crate::credential::try_owner_state_write_guard() {
                acquired = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            acquired,
            "owner state write guard must not be held during prepare await"
        );

        release_tx.send(()).unwrap();
        task.await.unwrap().unwrap();

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn credential_matches_admitted_fresh_pair_requires_binding_generation_and_cid() {
        let credential_a = certified_credential();
        let admitted_a = AdmittedFreshPair {
            binding: JournalIdentity::from_credential(&credential_a).client_cert_sha256,
            pairing_generation: pairing_generation(&credential_a.client_cert_pem),
            revision: 1,
            cid: credential_cid(&credential_a).unwrap(),
        };

        assert!(credential_matches_admitted_fresh_pair(&credential_a, &admitted_a).unwrap());

        let credential_b = certified_credential();
        assert!(!credential_matches_admitted_fresh_pair(&credential_b, &admitted_a).unwrap());

        // Mismatched binding
        let mut admitted_bad_binding = admitted_a.clone();
        admitted_bad_binding.binding = format!("sha256:{}", "0".repeat(64));
        assert!(
            !credential_matches_admitted_fresh_pair(&credential_a, &admitted_bad_binding).unwrap()
        );

        // Mismatched generation
        let mut admitted_bad_gen = admitted_a.clone();
        admitted_bad_gen.pairing_generation = [99u8; 32];
        assert!(!credential_matches_admitted_fresh_pair(&credential_a, &admitted_bad_gen).unwrap());

        // Mismatched cid
        let mut admitted_bad_cid = admitted_a.clone();
        admitted_bad_cid.cid = format!("sha256:{}", "f".repeat(64));
        assert!(!credential_matches_admitted_fresh_pair(&credential_a, &admitted_bad_cid).unwrap());
    }

    #[tokio::test]
    async fn classify_admitted_superseded_on_unchanged_generation_leaves_unchecked() {
        let (dir, state_path) = temp_state_path("migration-fenced-superseded");
        let credential = certified_credential();
        let binding = JournalIdentity::from_credential(&credential).client_cert_sha256;
        let generation = pairing_generation(&credential.client_cert_pem);
        let cid = credential_cid(&credential).unwrap();
        PairedState {
            credential: Some(credential.clone()),
            ..Default::default()
        }
        .save(&state_path)
        .unwrap();
        record_fresh_pair_offer(&state_path, &credential).unwrap();
        write_answer(
            &answer_path(&state_path),
            &AnswerState {
                confirmed: binding.clone(),
                rejected: String::new(),
            },
        )
        .unwrap();

        let before = load(&state_path).unwrap().unwrap();
        let before_rev = before.revision;

        classify_admitted_fresh_pair_offer(
            &state_path,
            |_admitted| async { Ok(AdmittedFreshPairRead::Superseded) },
            |record| save(&state_path, record),
        )
        .await
        .unwrap();

        let record = load(&state_path).unwrap().unwrap();
        assert_eq!(record.eligibility, FreshPairEligibility::Unchecked);
        assert!(record.offer_pending);
        assert!(!record.offer_shown);
        assert_eq!(record.phase, MigrationPhase::Offered);
        assert!(record.pending_decision.is_none());
        assert!(record.server_state.is_none());
        assert_eq!(record.revision, before_rev);
        assert_eq!(record.binding, binding);
        assert_eq!(record.pairing_generation, generation);
        assert_eq!(record.cid, cid);
        assert!(!view(&state_path).unwrap().unwrap().offer_available);
        assert!(fresh_pair_offer_needs_list(&state_path).unwrap());

        let _ = std::fs::remove_dir_all(dir);
    }
}
