// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Shared ownership of one paired credential transport authority.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use observer_model::SyncSnapshot;

use crate::client::ClientSlot;
use crate::credential::{pairing_generation, CasKey, PairedState};
use crate::journal_version::{JournalVersionController, JournalVersionSessionToken};
use crate::observe::ObserverHandle;
use crate::post_connect::{PostConnectController, PostConnectSessionToken};
use crate::service::SyncConfig;
use crate::{ObserverClient, TransportError};

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
}

impl CredentialAccess {
    pub fn bind(
        paired: &PairedState,
        cfg: &SyncConfig,
        sync: Arc<Mutex<SyncSnapshot>>,
        observer: ObserverHandle,
    ) -> Result<Self, TransportError> {
        let credential = paired.credential.clone().ok_or(TransportError::NotPaired)?;
        let client = ObserverClient::new(credential.clone())?
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

    /// Retire this authority before publishing a same-home re-pair replacement.
    pub fn retire(&self) {
        self.client_slot.retire();
        self.post_connect.disconnect_relay();
        self.post_connect
            .mark_session_disconnected(self.post_connect_token);
    }
}
