// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Shared ownership of one paired credential transport authority.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use observer_model::SyncSnapshot;

use crate::client::ClientSlot;
use crate::credential::{pairing_generation, CasKey, PairedState};
use crate::journal_version::{JournalVersionController, JournalVersionSessionToken};
use crate::post_connect::{PostConnectController, PostConnectSessionToken};
use crate::service::SyncConfig;
use crate::{ObserverClient, ObserverHandle, TransportError};

/// Load the current client immediately before a send, including replacements
/// of the credential authority at the same journal.
pub async fn load_current_client(
    access: &tokio::sync::Mutex<Option<CredentialAccess>>,
) -> Option<Arc<ObserverClient>> {
    access.lock().await.as_ref().map(|a| a.client_slot().load())
}

/// The one live authority for a paired credential file.
///
/// Cloning this type is cheap: all consumers use the same replacement slot,
/// post-connect controller, and session identities.
#[derive(Clone)]
pub struct CredentialAccess {
    client_slot: ClientSlot,
    post_connect: Arc<PostConnectController>,
    state_path: PathBuf,
    post_connect_token: PostConnectSessionToken,
    journal_version_token: JournalVersionSessionToken,
    journal_version: Arc<JournalVersionController>,
    sync: Arc<Mutex<SyncSnapshot>>,
    confirmation: Arc<Mutex<String>>,
    tombstone: Arc<Mutex<Option<String>>>,
}

impl CredentialAccess {
    pub fn bind(
        paired: &PairedState,
        cfg: &SyncConfig,
        sync: Arc<Mutex<SyncSnapshot>>,
        observer: ObserverHandle,
    ) -> Result<Self, TransportError> {
        if paired.retirement_intent.is_some() {
            return Err(TransportError::Pairing(
                "a client retirement is still pending".to_owned(),
            ));
        }
        let credential = paired.credential.clone().ok_or(TransportError::NotPaired)?;
        let binding = crate::ack::JournalIdentity::from_credential(&credential).client_cert_sha256;
        let confirmed = cfg.confirmation.lock().unwrap();
        let gate_open = !confirmed.is_empty() && *confirmed == binding;
        let gate = Arc::new(std::sync::atomic::AtomicBool::new(gate_open));
        let client = ObserverClient::new(credential.clone(), gate)?
            .with_state_path(cfg.state_path.clone())
            .with_cas_key(CasKey {
                pairing_generation: pairing_generation(&credential.client_cert_pem),
                access_mutation_generation: paired.access_mutation_generation,
            })
            .with_observer(observer);
        let client_slot = ClientSlot::new(Arc::new(client));
        let post_connect = Arc::new(PostConnectController::new(
            client_slot.clone(),
            Some(cfg.state_path.clone()),
            Some(cfg.journal_version.clone()),
            sync.clone(),
            cfg.facts_fn.clone(),
        ));
        let journal_version_token = cfg.journal_version.begin_session(&credential, &sync);
        let post_connect_token = post_connect.begin_session(&credential);
        post_connect.set_journal_version_token(journal_version_token);

        Ok(Self {
            client_slot,
            post_connect,
            state_path: cfg.state_path.clone(),
            post_connect_token,
            journal_version_token,
            journal_version: cfg.journal_version.clone(),
            sync,
            confirmation: cfg.confirmation.clone(),
            tombstone: cfg.tombstone.clone(),
        })
    }

    pub fn client_slot(&self) -> ClientSlot {
        self.client_slot.clone()
    }

    pub fn post_connect(&self) -> Arc<PostConnectController> {
        self.post_connect.clone()
    }

    pub fn state_path(&self) -> &PathBuf {
        &self.state_path
    }

    pub fn post_connect_token(&self) -> PostConnectSessionToken {
        self.post_connect_token
    }

    pub fn journal_version_token(&self) -> JournalVersionSessionToken {
        self.journal_version_token
    }

    pub(crate) fn journal_version(&self) -> Arc<JournalVersionController> {
        self.journal_version.clone()
    }

    pub(crate) fn sync(&self) -> Arc<Mutex<SyncSnapshot>> {
        self.sync.clone()
    }

    pub(crate) fn confirmation(&self) -> Arc<Mutex<String>> {
        self.confirmation.clone()
    }

    pub(crate) fn tombstone(&self) -> Arc<Mutex<Option<String>>> {
        self.tombstone.clone()
    }

    /// Retire this authority before publishing a same-home re-pair replacement.
    pub fn retire(&self) {
        self.client_slot.retire();
        self.post_connect.disconnect_relay();
        self.post_connect
            .mark_session_disconnected(self.post_connect_token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential::{Credential, RetirementIntent, RetirementPhase};
    use crate::device_metadata::RawDeviceFacts;
    use crate::journal_version::JournalVersionController;
    use crate::service::SyncConfig;

    #[derive(Debug)]
    struct FixedOffset;

    impl observer_model::LocalOffset for FixedOffset {
        fn local_zone(
            &self,
            _epoch_secs: u64,
        ) -> Result<observer_model::LocalZone, observer_model::LocalOffsetError> {
            Ok(observer_model::LocalZone {
                tz: Some("UTC".to_owned()),
                utc_offset_seconds: 0,
            })
        }
    }

    #[test]
    fn bind_refuses_an_unresolved_retirement_before_constructing_a_client() {
        let dir = std::env::temp_dir().join(format!(
            "access-retirement-pending-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let rejected = Credential {
            client_key_pem: "key".into(),
            client_cert_pem: "rejected-cert".into(),
            ca_chain_pem: vec!["ca".into()],
            ca_fp_prefix: vec![1, 2, 3],
            instance_id: "journal".into(),
            home_label: "journal".into(),
            endpoints: Vec::new(),
            relay_origin: None,
            device_token: None,
            device_token_expires_at: None,
        };
        let paired = PairedState {
            credential: Some(rejected.clone()),
            retirement_intent: Some(RetirementIntent {
                schema: 1,
                operation_id: "pending".into(),
                phase: RetirementPhase::Prepared,
                owner_generation: pairing_generation(&rejected.client_cert_pem),
                access_mutation_generation: 0,
                client_id: "sha256:fixture".into(),
                credential: rejected,
            }),
            ..Default::default()
        };
        let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
        let cfg = SyncConfig {
            device_label: "device".into(),
            period_secs: 300,
            state_path: dir.join("pairing.json"),
            segments_root: dir.join("segments"),
            local_offset: Arc::new(FixedOffset),
            journal_version: Arc::new(JournalVersionController::new(
                dir.join("journal-version.json"),
            )),
            facts_fn: Arc::new(|| RawDeviceFacts {
                name: None,
                platform: None,
                device_type: None,
                app_id: None,
                app_version: None,
            }),
            confirmation: Arc::new(Mutex::new(String::new())),
            tombstone: Arc::new(Mutex::new(None)),
            #[cfg(feature = "awaiting-hold")]
            awaiting_hold: None,
        };

        assert!(matches!(
            CredentialAccess::bind(&paired, &cfg, sync, None),
            Err(TransportError::Pairing(_))
        ));
        let _ = std::fs::remove_dir_all(dir);
    }
}
