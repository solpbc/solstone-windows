// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Local upload acknowledgment file persistence and validation.

use serde::{Deserialize, Serialize};

use observer_pl::ingest::{FileDescriptor, SegmentFileStatus};

use crate::credential::{hex_lower, Credential};

pub const LOCAL_UPLOAD_ACK_SCHEMA: &str = "solstone.local-upload-ack.v1";

/// The journal identity pinned to an acknowledgment.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct JournalIdentity {
    pub instance_id: String,
    pub ca_fp_prefix: String,
    pub client_cert_sha256: String,
}

impl JournalIdentity {
    pub fn from_credential(cred: &Credential) -> Self {
        let cert_digest = spl_core::ca::sha256(cred.client_cert_pem.as_bytes());
        Self {
            instance_id: cred.instance_id.clone(),
            ca_fp_prefix: hex_lower(&cred.ca_fp_prefix),
            client_cert_sha256: hex_lower(&cert_digest),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AckProofKind {
    Upload,
    Listing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AckFile {
    pub submitted: String,
    pub written: String,
    pub size: u64,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disposition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listing_status: Option<SegmentFileStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadAck {
    pub schema: String,
    pub journal_identity: JournalIdentity,
    pub day: String,
    pub local_segment: String,
    pub server_segment: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub proof: AckProofKind,
    pub files: Vec<AckFile>,
}

impl UploadAck {
    pub fn new_upload(
        journal_identity: JournalIdentity,
        day: impl Into<String>,
        local_segment: impl Into<String>,
        server_segment: impl Into<String>,
        status: &str,
        receipt_files: &[FileDescriptor],
    ) -> Self {
        Self {
            schema: LOCAL_UPLOAD_ACK_SCHEMA.to_owned(),
            journal_identity,
            day: day.into(),
            local_segment: local_segment.into(),
            server_segment: server_segment.into(),
            status: Some(status.to_owned()),
            proof: AckProofKind::Upload,
            files: receipt_files
                .iter()
                .map(|f| AckFile {
                    submitted: f.submitted.clone(),
                    written: f.written.clone(),
                    size: f.size,
                    sha256: f.sha256.clone(),
                    disposition: Some(f.disposition.clone()),
                    listing_status: None,
                })
                .collect(),
        }
    }

    pub fn new_listing(
        journal_identity: JournalIdentity,
        day: impl Into<String>,
        local_segment: impl Into<String>,
        server_segment: impl Into<String>,
        files: Vec<AckFile>,
    ) -> Self {
        Self {
            schema: LOCAL_UPLOAD_ACK_SCHEMA.to_owned(),
            journal_identity,
            day: day.into(),
            local_segment: local_segment.into(),
            server_segment: server_segment.into(),
            status: None,
            proof: AckProofKind::Listing,
            files,
        }
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec_pretty(self)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        let ack: Self = serde_json::from_slice(bytes)?;
        if ack.schema != LOCAL_UPLOAD_ACK_SCHEMA {
            return Err(serde::de::Error::custom("schema mismatch"));
        }
        Ok(ack)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential::EndpointAddr;

    #[test]
    fn journal_identity_key_order_and_sha256_encoding() {
        let cred = Credential {
            client_key_pem: "KEY".into(),
            client_cert_pem: "CERT_PEM_BYTES".into(),
            ca_chain_pem: vec!["CA".into()],
            ca_fp_prefix: vec![0x12, 0xab, 0xcd],
            instance_id: "inst-42".into(),
            home_label: "Home".into(),
            endpoints: vec![EndpointAddr {
                host: "127.0.0.1".into(),
                port: 1,
            }],
            relay_origin: None,
            device_token: None,
            device_token_expires_at: None,
        };
        let identity = JournalIdentity::from_credential(&cred);
        assert_eq!(identity.instance_id, "inst-42");
        assert_eq!(identity.ca_fp_prefix, "12abcd");
        let expected_sha = hex_lower(&spl_core::ca::sha256(b"CERT_PEM_BYTES"));
        assert_eq!(identity.client_cert_sha256, expected_sha);
        assert_eq!(expected_sha.len(), 64);

        let json = serde_json::to_string(&identity).unwrap();
        assert!(json.starts_with(
            r#"{"instance_id":"inst-42","ca_fp_prefix":"12abcd","client_cert_sha256":""#
        ));
    }
}
