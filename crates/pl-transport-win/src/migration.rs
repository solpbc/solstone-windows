// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Durable client-side state for the journal's authenticated device migration v1.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ack::JournalIdentity;
use crate::answer::{answer_path, read_answer, write_answer, AnswerState};
#[cfg(windows)]
use crate::credential::protect_candidate_key;
#[cfg(any(windows, test))]
use crate::credential::GeneratedKey;
use crate::credential::{
    pairing_generation, unprotect_candidate_key, Credential, EndpointAddr, PairedState,
};
use crate::device_marker::{evaluate, DeviceMarker, MarkerDecision, MarkerResult};
use crate::{ObserverClient, TransportError};

pub const MIGRATION_RECORD_SCHEMA: &str = "solstone.windows-device-migration.v1";
const DEVICE_LABEL_LIMIT: usize = 80;
const CLIENT_LABEL_LIMIT: usize = 253;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Choice {
    NewDevice,
    SameDevice,
    ReplaceDevice,
}

impl Choice {
    fn as_wire(self) -> &'static str {
        match self {
            Self::NewDevice => "new_device",
            Self::SameDevice => "same_device",
            Self::ReplaceDevice => "replace_device",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerState {
    None,
    Pending,
    NewDevice,
    SameDevice,
    ReplacedDevice,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RekeyRequest {
    pub protocol_version: u32,
    pub operation_id: String,
    pub csr: String,
    pub device_label: String,
    pub client_label: String,
    pub platform: String,
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
pub struct RekeyResponse {
    pub protocol_version: u32,
    pub operation_id: String,
    pub state: ServerState,
    pub previous_cid: String,
    pub cid: String,
    pub pairing: Value,
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
    Baseline,
    RequestPrepared,
    ResponseRecorded,
    CredentialPublished,
    DecisionUnknown,
    Admitted,
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
    pub baseline_marker: Option<DeviceMarker>,
    pub marker_probe: MarkerResult,
    pub bound_marker: Option<DeviceMarker>,
    pub bound_missing_marker: bool,
    pub pairing_generation: [u8; 32],
    pub access_mutation_generation: u64,
    pub phase: MigrationPhase,
    pub request: Option<RekeyRequest>,
    pub request_bytes_base64: Option<String>,
    pub candidate_key_protected: Option<String>,
    pub candidate_spki_base64: Option<String>,
    pub old_cid: Option<String>,
    pub old_certificate_binding: Option<String>,
    pub old_instance_id: Option<String>,
    pub response_bytes_base64: Option<String>,
    pub new_cid: Option<String>,
    pub new_certificate_binding: Option<String>,
    pub answer_snapshot: AnswerState,
    pub answer_published: bool,
    pub pending_decision: Option<SavedDecision>,
    #[serde(default)]
    pub terminal_decisions: Vec<SavedDecision>,
    pub server_state: Option<ServerState>,
    pub replaced_cid: Option<String>,
    pub decision_display_label: Option<String>,
    pub fresh_pair_offer_pending: bool,
    pub fresh_pair_offer_shown: bool,
    pub fresh_pair_offer_binding: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MigrationView {
    pub phase: MigrationPhase,
    pub revision: u64,
    pub pairing_generation: [u8; 32],
    pub state: Option<ServerState>,
    pub old_cid: Option<String>,
    pub new_cid: Option<String>,
    pub replaced_cid: Option<String>,
    pub decision_choice: Option<Choice>,
    pub decision_result: Option<String>,
    pub same_device_available: bool,
    pub offer_available: bool,
    pub offer_binding: Option<String>,
}

impl MigrationRecord {
    fn baseline(marker: Option<DeviceMarker>, probe: MarkerResult) -> Self {
        Self {
            schema: MIGRATION_RECORD_SCHEMA.to_owned(),
            revision: 0,
            baseline_marker: marker,
            marker_probe: probe,
            bound_marker: None,
            bound_missing_marker: false,
            pairing_generation: [0; 32],
            access_mutation_generation: 0,
            phase: MigrationPhase::Baseline,
            request: None,
            request_bytes_base64: None,
            candidate_key_protected: None,
            candidate_spki_base64: None,
            old_cid: None,
            old_certificate_binding: None,
            old_instance_id: None,
            response_bytes_base64: None,
            new_cid: None,
            new_certificate_binding: None,
            answer_snapshot: AnswerState::default(),
            answer_published: false,
            pending_decision: None,
            terminal_decisions: Vec::new(),
            server_state: None,
            replaced_cid: None,
            decision_display_label: None,
            fresh_pair_offer_pending: false,
            fresh_pair_offer_shown: false,
            fresh_pair_offer_binding: None,
        }
    }

    pub fn is_send_admitted(&self) -> bool {
        matches!(
            self.phase,
            MigrationPhase::Baseline | MigrationPhase::Admitted
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkerAction {
    EstablishBaseline,
    Continue,
    StartOrResumeMigration,
    HoldForProbe,
}

pub fn marker_action(record: Option<&MigrationRecord>, current: &MarkerResult) -> MarkerAction {
    let Some(record) = record else {
        return match current {
            MarkerResult::Available(_) => MarkerAction::EstablishBaseline,
            MarkerResult::Missing => MarkerAction::HoldForProbe,
            MarkerResult::ProbeFailure { .. } => MarkerAction::HoldForProbe,
        };
    };
    if record.baseline_marker.is_none() && matches!(current, MarkerResult::Missing) {
        if record.bound_missing_marker && record.request.is_some() {
            return if record.is_send_admitted() {
                MarkerAction::Continue
            } else {
                MarkerAction::StartOrResumeMigration
            };
        }
        if record.fresh_pair_offer_binding.is_some()
            && record.request.is_none()
            && record.is_send_admitted()
        {
            return MarkerAction::Continue;
        }
        return MarkerAction::HoldForProbe;
    }
    match evaluate(record.baseline_marker.as_ref(), current) {
        MarkerDecision::EstablishBaseline => MarkerAction::EstablishBaseline,
        MarkerDecision::UnchangedPrimary | MarkerDecision::SameFallback => {
            if record.is_send_admitted() {
                MarkerAction::Continue
            } else {
                MarkerAction::StartOrResumeMigration
            }
        }
        MarkerDecision::Changed | MarkerDecision::Missing => MarkerAction::StartOrResumeMigration,
        MarkerDecision::ProbeFailure => MarkerAction::HoldForProbe,
    }
}

pub fn request_is_reusable(
    record: &MigrationRecord,
    marker: &MarkerResult,
    current_cid: &str,
) -> bool {
    let marker_matches = match marker {
        MarkerResult::Available(value) => record.bound_marker.as_ref() == Some(value),
        MarkerResult::Missing => record.bound_missing_marker && record.bound_marker.is_none(),
        MarkerResult::ProbeFailure { .. } => false,
    };
    let identity_matches = record.old_cid.as_deref() == Some(current_cid)
        || record.new_cid.as_deref() == Some(current_cid);
    record.request.is_some()
        && record.request_bytes_base64.is_some()
        && record.candidate_key_protected.is_some()
        && record.candidate_spki_base64.is_some()
        && marker_matches
        && identity_matches
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

fn cid_for_cert(pem: &str) -> Result<String, TransportError> {
    let certs = spl_transport::tls::parse_certs(pem).map_err(crate::pairing::map_shared_error)?;
    let cert = certs.first().ok_or(TransportError::CredentialMalformed)?;
    Ok(format!(
        "sha256:{}",
        spl_core::ca::sha256_hex(cert.as_ref())
    ))
}

/// Return the journal's exact CID for this saved certificate.
pub fn credential_cid(credential: &Credential) -> Result<String, TransportError> {
    cid_for_cert(&credential.client_cert_pem)
}

#[cfg(any(windows, test))]
fn new_operation_id(spki: &[u8]) -> String {
    let mut bytes: [u8; 16] = spl_core::ca::sha256(spki)[..16].try_into().unwrap();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}

#[cfg(any(windows, test))]
struct RekeyPreparation<'a> {
    marker: &'a MarkerResult,
    paired: &'a PairedState,
    answer: AnswerState,
    device_label: &'a str,
    client_label: &'a str,
    candidate: GeneratedKey,
    candidate_key_protected: String,
}

#[cfg(any(windows, test))]
fn prepare_rekey(
    state_path: &Path,
    mut record: MigrationRecord,
    preparation: RekeyPreparation<'_>,
) -> Result<MigrationRecord, TransportError> {
    let RekeyPreparation {
        marker,
        paired,
        answer,
        device_label,
        client_label,
        candidate,
        candidate_key_protected,
    } = preparation;
    let credential = paired
        .credential
        .as_ref()
        .ok_or(TransportError::NotPaired)?;
    if device_label.trim().is_empty()
        || device_label.chars().count() > DEVICE_LABEL_LIMIT
        || client_label.trim().is_empty()
        || client_label.chars().count() > CLIENT_LABEL_LIMIT
    {
        return Err(TransportError::CredentialMalformed);
    }
    let bound_marker = match marker {
        MarkerResult::Available(marker) => Some(marker.clone()),
        MarkerResult::Missing | MarkerResult::ProbeFailure { .. } => None,
    };
    let bound_missing_marker = matches!(marker, MarkerResult::Missing);
    let old_cid = cid_for_cert(&credential.client_cert_pem)?;
    let old_binding = JournalIdentity::from_credential(credential).client_cert_sha256;
    let operation_id = new_operation_id(&candidate.public_key_spki_der);
    let request = RekeyRequest {
        protocol_version: 1,
        operation_id,
        csr: candidate.csr_pem,
        device_label: device_label.to_owned(),
        client_label: client_label.to_owned(),
        platform: "windows".to_owned(),
    };
    validate_rekey_request(&request)?;
    let request_bytes = serde_json::to_vec(&request)?;
    record.schema = MIGRATION_RECORD_SCHEMA.to_owned();
    record.marker_probe = marker.clone();
    record.bound_marker = bound_marker;
    record.bound_missing_marker = bound_missing_marker;
    record.pairing_generation = pairing_generation(&credential.client_cert_pem);
    record.access_mutation_generation = paired.access_mutation_generation;
    record.phase = MigrationPhase::RequestPrepared;
    record.request = Some(request);
    record.request_bytes_base64 =
        Some(base64::engine::general_purpose::STANDARD.encode(request_bytes));
    record.candidate_key_protected = Some(candidate_key_protected);
    record.candidate_spki_base64 =
        Some(base64::engine::general_purpose::STANDARD.encode(candidate.public_key_spki_der));
    record.old_cid = Some(old_cid);
    record.old_certificate_binding = Some(old_binding);
    record.old_instance_id = Some(credential.instance_id.clone());
    record.response_bytes_base64 = None;
    record.new_cid = None;
    record.new_certificate_binding = None;
    record.answer_snapshot = answer;
    record.answer_published = false;
    record.pending_decision = None;
    record.terminal_decisions.clear();
    record.server_state = None;
    record.replaced_cid = None;
    record.decision_display_label = None;
    record.fresh_pair_offer_pending = false;
    record.fresh_pair_offer_shown = false;
    record.fresh_pair_offer_binding = None;
    save(state_path, &mut record)?;
    Ok(record)
}

pub fn validate_rekey_request(request: &RekeyRequest) -> Result<(), TransportError> {
    let uuid = request.operation_id.as_bytes();
    if request.protocol_version != 1
        || request.platform != "windows"
        || request.operation_id.len() != 36
        || uuid.iter().enumerate().any(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte != b'-'
            } else {
                !byte.is_ascii_hexdigit()
            }
        })
        || request.csr.trim().is_empty()
        || request.device_label.trim().is_empty()
        || request.device_label.chars().count() > DEVICE_LABEL_LIMIT
        || request.client_label.trim().is_empty()
        || request.client_label.chars().count() > CLIENT_LABEL_LIMIT
    {
        return Err(TransportError::CredentialMalformed);
    }
    Ok(())
}

pub fn saved_request_bytes(record: &MigrationRecord) -> Result<Vec<u8>, TransportError> {
    let request = record
        .request
        .as_ref()
        .ok_or(TransportError::CredentialMalformed)?;
    validate_rekey_request(request)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(
            record
                .request_bytes_base64
                .as_deref()
                .ok_or(TransportError::CredentialMalformed)?,
        )
        .map_err(|_| TransportError::CredentialMalformed)?;
    let decoded: RekeyRequest =
        serde_json::from_slice(&bytes).map_err(|_| TransportError::CredentialMalformed)?;
    if decoded != *request {
        return Err(TransportError::CredentialMalformed);
    }
    Ok(bytes)
}

pub fn record_response(
    state_path: &Path,
    record: &mut MigrationRecord,
    status: u16,
    body: &[u8],
) -> Result<RekeyResponse, TransportError> {
    if status != 200 && status != 201 {
        return Err(TransportError::Rejected {
            status,
            body: String::from_utf8_lossy(body).into_owned(),
        });
    }
    let response: RekeyResponse =
        serde_json::from_slice(body).map_err(|_| TransportError::CredentialMalformed)?;
    let request = record
        .request
        .as_ref()
        .ok_or(TransportError::CredentialMalformed)?;
    if response.protocol_version != 1
        || response.operation_id != request.operation_id
        || response.state != ServerState::Pending
        || Some(response.previous_cid.as_str()) != record.old_cid.as_deref()
        || !valid_cid(&response.cid)
    {
        return Err(TransportError::CredentialMalformed);
    }
    record.response_bytes_base64 = Some(base64::engine::general_purpose::STANDARD.encode(body));
    record.new_cid = Some(response.cid.clone());
    record.server_state = Some(ServerState::Pending);
    record.phase = MigrationPhase::ResponseRecorded;
    save(state_path, record)?;
    Ok(response)
}

fn valid_cid(cid: &str) -> bool {
    cid.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, TransportError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or(TransportError::CredentialMalformed)
}

fn cert_der(pem: &str) -> Result<Vec<u8>, TransportError> {
    spl_transport::tls::parse_certs(pem)
        .map_err(crate::pairing::map_shared_error)?
        .into_iter()
        .next()
        .map(|cert| cert.as_ref().to_vec())
        .ok_or(TransportError::CredentialMalformed)
}

fn validate_chain_and_leaf(
    old: &Credential,
    leaf_pem: &str,
    ca_chain: &[String],
    candidate_spki: &[u8],
    expected_cid: &str,
    expected_fingerprint: &str,
) -> Result<Vec<u8>, TransportError> {
    if ca_chain.is_empty() || ca_chain.len() != old.ca_chain_pem.len() {
        return Err(TransportError::CredentialMalformed);
    }
    let old_chain = old
        .ca_chain_pem
        .iter()
        .map(|pem| cert_der(pem))
        .collect::<Result<Vec<_>, _>>()?;
    let new_chain = ca_chain
        .iter()
        .map(|pem| cert_der(pem))
        .collect::<Result<Vec<_>, _>>()?;
    if old_chain != new_chain {
        return Err(TransportError::CredentialMalformed);
    }
    let ca_fp = spl_core::ca::sha256(&new_chain[0]);
    if old.ca_fp_prefix.is_empty() || ca_fp[..old.ca_fp_prefix.len()] != old.ca_fp_prefix {
        return Err(TransportError::CredentialMalformed);
    }
    let leaf_der = cert_der(leaf_pem)?;
    let (_, leaf) = x509_parser::parse_x509_certificate(&leaf_der)
        .map_err(|_| TransportError::CredentialMalformed)?;
    if leaf.tbs_certificate.subject_pki.raw != candidate_spki {
        return Err(TransportError::CredentialMalformed);
    }
    let leaf_cid = format!("sha256:{}", spl_core::ca::sha256_hex(&leaf_der));
    if leaf_cid != expected_cid || expected_fingerprint != expected_cid {
        return Err(TransportError::CredentialMalformed);
    }
    let issuer_matches = new_chain.iter().any(|der| {
        x509_parser::parse_x509_certificate(der)
            .ok()
            .is_some_and(|(_, issuer)| {
                leaf.issuer() == issuer.subject()
                    && leaf
                        .verify_signature(Some(&issuer.tbs_certificate.subject_pki))
                        .is_ok()
            })
    });
    if !issuer_matches {
        return Err(TransportError::CredentialMalformed);
    }
    Ok(ca_fp[..old.ca_fp_prefix.len()].to_vec())
}

fn answer_can_be_rebound(
    saved: &AnswerState,
    current: &AnswerState,
    old_binding: &str,
    new_binding: &str,
) -> bool {
    saved.rejected.is_empty()
        && saved.confirmed == old_binding
        && current.rejected.is_empty()
        && (current.confirmed == old_binding || current.confirmed == new_binding)
}

fn publish_migrated_credential(
    latest: &mut Credential,
    candidate: Credential,
    keep_latest_access: bool,
) {
    let latest_access = (
        latest.relay_origin.clone(),
        latest.device_token.clone(),
        latest.device_token_expires_at,
    );
    latest.client_key_pem = candidate.client_key_pem;
    latest.client_cert_pem = candidate.client_cert_pem;
    latest.ca_chain_pem = candidate.ca_chain_pem;
    latest.ca_fp_prefix = candidate.ca_fp_prefix;
    latest.instance_id = candidate.instance_id;
    latest.home_label = candidate.home_label;
    latest.endpoints = candidate.endpoints;
    if keep_latest_access {
        latest.relay_origin = latest_access.0;
        latest.device_token = latest_access.1;
        latest.device_token_expires_at = latest_access.2;
    } else {
        latest.relay_origin = candidate.relay_origin;
        latest.device_token = candidate.device_token;
        latest.device_token_expires_at = candidate.device_token_expires_at;
    }
}

fn parse_endpoint_values(value: Option<&Value>) -> Result<Vec<EndpointAddr>, TransportError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let Some(items) = value.as_array() else {
        return Err(TransportError::CredentialMalformed);
    };
    items
        .iter()
        .map(|item| {
            let host = required_string(item, "ip")?;
            let port = item
                .get("port")
                .and_then(Value::as_u64)
                .and_then(|port| u16::try_from(port).ok())
                .filter(|port| *port != 0)
                .ok_or(TransportError::CredentialMalformed)?;
            Ok(EndpointAddr {
                host: host.to_owned(),
                port,
            })
        })
        .collect()
}

fn parse_expiry(value: &str) -> Result<i64, TransportError> {
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .map(|time| time.unix_timestamp())
        .map_err(|_| TransportError::CredentialMalformed)
}

pub fn candidate_credential(
    record: &MigrationRecord,
    old: &Credential,
    response: &RekeyResponse,
) -> Result<Credential, TransportError> {
    let protected_key = record
        .candidate_key_protected
        .as_deref()
        .ok_or(TransportError::CredentialMalformed)?;
    let client_key_pem = unprotect_candidate_key(protected_key)?;
    candidate_credential_with_key(record, old, response, client_key_pem)
}

fn candidate_credential_with_key(
    record: &MigrationRecord,
    old: &Credential,
    response: &RekeyResponse,
    client_key_pem: String,
) -> Result<Credential, TransportError> {
    record
        .request
        .as_ref()
        .ok_or(TransportError::CredentialMalformed)?;
    let expected_spki = base64::engine::general_purpose::STANDARD
        .decode(
            record
                .candidate_spki_base64
                .as_deref()
                .ok_or(TransportError::CredentialMalformed)?,
        )
        .map_err(|_| TransportError::CredentialMalformed)?;
    let pairing = &response.pairing;
    let client_cert_pem = required_string(pairing, "client_cert")?.to_owned();
    let ca_chain_pem = pairing
        .get("ca_chain")
        .and_then(Value::as_array)
        .ok_or(TransportError::CredentialMalformed)?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or(TransportError::CredentialMalformed)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let instance_id = required_string(pairing, "instance_id")?.to_owned();
    if instance_id != old.instance_id
        || Some(instance_id.as_str()) != record.old_instance_id.as_deref()
        || required_string(pairing, "home_label")?.len() > 253
    {
        return Err(TransportError::CredentialMalformed);
    }
    let ca_fp_prefix = validate_chain_and_leaf(
        old,
        &client_cert_pem,
        &ca_chain_pem,
        &expected_spki,
        &response.cid,
        required_string(pairing, "fingerprint")?,
    )?;
    let parsed_endpoints = parse_endpoint_values(pairing.get("local_endpoints"))?;
    let mut credential = Credential {
        client_key_pem,
        client_cert_pem,
        ca_chain_pem,
        ca_fp_prefix,
        instance_id,
        home_label: required_string(pairing, "home_label")?.to_owned(),
        endpoints: if parsed_endpoints.is_empty() {
            old.endpoints.clone()
        } else {
            parsed_endpoints
        },
        relay_origin: None,
        device_token: None,
        device_token_expires_at: None,
    };
    if let Some(access) = pairing.get("relay_access") {
        let status = required_string(access, "status")?;
        let protocol = access.get("protocol_version").and_then(Value::as_u64);
        if status == "ready" {
            if protocol != Some(2)
                || required_string(access, "instance_id")? != credential.instance_id
            {
                return Err(TransportError::CredentialMalformed);
            }
            credential.relay_origin = Some(required_string(access, "relay_origin")?.to_owned());
            credential.device_token = Some(required_string(access, "device_token")?.to_owned());
            credential.device_token_expires_at =
                Some(parse_expiry(required_string(access, "expires_at")?)?);
        } else if protocol != Some(2) {
            return Err(TransportError::CredentialMalformed);
        }
    }
    ObserverClient::new(
        credential.clone(),
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    )?;
    Ok(credential)
}

pub fn publish_candidate(
    state_path: &Path,
    record: &mut MigrationRecord,
    old_paired: &PairedState,
    candidate: Credential,
) -> Result<(), TransportError> {
    let old = old_paired
        .credential
        .as_ref()
        .ok_or(TransportError::NotPaired)?;
    let current = PairedState::load(state_path)?;
    let current_credential = current
        .credential
        .as_ref()
        .ok_or(TransportError::NotPaired)?;
    let current_cid = cid_for_cert(&current_credential.client_cert_pem)?;
    if current_cid != record.new_cid.as_deref().unwrap_or_default() {
        if pairing_generation(&current_credential.client_cert_pem) != record.pairing_generation
            || current_credential.instance_id != old.instance_id
        {
            return Err(TransportError::CredentialMalformed);
        }
        let candidate_for_mutation = candidate.clone();
        let keep_latest_access = current.access_mutation_generation
            != record.access_mutation_generation
            || candidate.relay_origin.is_none();
        let expected = crate::credential::CasKey {
            pairing_generation: pairing_generation(&current_credential.client_cert_pem),
            access_mutation_generation: current.access_mutation_generation,
        };
        PairedState::mutate(state_path, expected, move |latest| {
            publish_migrated_credential(latest, candidate_for_mutation, keep_latest_access);
            Ok(())
        })
        .map_err(TransportError::from)?;
    }

    let new_binding = JournalIdentity::from_credential(&candidate).client_cert_sha256;
    let answer_path = answer_path(state_path);
    let mut answer = read_answer(&answer_path)
        .map_err(TransportError::from)?
        .unwrap_or_default();
    if record
        .old_certificate_binding
        .as_deref()
        .is_some_and(|old_binding| {
            answer_can_be_rebound(&record.answer_snapshot, &answer, old_binding, &new_binding)
        })
    {
        answer.confirmed = new_binding.clone();
    } else {
        answer.confirmed.clear();
        answer.rejected.clear();
    }
    write_answer(&answer_path, &answer).map_err(TransportError::from)?;
    record.answer_published = true;
    record.new_cid = Some(cid_for_cert(&candidate.client_cert_pem)?);
    record.new_certificate_binding = Some(new_binding);
    record.pairing_generation = pairing_generation(&candidate.client_cert_pem);
    record.access_mutation_generation = PairedState::load(state_path)?.access_mutation_generation;
    if let MarkerResult::Available(marker) = &record.marker_probe {
        record.baseline_marker = Some(marker.clone());
    }
    record.phase = MigrationPhase::Admitted;
    save(state_path, record)?;
    Ok(())
}

pub fn commit_baseline_after_answer(
    state_path: &Path,
    marker: &MarkerResult,
    paired: &PairedState,
) -> Result<(), TransportError> {
    let Some(credential) = paired.credential.as_ref() else {
        return Ok(());
    };
    let Ok(Some(answer)) = read_answer(&answer_path(state_path)) else {
        return Ok(());
    };
    if answer.confirmed.is_empty()
        || !answer.rejected.is_empty()
        || answer.confirmed != JournalIdentity::from_credential(credential).client_cert_sha256
    {
        return Ok(());
    }
    let MarkerResult::Available(marker_value) = marker else {
        return Ok(());
    };
    let mut record = load(state_path)?
        .unwrap_or_else(|| MigrationRecord::baseline(Some(marker_value.clone()), marker.clone()));
    record.baseline_marker = Some(marker_value.clone());
    record.marker_probe = marker.clone();
    record.pairing_generation = pairing_generation(&credential.client_cert_pem);
    record.access_mutation_generation = paired.access_mutation_generation;
    if record.phase == MigrationPhase::CredentialPublished && record.answer_published {
        record.phase = MigrationPhase::Admitted;
        record.fresh_pair_offer_pending = true;
    } else if record.phase == MigrationPhase::RequestPrepared {
        record.phase = MigrationPhase::Baseline;
    }
    save(state_path, &mut record)
}

pub fn persist_decision(
    state_path: &Path,
    record: &mut MigrationRecord,
    choice: Choice,
    replaces_cid: Option<String>,
) -> Result<(), TransportError> {
    if (choice == Choice::ReplaceDevice) != replaces_cid.is_some()
        || replaces_cid.as_deref().is_some_and(|cid| !valid_cid(cid))
        || replaces_cid.as_deref() == record.new_cid.as_deref()
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

    let fresh_pair = record.request.is_none();
    let lineage_valid = if fresh_pair {
        record.fresh_pair_offer_pending
            && record.old_cid.is_none()
            && record.new_cid.as_deref().is_some_and(valid_cid)
            && record.new_certificate_binding.is_some()
            && matches!(
                record.phase,
                MigrationPhase::Baseline | MigrationPhase::Admitted
            )
            && record.server_state.is_none()
            && choice != Choice::SameDevice
    } else {
        record.phase == MigrationPhase::Admitted
            && record.server_state == Some(ServerState::Pending)
            && record.new_cid.as_deref().is_some_and(valid_cid)
            && record.old_cid.as_deref().is_some_and(valid_cid)
            && record.new_certificate_binding.is_some()
    };
    if !lineage_valid {
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
    let expected_state = match saved.request.choice {
        Choice::NewDevice => ServerState::NewDevice,
        Choice::SameDevice => ServerState::SameDevice,
        Choice::ReplaceDevice => ServerState::ReplacedDevice,
    };
    if response.protocol_version != 1
        || response.operation_id != saved.request.operation_id
        || response.state != expected_state
        || response.previous_cid.as_deref() != record.old_cid.as_deref()
        || response.cid != record.new_cid.as_deref().unwrap_or_default()
        || !valid_cid(&response.cid)
        || match saved.request.choice {
            Choice::ReplaceDevice => response.replaced_cid != saved.request.replaces_cid,
            Choice::SameDevice => response.replaced_cid.as_deref() != record.old_cid.as_deref(),
            Choice::NewDevice => response.replaced_cid.is_some(),
        }
    {
        return Err(TransportError::CredentialMalformed);
    }
    saved.result = response.state.as_wire().to_owned();
    record.server_state = Some(response.state);
    record.replaced_cid = response.replaced_cid.clone();
    record.decision_display_label = Some(response.display_label.clone());
    record.phase = MigrationPhase::Admitted;
    record.fresh_pair_offer_pending = false;
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
    if let Some(request) = &record.request {
        material.extend_from_slice(request.operation_id.as_bytes());
    }
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
    record.phase = if record.request.is_some() {
        MigrationPhase::Admitted
    } else {
        MigrationPhase::Baseline
    };
    save(state_path, record)?;
    Ok(true)
}

pub fn reconcile_decision_state(
    state_path: &Path,
    record: &mut MigrationRecord,
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
    let expected = match saved.request.choice {
        Choice::NewDevice => ServerState::NewDevice,
        Choice::SameDevice => ServerState::SameDevice,
        Choice::ReplaceDevice => ServerState::ReplacedDevice,
    };
    let replaced_matches = match saved.request.choice {
        Choice::ReplaceDevice => response.replaced_cid == saved.request.replaces_cid,
        Choice::SameDevice => response.replaced_cid.as_deref() == record.old_cid.as_deref(),
        Choice::NewDevice => response.replaced_cid.is_none(),
    };
    match record.request.as_ref() {
        Some(request)
            if response.rekey_operation_id.as_deref() == Some(request.operation_id.as_str())
                && response.previous_cid.as_deref() == record.old_cid.as_deref() =>
        {
            if response.state == ServerState::Pending && response.replaced_cid.is_none() {
                return Ok(DecisionReconcile::ReplayExactRequest);
            }
            if response.state == expected && replaced_matches {
                record.server_state = Some(response.state);
                record.replaced_cid = response.replaced_cid.clone();
                record.pending_decision.as_mut().unwrap().result =
                    response.state.as_wire().to_owned();
                record.phase = MigrationPhase::Admitted;
                record.fresh_pair_offer_pending = false;
                save(state_path, record)?;
                return Ok(DecisionReconcile::Terminal);
            }
        }
        None if response.rekey_operation_id.is_none()
            && response.previous_cid.is_none()
            && (response.state == ServerState::None
                || (response.state == expected && replaced_matches)) =>
        {
            // The fresh-pair GET has no decision UUID. Replay the saved PUT to
            // recover its response and prove which request reached the journal.
            return Ok(DecisionReconcile::ReplayExactRequest);
        }
        _ => {}
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
    match reconcile_decision_state(state_path, record, &server_state)? {
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
    let offer_available = record.fresh_pair_offer_pending
        && !record.fresh_pair_offer_shown
        && record.fresh_pair_offer_binding.as_deref() == current_binding.as_deref()
        && answer.rejected.is_empty()
        && current_binding.as_deref() == Some(answer.confirmed.as_str())
        && !answer.confirmed.is_empty();
    let same_device_available = record.request.is_some() && record.old_cid.is_some();
    let decision = record.pending_decision.as_ref();
    Ok(Some(MigrationView {
        phase: record.phase,
        revision: record.revision,
        pairing_generation: record.pairing_generation,
        state: record.server_state,
        old_cid: record.old_cid,
        new_cid: record.new_cid,
        replaced_cid: record.replaced_cid,
        decision_choice: decision.map(|decision| decision.request.choice),
        decision_result: decision.map(|decision| decision.result.clone()),
        same_device_available,
        offer_available,
        offer_binding: record.fresh_pair_offer_binding,
    }))
}

pub fn record_fresh_pair_offer(
    state_path: &Path,
    credential: &Credential,
) -> Result<(), TransportError> {
    let marker = crate::device_marker::probe_platform();
    record_fresh_pair_offer_with_marker(state_path, credential, marker)
}

fn record_fresh_pair_offer_with_marker(
    state_path: &Path,
    credential: &Credential,
    marker: MarkerResult,
) -> Result<(), TransportError> {
    let previous_revision = load(state_path)?.map_or(0, |record| record.revision);
    let mut record = MigrationRecord::baseline(
        match &marker {
            MarkerResult::Available(marker) => Some(marker.clone()),
            MarkerResult::Missing | MarkerResult::ProbeFailure { .. } => None,
        },
        marker,
    );
    record.revision = previous_revision;
    record.pairing_generation = pairing_generation(&credential.client_cert_pem);
    record.new_cid = Some(cid_for_cert(&credential.client_cert_pem)?);
    record.new_certificate_binding =
        Some(JournalIdentity::from_credential(credential).client_cert_sha256);
    record.fresh_pair_offer_pending = true;
    record.fresh_pair_offer_shown = false;
    record.fresh_pair_offer_binding =
        Some(JournalIdentity::from_credential(credential).client_cert_sha256);
    save(state_path, &mut record)
}

/// Restartable replacement commits must rebuild migration ownership from the
/// durable marker sampled before old-credential retirement. Repeating this
/// after a crash is idempotent for the candidate generation.
pub(crate) fn record_pair_replacement_offer(
    state_path: &Path,
    credential: &Credential,
    marker: &MarkerResult,
) -> Result<(), TransportError> {
    let generation = pairing_generation(&credential.client_cert_pem);
    let binding = JournalIdentity::from_credential(credential).client_cert_sha256;
    if load(state_path)?.is_some_and(|record| {
        record.pairing_generation == generation
            && record.fresh_pair_offer_binding.as_deref() == Some(binding.as_str())
    }) {
        return Ok(());
    }
    record_fresh_pair_offer_with_marker(state_path, credential, marker.clone())
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
        || record.fresh_pair_offer_binding.as_deref() != Some(expected_binding)
        || current_binding.as_deref() != Some(expected_binding)
        || paired.credential.as_ref().is_none_or(|credential| {
            pairing_generation(&credential.client_cert_pem) != expected_generation
        })
    {
        return Err(TransportError::ReplayUnsafe);
    }
    if record.fresh_pair_offer_pending {
        record.fresh_pair_offer_pending = false;
        record.fresh_pair_offer_shown = true;
        save(state_path, &mut record)?;
    } else if !record.fresh_pair_offer_shown {
        return Err(TransportError::ReplayUnsafe);
    }
    Ok(())
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

#[cfg(windows)]
pub async fn resume_on_launch(
    state_path: &Path,
    marker: MarkerResult,
    device_label: &str,
    client_label: &str,
) -> Result<bool, TransportError> {
    let paired = PairedState::load(state_path)?;
    let Some(credential) = paired.credential.as_ref() else {
        return Ok(true);
    };
    let answer = read_answer(&answer_path(state_path))
        .map_err(TransportError::from)?
        .unwrap_or_default();
    if !answer.rejected.is_empty() {
        return Ok(true);
    }
    let mut record = load(state_path)?;
    match marker_action(record.as_ref(), &marker) {
        MarkerAction::Continue => return Ok(true),
        MarkerAction::HoldForProbe => return Ok(false),
        MarkerAction::EstablishBaseline => return Ok(true),
        MarkerAction::StartOrResumeMigration => {}
    }
    let current_cid = cid_for_cert(&credential.client_cert_pem)?;
    if record
        .as_ref()
        .is_none_or(|record| !request_is_reusable(record, &marker, &current_cid))
    {
        let generated = crate::credential::generate_csr(device_label)?;
        let current = record
            .take()
            .unwrap_or_else(|| MigrationRecord::baseline(None, marker.clone()));
        record = Some(prepare_rekey(
            state_path,
            current,
            RekeyPreparation {
                marker: &marker,
                paired: &paired,
                answer,
                device_label,
                client_label,
                candidate_key_protected: protect_candidate_key(&generated.key_pem)?,
                candidate: generated,
            },
        )?);
    }
    let mut record = record.ok_or(TransportError::CredentialMalformed)?;
    if record.phase == MigrationPhase::RequestPrepared {
        let request_bytes = saved_request_bytes(&record)?;
        let client = ObserverClient::new(
            credential.clone(),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )?;
        let response = client.rekey(&request_bytes).await?;
        let parsed = record_response(state_path, &mut record, response.status, &response.body)?;
        let candidate = candidate_credential(&record, credential, &parsed)?;
        publish_candidate(state_path, &mut record, &paired, candidate)?;
    } else if record.phase == MigrationPhase::ResponseRecorded {
        let response_bytes = base64::engine::general_purpose::STANDARD
            .decode(
                record
                    .response_bytes_base64
                    .as_deref()
                    .ok_or(TransportError::CredentialMalformed)?,
            )
            .map_err(|_| TransportError::CredentialMalformed)?;
        let parsed: RekeyResponse = serde_json::from_slice(&response_bytes)
            .map_err(|_| TransportError::CredentialMalformed)?;
        let candidate = candidate_credential(&record, credential, &parsed)?;
        publish_candidate(state_path, &mut record, &paired, candidate)?;
    }
    Ok(record.phase == MigrationPhase::Admitted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_marker::{MarkerConfidence, MarkerSource, ProbeStatus};
    use rcgen::{
        BasicConstraints, CertificateParams, IsCa, KeyPair, KeyUsagePurpose, PKCS_ECDSA_P256_SHA256,
    };

    fn marker(value: &str, source: MarkerSource) -> DeviceMarker {
        DeviceMarker {
            digest: spl_core::ca::sha256_hex(value.as_bytes()),
            source,
            confidence: if source == MarkerSource::PublisherSystemId {
                MarkerConfidence::Primary
            } else {
                MarkerConfidence::Fallback
            },
        }
    }

    fn available(value: &str) -> MarkerResult {
        MarkerResult::Available(marker(value, MarkerSource::PublisherSystemId))
    }

    fn fixture_credential() -> Credential {
        Credential {
            client_key_pem: "key".into(),
            client_cert_pem: "certificate".into(),
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

    fn certified_credential() -> Credential {
        let cert = rcgen::generate_simple_self_signed(vec!["device.test".to_owned()]).unwrap();
        let mut credential = fixture_credential();
        credential.client_key_pem = cert.key_pair.serialize_pem();
        credential.client_cert_pem = cert.cert.pem();
        credential
    }

    fn carried_pending_record(state_path: &Path, old_cid: &str, new_cid: &str) -> MigrationRecord {
        let mut record = MigrationRecord::baseline(
            Some(marker("old", MarkerSource::PublisherSystemId)),
            available("old"),
        );
        record.phase = MigrationPhase::Admitted;
        record.server_state = Some(ServerState::Pending);
        record.pairing_generation = [7; 32];
        record.request = Some(RekeyRequest {
            protocol_version: 1,
            operation_id: "123e4567-e89b-42d3-a456-426614174000".into(),
            csr: "saved-csr".into(),
            device_label: "device".into(),
            client_label: "solpbc/solstone-windows".into(),
            platform: "windows".into(),
        });
        record.old_cid = Some(old_cid.to_owned());
        record.old_certificate_binding = Some(format!("sha256:{}", "c".repeat(64)));
        record.new_cid = Some(new_cid.to_owned());
        record.new_certificate_binding = Some(format!("sha256:{}", "d".repeat(64)));
        save(state_path, &mut record).unwrap();
        record
    }

    #[test]
    fn mark_answer_transfer_is_idempotent_but_requires_the_saved_old_confirmation() {
        let old = format!("sha256:{}", "a".repeat(64));
        let new = format!("sha256:{}", "b".repeat(64));
        let saved = AnswerState {
            confirmed: old.clone(),
            rejected: String::new(),
        };
        assert!(answer_can_be_rebound(&saved, &saved, &old, &new));
        assert!(answer_can_be_rebound(
            &saved,
            &AnswerState {
                confirmed: new.clone(),
                rejected: String::new(),
            },
            &old,
            &new
        ));
        assert!(!answer_can_be_rebound(
            &saved,
            &AnswerState {
                confirmed: old.clone(),
                rejected: new.clone(),
            },
            &old,
            &new
        ));
        assert!(!answer_can_be_rebound(
            &AnswerState::default(),
            &saved,
            &old,
            &new
        ));
    }

    #[test]
    fn migration_preserves_a_newer_relay_refresh_and_uses_response_access_without_a_race() {
        let mut latest = fixture_credential();
        latest.client_cert_pem = "new certificate".into();
        latest.relay_origin = Some("https://relay-current".into());
        latest.device_token = Some("fresh-token".into());
        latest.device_token_expires_at = Some(200);
        let mut candidate = fixture_credential();
        candidate.client_cert_pem = "new certificate".into();
        candidate.relay_origin = Some("https://relay-response".into());
        candidate.device_token = Some("response-token".into());
        candidate.device_token_expires_at = Some(100);

        publish_migrated_credential(&mut latest, candidate.clone(), true);

        assert_eq!(
            latest.relay_origin.as_deref(),
            Some("https://relay-current")
        );
        assert_eq!(latest.device_token.as_deref(), Some("fresh-token"));
        assert_eq!(latest.device_token_expires_at, Some(200));

        let mut no_race = fixture_credential();
        publish_migrated_credential(&mut no_race, candidate, false);
        assert_eq!(
            no_race.relay_origin.as_deref(),
            Some("https://relay-response")
        );
        assert_eq!(no_race.device_token.as_deref(), Some("response-token"));
        assert_eq!(no_race.device_token_expires_at, Some(100));
    }

    #[test]
    fn fresh_pair_offer_dismissal_is_fenced_to_binding_generation_and_revision() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("migration-offer-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let state_path = dir.join("pairing.json");
        let credential = certified_credential();
        let binding = JournalIdentity::from_credential(&credential).client_cert_sha256;
        let generation = pairing_generation(&credential.client_cert_pem);
        PairedState {
            credential: Some(credential),
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
        let mut record = MigrationRecord::baseline(None, MarkerResult::Missing);
        record.phase = MigrationPhase::Admitted;
        record.pairing_generation = generation;
        record.fresh_pair_offer_pending = true;
        record.fresh_pair_offer_binding = Some(binding.clone());
        save(&state_path, &mut record).unwrap();
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

    #[test]
    fn marker_actions_hold_probe_failures_and_reconcile_changed_or_missing_markers() {
        assert_eq!(
            marker_action(None, &MarkerResult::Missing),
            MarkerAction::HoldForProbe
        );
        let baseline = MigrationRecord::baseline(
            Some(marker("old", MarkerSource::PublisherSystemId)),
            available("old"),
        );
        assert_eq!(
            marker_action(Some(&baseline), &available("old")),
            MarkerAction::Continue
        );
        assert_eq!(
            marker_action(Some(&baseline), &available("new")),
            MarkerAction::StartOrResumeMigration
        );
        assert_eq!(
            marker_action(Some(&baseline), &MarkerResult::Missing),
            MarkerAction::StartOrResumeMigration
        );
        assert_eq!(
            marker_action(
                Some(&baseline),
                &MarkerResult::ProbeFailure {
                    system_identification: ProbeStatus::Failed,
                    registry_fallback: ProbeStatus::Failed,
                }
            ),
            MarkerAction::HoldForProbe
        );
    }

    #[test]
    fn fresh_pair_with_no_marker_does_not_start_a_carried_credential_migration() {
        let mut record = MigrationRecord::baseline(None, MarkerResult::Missing);
        record.fresh_pair_offer_binding = Some(format!("sha256:{}", "a".repeat(64)));
        assert_eq!(
            marker_action(Some(&record), &MarkerResult::Missing),
            MarkerAction::Continue
        );

        record.fresh_pair_offer_binding = None;
        assert_eq!(
            marker_action(Some(&record), &MarkerResult::Missing),
            MarkerAction::HoldForProbe
        );
    }

    #[test]
    fn fresh_pair_offer_replaces_stale_migration_owner_payload() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "migration-fresh-pair-reset-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state_path = dir.join("pairing.json");
        let mut stale = MigrationRecord::baseline(
            Some(marker("old", MarkerSource::PublisherSystemId)),
            available("old"),
        );
        stale.phase = MigrationPhase::DecisionUnknown;
        stale.request = Some(RekeyRequest {
            protocol_version: 1,
            operation_id: "123e4567-e89b-42d3-a456-426614174000".into(),
            csr: "old-csr".into(),
            device_label: "old-device".into(),
            client_label: "solpbc/solstone-windows".into(),
            platform: "windows".into(),
        });
        stale.old_cid = Some(format!("sha256:{}", "b".repeat(64)));
        stale.new_cid = Some(format!("sha256:{}", "c".repeat(64)));
        stale.pending_decision = Some(SavedDecision {
            request: DecisionRequest {
                protocol_version: 1,
                operation_id: "123e4567-e89b-42d3-a456-426614174000".into(),
                choice: Choice::SameDevice,
                replaces_cid: None,
            },
            request_bytes_base64: "e30=".into(),
            result: "unknown".into(),
        });
        save(&state_path, &mut stale).unwrap();

        let credential = certified_credential();
        let binding = JournalIdentity::from_credential(&credential).client_cert_sha256;
        let current_marker = available("current");
        record_fresh_pair_offer_with_marker(&state_path, &credential, current_marker.clone())
            .unwrap();

        let fresh = load(&state_path).unwrap().unwrap();
        assert_eq!(fresh.revision, 2);
        assert_eq!(fresh.phase, MigrationPhase::Baseline);
        assert_eq!(
            fresh.baseline_marker,
            Some(marker("current", MarkerSource::PublisherSystemId))
        );
        assert_eq!(fresh.marker_probe, current_marker);
        assert!(fresh.request.is_none());
        assert!(fresh.pending_decision.is_none());
        assert!(fresh.old_cid.is_none());
        assert_eq!(
            fresh.new_cid.as_deref(),
            Some(credential_cid(&credential).unwrap().as_str())
        );
        assert!(fresh.fresh_pair_offer_pending);
        assert!(!fresh.fresh_pair_offer_shown);
        assert_eq!(
            fresh.fresh_pair_offer_binding.as_deref(),
            Some(binding.as_str())
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn decision_payload_is_durable_and_replayed_exactly_with_an_exact_replacement_cid() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("migration-decision-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let state_path = dir.join("pairing.json");
        let mut record = MigrationRecord::baseline(
            Some(marker("old", MarkerSource::PublisherSystemId)),
            available("old"),
        );
        record.phase = MigrationPhase::Admitted;
        record.server_state = Some(ServerState::Pending);
        record.new_certificate_binding = Some(format!("sha256:{}", "b".repeat(64)));
        record.old_cid = Some(format!("sha256:{}", "d".repeat(64)));
        record.new_cid = Some(format!("sha256:{}", "e".repeat(64)));
        record.request = Some(RekeyRequest {
            protocol_version: 1,
            operation_id: "123e4567-e89b-42d3-a456-426614174000".into(),
            csr: "csr".into(),
            device_label: "device".into(),
            client_label: "windows".into(),
            platform: "windows".into(),
        });
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
            Some(cid)
        );
        assert_eq!(saved.result, "unknown");
        let request_id = saved.request.operation_id.clone();
        let stale_terminal = MigrationStateResponse {
            protocol_version: 1,
            rekey_operation_id: Some("123e4567-e89b-42d3-a456-426614174001".into()),
            previous_cid: record.old_cid.clone(),
            state: ServerState::ReplacedDevice,
            replaced_cid: Some(format!("sha256:{}", "a".repeat(64))),
        };
        assert_eq!(
            reconcile_decision_state(&state_path, &mut record, &stale_terminal).unwrap(),
            DecisionReconcile::Unknown
        );
        assert_eq!(record.pending_decision.as_ref().unwrap().result, "unknown");
        assert_eq!(
            persist_decision(&state_path, &mut record, Choice::SameDevice, None,)
                .unwrap_err()
                .to_string(),
            TransportError::ReplayUnsafe.to_string()
        );
        assert_eq!(
            record
                .pending_decision
                .as_ref()
                .unwrap()
                .request
                .operation_id,
            request_id
        );
        let terminal = DecisionResponse {
            protocol_version: 1,
            operation_id: request_id,
            state: ServerState::ReplacedDevice,
            previous_cid: record.old_cid.clone(),
            cid: record.new_cid.clone().unwrap(),
            replaced_cid: Some(format!("sha256:{}", "a".repeat(64))),
            display_label: "target".into(),
        };
        mark_decision_terminal(&state_path, &mut record, &terminal).unwrap();
        assert_eq!(
            record.pending_decision.as_ref().unwrap().result,
            "replaced_device"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn fresh_pair_decision_uses_its_own_uuid_without_fabricating_rekey_lineage() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "migration-fresh-decision-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state_path = dir.join("pairing.json");
        let credential = certified_credential();
        record_fresh_pair_offer_with_marker(&state_path, &credential, available("fresh")).unwrap();
        let mut record = load(&state_path).unwrap().unwrap();
        let current_cid = credential_cid(&credential).unwrap();

        assert!(record.request.is_none());
        assert!(record.old_cid.is_none());
        assert_eq!(record.new_cid.as_deref(), Some(current_cid.as_str()));
        assert_eq!(
            record.new_certificate_binding.as_deref(),
            Some(
                JournalIdentity::from_credential(&credential)
                    .client_cert_sha256
                    .as_str()
            )
        );
        let no_rekey_state = MigrationStateResponse {
            protocol_version: 1,
            rekey_operation_id: None,
            previous_cid: None,
            state: ServerState::None,
            replaced_cid: None,
        };
        assert!(matches!(
            persist_decision(&state_path, &mut record, Choice::SameDevice, None),
            Err(TransportError::ReplayUnsafe)
        ));
        assert!(record.pending_decision.is_none());

        persist_decision(&state_path, &mut record, Choice::NewDevice, None).unwrap();
        let exact_bytes = decision_bytes(record.pending_decision.as_ref().unwrap()).unwrap();
        assert_eq!(
            reconcile_decision_state(&state_path, &mut record, &no_rekey_state).unwrap(),
            DecisionReconcile::ReplayExactRequest
        );
        assert_eq!(
            decision_bytes(record.pending_decision.as_ref().unwrap()).unwrap(),
            exact_bytes
        );
        let saved = record.pending_decision.as_ref().unwrap();
        assert_eq!(saved.request.choice, Choice::NewDevice);
        assert!(saved.request.operation_id.contains('-'));
        assert_eq!(
            decision_bytes(saved).unwrap(),
            serde_json::to_vec(&saved.request).unwrap()
        );
        assert_eq!(saved.result, "unknown");
        assert_eq!(record.phase, MigrationPhase::DecisionUnknown);
        assert!(record.request.is_none());
        assert!(record.old_cid.is_none());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn carried_same_device_decision_uses_new_decision_uuid_and_saved_old_lineage() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "migration-carried-same-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state_path = dir.join("pairing.json");
        let old_cid = format!("sha256:{}", "a".repeat(64));
        let new_cid = format!("sha256:{}", "b".repeat(64));
        let mut record = carried_pending_record(&state_path, &old_cid, &new_cid);
        let rekey_id = record.request.as_ref().unwrap().operation_id.clone();

        persist_decision(&state_path, &mut record, Choice::SameDevice, None).unwrap();
        let saved = record.pending_decision.as_ref().unwrap();
        assert_ne!(saved.request.operation_id, rekey_id);
        assert_eq!(saved.request.choice, Choice::SameDevice);
        let exact_bytes = decision_bytes(saved).unwrap();
        let pending = MigrationStateResponse {
            protocol_version: 1,
            rekey_operation_id: Some(rekey_id),
            previous_cid: Some(old_cid.clone()),
            state: ServerState::Pending,
            replaced_cid: None,
        };
        assert_eq!(
            reconcile_decision_state(&state_path, &mut record, &pending).unwrap(),
            DecisionReconcile::ReplayExactRequest
        );
        assert_eq!(
            decision_bytes(record.pending_decision.as_ref().unwrap()).unwrap(),
            exact_bytes
        );

        let response = DecisionResponse {
            protocol_version: 1,
            operation_id: record
                .pending_decision
                .as_ref()
                .unwrap()
                .request
                .operation_id
                .clone(),
            state: ServerState::SameDevice,
            previous_cid: Some(old_cid.clone()),
            cid: new_cid,
            replaced_cid: Some(old_cid),
            display_label: "device".into(),
        };
        mark_decision_terminal(&state_path, &mut record, &response).unwrap();
        assert_eq!(record.server_state, Some(ServerState::SameDevice));
        assert_eq!(
            record.pending_decision.as_ref().unwrap().result,
            "same_device"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn confirmed_stale_target_allows_a_new_durable_replacement_operation() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "migration-target-retry-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state_path = dir.join("pairing.json");
        let credential = certified_credential();
        record_fresh_pair_offer_with_marker(&state_path, &credential, available("fresh")).unwrap();
        let mut record = load(&state_path).unwrap().unwrap();
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
        let target_not_found = br#"{"reason_code":"paired_device_not_found"}"#;
        assert!(record_target_missing(&state_path, &mut record, 404, target_not_found).unwrap());
        assert_eq!(record.phase, MigrationPhase::Baseline);
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
    fn unknown_replacement_keeps_uuid_and_exact_bytes_until_get_proves_a_retry_safe_result() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "migration-target-unknown-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state_path = dir.join("pairing.json");
        let old_cid = format!("sha256:{}", "a".repeat(64));
        let new_cid = format!("sha256:{}", "b".repeat(64));
        let stale_cid = format!("sha256:{}", "c".repeat(64));
        let mut record = carried_pending_record(&state_path, &old_cid, &new_cid);
        persist_decision(
            &state_path,
            &mut record,
            Choice::ReplaceDevice,
            Some(stale_cid.clone()),
        )
        .unwrap();
        let saved = record.pending_decision.as_ref().unwrap();
        let operation_id = saved.request.operation_id.clone();
        let exact_bytes = decision_bytes(saved).unwrap();
        let other_cid = format!("sha256:{}", "e".repeat(64));
        assert!(matches!(
            persist_decision(
                &state_path,
                &mut record,
                Choice::ReplaceDevice,
                Some(other_cid)
            ),
            Err(TransportError::ReplayUnsafe)
        ));
        assert_eq!(
            record
                .pending_decision
                .as_ref()
                .unwrap()
                .request
                .operation_id,
            operation_id
        );
        assert_eq!(
            decision_bytes(record.pending_decision.as_ref().unwrap()).unwrap(),
            exact_bytes
        );
        assert!(record.terminal_decisions.is_empty());

        let pending = MigrationStateResponse {
            protocol_version: 1,
            rekey_operation_id: record
                .request
                .as_ref()
                .map(|request| request.operation_id.clone()),
            previous_cid: Some(old_cid),
            state: ServerState::Pending,
            replaced_cid: None,
        };
        assert_eq!(
            reconcile_decision_state(&state_path, &mut record, &pending).unwrap(),
            DecisionReconcile::ReplayExactRequest
        );
        let reloaded = load(&state_path).unwrap().unwrap();
        let still_saved = reloaded.pending_decision.as_ref().unwrap();
        assert_eq!(still_saved.request.operation_id, operation_id);
        assert_eq!(decision_bytes(still_saved).unwrap(), exact_bytes);
        assert_eq!(still_saved.result, "unknown");
        assert!(reloaded.terminal_decisions.is_empty());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn stale_migration_snapshot_cannot_overwrite_a_newer_durable_revision() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("migration-cas-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let state_path = dir.join("pairing.json");
        let mut initial = MigrationRecord::baseline(
            Some(marker("old", MarkerSource::PublisherSystemId)),
            available("old"),
        );
        save(&state_path, &mut initial).unwrap();

        let mut stale = load(&state_path).unwrap().unwrap();
        let mut latest = load(&state_path).unwrap().unwrap();
        latest.fresh_pair_offer_shown = true;
        save(&state_path, &mut latest).unwrap();
        stale.fresh_pair_offer_pending = true;

        assert!(matches!(
            save(&state_path, &mut stale),
            Err(TransportError::ReplayUnsafe)
        ));
        let durable = load(&state_path).unwrap().unwrap();
        assert!(durable.fresh_pair_offer_shown);
        assert!(!durable.fresh_pair_offer_pending);
        assert_eq!(durable.revision, 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rekey_request_validation_matches_the_pinned_v1_shape() {
        let valid = RekeyRequest {
            protocol_version: 1,
            operation_id: "123e4567-e89b-42d3-a456-426614174000".into(),
            csr: "-----BEGIN CERTIFICATE REQUEST-----\nfixture\n-----END CERTIFICATE REQUEST-----"
                .into(),
            device_label: "Fixture device".into(),
            client_label: "Fixture client".into(),
            platform: "windows".into(),
        };
        assert!(validate_rekey_request(&valid).is_ok());
        let mut invalid = valid;
        invalid.protocol_version = 2;
        assert!(matches!(
            validate_rekey_request(&invalid),
            Err(TransportError::CredentialMalformed)
        ));
    }

    #[test]
    fn same_marker_reuses_saved_rekey_bytes_but_a_new_marker_requires_new_material() {
        let mut record = MigrationRecord::baseline(
            Some(marker("old", MarkerSource::PublisherSystemId)),
            available("old"),
        );
        record.bound_marker = Some(marker("new", MarkerSource::PublisherSystemId));
        record.request = Some(RekeyRequest {
            protocol_version: 1,
            operation_id: "123e4567-e89b-42d3-a456-426614174000".into(),
            csr: "saved-csr".into(),
            device_label: "device".into(),
            client_label: "windows".into(),
            platform: "windows".into(),
        });
        record.request_bytes_base64 = Some(
            base64::engine::general_purpose::STANDARD
                .encode(serde_json::to_vec(record.request.as_ref().unwrap()).unwrap()),
        );
        record.candidate_key_protected = Some("dpapi:v1:saved".into());
        record.candidate_spki_base64 = Some("c3BraQ==".into());
        record.old_cid = Some("sha256:old".into());
        let saved = saved_request_bytes(&record).unwrap();
        assert!(request_is_reusable(
            &record,
            &available("new"),
            "sha256:old"
        ));
        assert_eq!(
            saved,
            base64::engine::general_purpose::STANDARD
                .decode(record.request_bytes_base64.as_ref().unwrap())
                .unwrap()
        );
        assert!(!request_is_reusable(
            &record,
            &available("third"),
            "sha256:old"
        ));
    }

    type CorruptRekeyResponse = (&'static str, fn(&mut Value));

    #[test]
    fn candidate_and_exact_rekey_request_are_durable_before_send_admission() {
        let root = std::env::temp_dir().join(format!(
            "migration-prepared-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let state_path = root.join("pairing.json");
        let certified = rcgen::generate_simple_self_signed(vec!["old.test".to_owned()]).unwrap();
        let mut old = fixture_credential();
        old.client_cert_pem = certified.cert.pem();
        let paired = PairedState {
            credential: Some(old),
            ..Default::default()
        };
        let current_marker = available("new-marker");
        let record = MigrationRecord::baseline(
            Some(marker("old-marker", MarkerSource::PublisherSystemId)),
            available("old-marker"),
        );

        let prepared = prepare_rekey(
            &state_path,
            record,
            RekeyPreparation {
                marker: &current_marker,
                paired: &paired,
                answer: AnswerState::default(),
                device_label: "device",
                client_label: "solpbc/solstone-windows",
                candidate: GeneratedKey {
                    key_pem: "candidate-key".into(),
                    csr_pem: "candidate-csr".into(),
                    public_key_spki_der: vec![1, 2, 3, 4],
                },
                candidate_key_protected: "protected-candidate-key".into(),
            },
        )
        .unwrap();

        let persisted = load(&state_path).unwrap().unwrap();
        let exact_request = serde_json::to_vec(persisted.request.as_ref().unwrap()).unwrap();
        assert_eq!(persisted.phase, MigrationPhase::RequestPrepared);
        assert_eq!(
            persisted.bound_marker,
            Some(marker("new-marker", MarkerSource::PublisherSystemId))
        );
        assert_eq!(
            persisted.candidate_key_protected.as_deref(),
            Some("protected-candidate-key")
        );
        assert_eq!(saved_request_bytes(&persisted).unwrap(), exact_request);
        assert_eq!(
            prepared.request_bytes_base64,
            persisted.request_bytes_base64
        );
        let protected_record = std::fs::read(path_for_state(&state_path)).unwrap();
        let protected_key = b"protected-candidate-key";
        assert!(protected_record
            .windows(protected_key.len())
            .any(|window| window == protected_key));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rekey_response_pins_protocol_operation_lineage_and_cid_before_persisting() {
        let cases: [CorruptRekeyResponse; 5] = [
            ("protocol", |value: &mut Value| {
                value["protocol_version"] = 2.into()
            }),
            ("operation", |value: &mut Value| {
                value["operation_id"] = "123e4567-e89b-42d3-a456-426614174001".into()
            }),
            ("state", |value: &mut Value| {
                value["state"] = "same_device".into()
            }),
            ("previous_cid", |value: &mut Value| {
                value["previous_cid"] =
                    "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into()
            }),
            ("new_cid", |value: &mut Value| {
                value["cid"] = "invalid".into()
            }),
        ];
        for (label, corrupt) in cases {
            let root = std::env::temp_dir().join(format!(
                "migration-response-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&root).unwrap();
            let state_path = root.join("pairing.json");
            let request = RekeyRequest {
                protocol_version: 1,
                operation_id: "123e4567-e89b-42d3-a456-426614174000".into(),
                csr: "saved-csr".into(),
                device_label: "device".into(),
                client_label: "solpbc/solstone-windows".into(),
                platform: "windows".into(),
            };
            let mut record = MigrationRecord::baseline(None, MarkerResult::Missing);
            record.phase = MigrationPhase::RequestPrepared;
            record.request_bytes_base64 = Some(
                base64::engine::general_purpose::STANDARD
                    .encode(serde_json::to_vec(&request).unwrap()),
            );
            record.request = Some(request);
            record.old_cid = Some(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            );
            save(&state_path, &mut record).unwrap();

            let mut response = serde_json::json!({
                "protocol_version": 1,
                "operation_id": "123e4567-e89b-42d3-a456-426614174000",
                "state": "pending",
                "previous_cid": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "cid": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "pairing": {}
            });
            corrupt(&mut response);
            let body = serde_json::to_vec(&response).unwrap();
            assert!(
                matches!(
                    record_response(&state_path, &mut record, 200, &body),
                    Err(TransportError::CredentialMalformed)
                ),
                "{label}"
            );
            let persisted = load(&state_path).unwrap().unwrap();
            assert_eq!(persisted.phase, MigrationPhase::RequestPrepared, "{label}");
            assert!(persisted.response_bytes_base64.is_none(), "{label}");
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn candidate_admission_requires_saved_spki_fingerprint_ca_and_instance() {
        let ca_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages.push(KeyUsagePurpose::KeyCertSign);
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let candidate_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let leaf_params = CertificateParams::new(vec!["device.test".into()]).unwrap();
        let leaf = leaf_params.signed_by(&candidate_key, &ca, &ca_key).unwrap();
        let candidate_key_pem = candidate_key.serialize_pem();
        let cid = format!("sha256:{}", spl_core::ca::sha256_hex(leaf.der()));

        let mut old = fixture_credential();
        old.ca_chain_pem = vec![ca.pem()];
        old.ca_fp_prefix = spl_core::ca::sha256(ca.der())[..16].to_vec();
        let mut record = MigrationRecord::baseline(None, MarkerResult::Missing);
        record.request = Some(RekeyRequest {
            protocol_version: 1,
            operation_id: "123e4567-e89b-42d3-a456-426614174000".into(),
            csr: "saved-csr".into(),
            device_label: "device".into(),
            client_label: "solpbc/solstone-windows".into(),
            platform: "windows".into(),
        });
        record.candidate_spki_base64 =
            Some(base64::engine::general_purpose::STANDARD.encode(candidate_key.public_key_der()));
        record.candidate_key_protected = Some("protected-candidate-key".into());
        record.old_instance_id = Some(old.instance_id.clone());
        let response = RekeyResponse {
            protocol_version: 1,
            operation_id: record.request.as_ref().unwrap().operation_id.clone(),
            state: ServerState::Pending,
            previous_cid: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .into(),
            cid: cid.clone(),
            pairing: serde_json::json!({
                "client_cert": leaf.pem(),
                "ca_chain": [ca.pem()],
                "instance_id": old.instance_id.clone(),
                "home_label": "journal",
                "fingerprint": cid,
                "local_endpoints": [{"ip": "192.0.2.8", "port": 7657}]
            }),
        };

        let admitted =
            candidate_credential_with_key(&record, &old, &response, candidate_key_pem.clone())
                .unwrap();
        assert_eq!(admitted.client_key_pem, candidate_key_pem);
        assert_eq!(admitted.instance_id, "instance");

        let mut bad_spki = record.clone();
        bad_spki.candidate_spki_base64 =
            Some(base64::engine::general_purpose::STANDARD.encode([0u8; 32]));
        assert!(candidate_credential_with_key(
            &bad_spki,
            &old,
            &response,
            candidate_key_pem.clone(),
        )
        .is_err());

        let mut bad_fingerprint = response.clone();
        bad_fingerprint.pairing["fingerprint"] =
            "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc".into();
        assert!(candidate_credential_with_key(
            &record,
            &old,
            &bad_fingerprint,
            candidate_key_pem.clone(),
        )
        .is_err());

        let mut bad_instance = response.clone();
        bad_instance.pairing["instance_id"] = "other-instance".into();
        assert!(candidate_credential_with_key(
            &record,
            &old,
            &bad_instance,
            candidate_key_pem.clone(),
        )
        .is_err());

        let mut bad_ca = response;
        bad_ca.pairing["ca_chain"] = serde_json::json!([]);
        assert!(candidate_credential_with_key(&record, &old, &bad_ca, candidate_key_pem,).is_err());
    }
}
