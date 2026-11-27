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
use spl_core::bridge::{BridgeNames, RequestHead};
use spl_transport::client::CarrierOpenError;
use spl_transport::journal_bridge::{
    self as shared_bridge, BootstrapMode, BridgePolicy, BrowserAuthorizationError, CarrierOpener,
    JournalBridgeConfig, JournalBridgeStatus, LocalResponse,
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

    /// Issue a single-use bootstrap URL that redirects directly to `path`
    /// without first loading `/` (eliminating the cold-open `/` flash).
    pub fn issue_url_for_path(&self, path: &str) -> Result<String, BrowserAuthorizationError> {
        let authorizer = self
            .inner
            .lock()
            .expect("journal bridge handle lock")
            .as_ref()
            .expect("journal bridge handle is live")
            .browser_authorizer()
            .ok_or(BrowserAuthorizationError::InvalidPath)?;
        authorizer.issue_url_for_path(path)
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
        self.lifecycle.retire();
        if let Some(task) = self.status_task.take() {
            task.abort();
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
    AwaitingConfirmation,
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

const JOURNAL_ARCHIVE_EXPORT_PATH: &str = "/app/import/api/journal-archive/export";

pub fn is_export_download_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/app/export/api/ready/") else {
        return false;
    };
    let Some(export_id) = rest.strip_suffix("/download") else {
        return false;
    };
    !export_id.is_empty()
        && export_id.len() <= 128
        && export_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn streams_journal_response(head: &RequestHead) -> bool {
    (head.method == "GET" && matches!(head.path(), "/sse/events" | JOURNAL_ARCHIVE_EXPORT_PATH))
        || (matches!(head.method.as_str(), "GET" | "HEAD") && is_export_download_path(head.path()))
}

fn default_policy() -> BridgePolicy {
    BridgePolicy {
        stream_response: Arc::new(streams_journal_response),
        ..BridgePolicy::default()
    }
}

pub async fn start_observed_with_facts(
    access: CredentialAccess,
) -> Result<JournalBridgeHandle, BridgeStartError> {
    start_with_policy(access, default_policy()).await
}

/// Start a loopback journal bridge for the native WebView2 journal window:
/// uses single-use path-bound bootstrap tokens (`BootstrapMode::OneTimeToken`)
/// to avoid the cold-open `/` flash, streams SSE and journal archive export
/// downloads without buffering the full response in memory, and serves the
/// calm offline page at `/_solstone/offline` locally without hitting upstream.
pub async fn start_for_window(
    access: CredentialAccess,
    offline_html: Arc<dyn Fn(&str) -> String + Send + Sync>,
) -> Result<JournalBridgeHandle, BridgeStartError> {
    let policy = BridgePolicy {
        bootstrap_mode: BootstrapMode::OneTimeToken,
        local_response: Arc::new(move |head, _status| {
            if head.method == "GET" && head.path() == "/_solstone/offline" {
                Some(LocalResponse {
                    status: 200,
                    content_type: "text/html; charset=utf-8".into(),
                    body: offline_html(&head.target).into_bytes(),
                })
            } else {
                None
            }
        }),
        ..default_policy()
    };
    start_with_policy(access, policy).await
}

async fn start_with_policy(
    access: CredentialAccess,
    policy: BridgePolicy,
) -> Result<JournalBridgeHandle, BridgeStartError> {
    let client_slot = access.client_slot();
    let client = client_slot.load();
    if !client.gate_open() {
        return Err(BridgeStartError::AwaitingConfirmation);
    }
    let endpoint_hosts = client
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
        policy,
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
    confirmation: Arc<Mutex<String>>,
    tombstone: Arc<Mutex<Option<String>>>,
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
            confirmation: access.confirmation(),
            tombstone: access.tombstone(),
        }
    }

    fn apply(&self, status: JournalBridgeStatus) {
        if !self.active.load(Ordering::Acquire) {
            return;
        }
        if status.terminal_reason.is_some() {
            let client = self.client_slot.load();
            let binding = client.journal_identity().client_cert_sha256;
            let label = client.home_label().to_string();
            let mark = crate::service::journal_mark(&crate::answer::mark_jid(client.credential()));
            crate::service::publish_pairing(
                &self.sync,
                &self.confirmation,
                &self.tombstone,
                crate::service::PairingWrite::Bound {
                    binding,
                    label,
                    mark,
                    kind: crate::service::BoundKind::Failed {
                        detail: Some(crate::coordinator::PAIRING_REFUSED_DETAIL.to_string()),
                    },
                },
            );
        }
        if let Ok(mut snapshot) = self.sync.lock() {
            let client = self.client_slot.load();
            crate::unknown_journals::publish_unknown_journals(
                &mut snapshot,
                client.transport_client(),
                &crate::answer::mark_jid(client.credential()),
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
        if !self.active.load(Ordering::Acquire) {
            return;
        }
        self.carrier_live.store(false, Ordering::Release);
        self.mark_disconnected();
    }

    fn retire(&self) {
        if self.active.swap(false, Ordering::AcqRel) {
            self.carrier_live.store(false, Ordering::Release);
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

    #[test]
    fn streams_only_sse_and_exact_journal_archive_export_get() {
        let req = |method: &str, target: &str| RequestHead {
            method: method.into(),
            target: target.into(),
            headers: Vec::new(),
        };
        assert!(streams_journal_response(&req("GET", "/sse/events")));
        assert!(streams_journal_response(&req(
            "GET",
            "/app/import/api/journal-archive/export"
        )));
        assert!(streams_journal_response(&req(
            "GET",
            "/app/import/api/journal-archive/export?id=prepared"
        )));
        assert!(streams_journal_response(&req(
            "GET",
            "/app/export/api/ready/exp_20261126_abc-1/download"
        )));
        assert!(streams_journal_response(&req(
            "HEAD",
            "/app/export/api/ready/exp_20261126_abc-1/download"
        )));
        assert!(!streams_journal_response(&req(
            "POST",
            "/app/export/api/ready/exp_20261126_abc-1/download"
        )));
        assert!(!streams_journal_response(&req(
            "GET",
            "/app/export/api/ready/../download"
        )));
        assert!(!streams_journal_response(&req(
            "HEAD",
            "/app/import/api/journal-archive/export"
        )));
        assert!(!streams_journal_response(&req(
            "POST",
            "/app/import/api/journal-archive/export"
        )));
        assert!(!streams_journal_response(&req(
            "GET",
            "/app/import/api/journal-archive/export/extra"
        )));
        assert!(!streams_journal_response(&req(
            "GET",
            "/app/import/api/journal-archive/preview"
        )));
        assert!(!streams_journal_response(&req("GET", "/")));
    }
}
