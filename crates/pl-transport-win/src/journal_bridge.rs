// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Windows lifecycle adapter for the shared loopback journal bridge.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use observer_pl::{
    CAP_COOKIE_NAME, OBSERVER_HANDLE_HEADER, PROTOCOL_VERSION_HEADER, UPSTREAM_COOKIE_PREFIX,
};
use spl_core::bridge::BridgeNames;
use spl_transport::client::CarrierOpenError;
use spl_transport::journal_bridge::{
    self as shared_bridge, BridgePolicy, CarrierOpener, JournalBridgeConfig, JournalBridgeStatus,
};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::access::CredentialAccess;
use crate::client::ClientSlot;
use crate::pairing::map_shared_error;
use crate::{
    journal_version::JournalVersionController, post_connect::PostConnectController, TransportError,
};

pub struct JournalBridgeHandle {
    inner: Arc<Mutex<Option<shared_bridge::JournalBridgeHandle>>>,
    status_task: Option<JoinHandle<()>>,
    lifecycle: Arc<BridgeLifecycle>,
}

impl JournalBridgeHandle {
    pub fn port(&self) -> u16 {
        self.inner
            .lock()
            .expect("journal bridge handle lock")
            .as_ref()
            .expect("journal bridge handle is live")
            .port()
    }

    pub fn contacted(&self) -> bool {
        self.inner
            .lock()
            .expect("journal bridge handle lock")
            .as_ref()
            .expect("journal bridge handle is live")
            .contacted()
    }

    pub fn bootstrap_url(&self) -> String {
        self.inner
            .lock()
            .expect("journal bridge handle lock")
            .as_ref()
            .expect("journal bridge handle is live")
            .bootstrap_url()
            .expect("Windows journal bridge always enables capability authorization")
    }

    pub fn begin_shutdown(mut self) {
        self.stop_status_task();
        if let Some(handle) = self
            .inner
            .lock()
            .expect("journal bridge handle lock")
            .take()
        {
            handle.begin_shutdown();
        }
    }

    pub async fn shutdown_and_wait(mut self) {
        self.stop_status_task();
        if let Some(task) = self.status_task.take() {
            let _ = task.await;
        }
        let handle = self
            .inner
            .lock()
            .expect("journal bridge handle lock")
            .take();
        if let Some(handle) = handle {
            let _ = handle.shutdown_and_wait().await;
        }
    }

    fn stop_status_task(&mut self) {
        self.lifecycle.retire();
        if let Some(task) = self.status_task.take() {
            task.abort();
        }
    }
}

impl Drop for JournalBridgeHandle {
    fn drop(&mut self) {
        self.stop_status_task();
    }
}

#[derive(Debug)]
pub enum BridgeStartError {
    NotReady,
    Client(TransportError),
    Bind(std::io::Error),
}

impl From<shared_bridge::BridgeStartError> for BridgeStartError {
    fn from(error: shared_bridge::BridgeStartError) -> Self {
        match error {
            shared_bridge::BridgeStartError::Capability(error) => {
                Self::Client(map_shared_error(error))
            }
            shared_bridge::BridgeStartError::Bind(error) => Self::Bind(error),
        }
    }
}

pub async fn start(access: CredentialAccess) -> Result<JournalBridgeHandle, BridgeStartError> {
    start_observed(access).await
}

pub async fn start_with_facts(
    access: CredentialAccess,
) -> Result<JournalBridgeHandle, BridgeStartError> {
    start_observed(access).await
}

pub async fn start_observed(
    access: CredentialAccess,
) -> Result<JournalBridgeHandle, BridgeStartError> {
    start_observed_with_facts(access).await
}

pub async fn start_observed_with_facts(
    access: CredentialAccess,
) -> Result<JournalBridgeHandle, BridgeStartError> {
    let client_slot = access.client_slot();
    let endpoint_hosts = client_slot
        .load()
        .credential()
        .endpoints
        .iter()
        .map(|endpoint| endpoint.host.clone())
        .collect();
    let opener = Arc::new(WindowsCarrierOpener { client_slot });
    let config = JournalBridgeConfig {
        opener,
        bridge_names: BridgeNames {
            capability_cookie_name: CAP_COOKIE_NAME.into(),
            upstream_cookie_prefix: UPSTREAM_COOKIE_PREFIX.into(),
            observer_header_name: OBSERVER_HANDLE_HEADER.to_ascii_lowercase(),
            protocol_version_header_name: PROTOCOL_VERSION_HEADER.to_ascii_lowercase(),
        },
        endpoint_hosts,
        policy: BridgePolicy::default(),
    };
    let handle = shared_bridge::start(config)
        .await
        .map_err(BridgeStartError::from)?;
    let subscription = handle.subscribe_status();
    let lifecycle = Arc::new(BridgeLifecycle::new(&access));
    let inner = Arc::new(Mutex::new(Some(handle)));
    let task_lifecycle = lifecycle.clone();
    let task_inner = inner.clone();
    let status_task = tokio::spawn(async move {
        task_lifecycle.apply(subscription.initial());
        run_status_subscription(subscription, task_lifecycle, task_inner).await;
    });

    Ok(JournalBridgeHandle {
        inner,
        status_task: Some(status_task),
        lifecycle,
    })
}

struct WindowsCarrierOpener {
    client_slot: ClientSlot,
}

impl CarrierOpener for WindowsCarrierOpener {
    fn proxy_headers(
        &self,
        upstream_headers: &[(String, String)],
    ) -> Result<Vec<(String, String)>, spl_transport::TransportError> {
        Ok(self.client_slot.proxy_headers(upstream_headers))
    }

    fn dial_carrier(
        &self,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<spl_transport::DialedCarrier, spl_transport::TransportError>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            let client = self.client_slot.load();
            client
                .transport_client()
                .open_carrier(client.operation_observer())
                .await
                .map_err(map_carrier_open_error)
        })
    }
}

fn map_carrier_open_error(error: CarrierOpenError) -> spl_transport::TransportError {
    match error {
        CarrierOpenError::RelayDisabled
        | CarrierOpenError::RelayRetired
        | CarrierOpenError::PublicationRejected
        | CarrierOpenError::PublicationIndeterminate => spl_transport::TransportError::NoEndpoint,
        CarrierOpenError::Transport(error) => error,
    }
}

struct BridgeLifecycle {
    active: AtomicBool,
    carrier_live: AtomicBool,
    journal_version: Arc<JournalVersionController>,
    post_connect: Arc<PostConnectController>,
    journal_version_token: crate::JournalVersionSessionToken,
    post_connect_token: crate::PostConnectSessionToken,
    sync: Arc<Mutex<observer_model::SyncSnapshot>>,
    client_slot: ClientSlot,
}

impl BridgeLifecycle {
    fn new(access: &CredentialAccess) -> Self {
        Self {
            active: AtomicBool::new(true),
            carrier_live: AtomicBool::new(false),
            journal_version: access.journal_version(),
            post_connect: access.post_connect(),
            journal_version_token: access.journal_version_token(),
            post_connect_token: access.post_connect_token(),
            sync: access.sync(),
            client_slot: access.client_slot(),
        }
    }

    fn apply(&self, status: JournalBridgeStatus) {
        if !self.active.load(Ordering::Acquire) {
            return;
        }
        if let Ok(mut snapshot) = self.sync.lock() {
            if status.terminal_reason.is_some() {
                // The journal refused this device, or refusals went on too long:
                // the bridge has stopped dialing for this pairing.
                snapshot.pairing.phase = observer_model::PairingPhase::Failed;
                snapshot.pairing.detail =
                    Some(crate::coordinator::PAIRING_REFUSED_DETAIL.to_string());
            }
            let client = self.client_slot.load();
            crate::unknown_journals::publish_unknown_journals(
                &mut snapshot,
                client.transport_client(),
                &client.credential().instance_id,
            );
        }
        let was_live = self
            .carrier_live
            .swap(status.carrier_live, Ordering::AcqRel);
        if was_live && !status.carrier_live {
            self.mark_disconnected();
        } else if !was_live && status.carrier_live {
            self.post_connect.note_connected(self.post_connect_token);
        }
    }

    fn mark_unknown(&self) {
        self.carrier_live.store(false, Ordering::Release);
        self.mark_disconnected();
    }

    fn retire(&self) {
        if self.active.swap(false, Ordering::AcqRel) {
            self.carrier_live.store(false, Ordering::Release);
            self.mark_disconnected();
        }
    }

    fn mark_disconnected(&self) {
        self.journal_version
            .mark_session_disconnected(self.journal_version_token, &self.sync);
        self.post_connect
            .mark_session_disconnected(self.post_connect_token);
    }
}

async fn run_status_subscription(
    mut subscription: shared_bridge::JournalBridgeStatusSubscription,
    lifecycle: Arc<BridgeLifecycle>,
    inner: Arc<Mutex<Option<shared_bridge::JournalBridgeHandle>>>,
) {
    loop {
        match subscription.recv().await {
            Ok(status) => lifecycle.apply(status),
            Err(broadcast::error::RecvError::Lagged(_)) => {
                lifecycle.mark_unknown();
                let snapshot = inner
                    .lock()
                    .expect("journal bridge handle lock")
                    .as_ref()
                    .map(shared_bridge::JournalBridgeHandle::status);
                if let Some(snapshot) = snapshot {
                    lifecycle.apply(snapshot);
                }
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carrier_open_fences_collapse_only_at_the_bridge_opener_boundary() {
        for error in [
            CarrierOpenError::RelayDisabled,
            CarrierOpenError::RelayRetired,
            CarrierOpenError::PublicationRejected,
            CarrierOpenError::PublicationIndeterminate,
        ] {
            assert!(matches!(
                map_carrier_open_error(error),
                spl_transport::TransportError::NoEndpoint
            ));
        }
        assert!(matches!(
            map_carrier_open_error(CarrierOpenError::Transport(
                spl_transport::TransportError::TlsAccessDenied
            )),
            spl_transport::TransportError::TlsAccessDenied
        ));
    }
}
