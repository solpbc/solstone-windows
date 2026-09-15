// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The observer client over established mTLS.
//!
//! Wraps a paired [`Credential`] and speaks the linked-device protocol-v3
//! endpoints. Those endpoints authenticate exclusively with the mTLS peer and
//! send only their protocol header.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use observer_model::TransportPath;
use observer_pl::http::HttpResponse;
use observer_pl::ingest::{
    DayManifest, FilePart, IngestManifest, IngestMultipart, IngestResponse, IngestStatus,
    SegmentsEnvelope,
};
use observer_pl::{OBSERVER_HANDLE_HEADER, OBSERVER_PROTOCOL_VERSION, PROTOCOL_VERSION_HEADER};
use rustls::ClientConfig;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::connection::dial_tls;
#[cfg(test)]
use crate::credential::FS_FAIL_POINT;
use crate::credential::{
    pairing_generation, CasKey, Credential, PairedState, StorageError, WindowsTokenTransaction,
};
use crate::ordinary_request::OrdinaryRequest;
use crate::pairing::{map_request_error, map_shared_error, windows_to_shared_credential};
use crate::relay::{dial_relay_carrier, RelayTerminationHandle};
use crate::relay_token::{refresh_device_token, RefreshOutcome};
use crate::{tls, ObserverHandle, RelayError, TransportError};
use spl_transport::client::{RelayFence as SharedRelayFence, RelayPermit, TokenPublication};
use spl_transport::request::{RequestOptions, RequestOutcome, SelectedPath};

/// Relay transient retry count. Mirrors the LAN connection/handshake retry bound.
const RELAY_MAX_TRANSIENT_ATTEMPTS: usize = 5;

/// Maximum allowed response bytes for post-connect metadata/access endpoints (64 KiB).
pub(crate) const MAX_POST_CONNECT_RESPONSE_BYTES: usize = 64 * 1024;

/// Process-local relay authority shared by every incarnation in one client slot.
///
/// A stale `Arc<ObserverClient>` can still be held by a request after a live
/// replacement. The incarnation check keeps that retired client from dialing
/// relay with credentials that no longer belong to the current slot.
pub(crate) struct RelayFence {
    disabled: AtomicBool,
    retired: AtomicBool,
    incarnation: AtomicU64,
    publication: Arc<std::sync::Mutex<()>>,
}

impl RelayFence {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            disabled: AtomicBool::new(!enabled),
            retired: AtomicBool::new(false),
            incarnation: AtomicU64::new(1),
            publication: Arc::new(std::sync::Mutex::new(())),
        }
    }

    pub(crate) fn allows(&self, incarnation: u64) -> bool {
        !self.retired.load(Ordering::Acquire)
            && !self.disabled.load(Ordering::Acquire)
            && self.incarnation.load(Ordering::Acquire) == incarnation
    }

    fn advance_from(&self, expected: u64, enabled: bool) -> Option<u64> {
        let next = expected.wrapping_add(1);
        self.incarnation
            .compare_exchange(expected, next, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        self.disabled.store(!enabled, Ordering::Release);
        Some(next)
    }

    pub(crate) fn disable(&self) {
        self.disabled.store(true, Ordering::Release);
        self.incarnation.fetch_add(1, Ordering::AcqRel);
    }

    /// Fence relay for the current client incarnation after a publication
    /// rejection without changing the lifecycle incarnation.
    pub(crate) fn mark_relay_ineligible(&self, incarnation: u64) {
        if self.incarnation.load(Ordering::Acquire) == incarnation {
            self.disabled.store(true, Ordering::Release);
        }
    }
}

impl SharedRelayFence for RelayFence {
    fn permit(&self, incarnation: u64) -> RelayPermit {
        if self.retired.load(Ordering::Acquire) {
            RelayPermit::Retired
        } else if self.disabled.load(Ordering::Acquire) {
            RelayPermit::Disabled
        } else if self.incarnation.load(Ordering::Acquire) != incarnation {
            RelayPermit::Retired
        } else {
            RelayPermit::Allow
        }
    }

    fn with_publication(&self, body: &mut dyn FnMut()) {
        let _guard = self.publication.lock().unwrap();
        body();
    }
}

/// A thread-safe container for the active `ObserverClient` allowing atomic live replacement.
#[derive(Clone)]
pub struct ClientSlot {
    inner: Arc<std::sync::RwLock<Arc<ObserverClient>>>,
    relay_fence: Arc<RelayFence>,
}

impl ClientSlot {
    pub fn new(initial: Arc<ObserverClient>) -> Self {
        Self {
            relay_fence: initial.relay_fence.clone(),
            inner: Arc::new(std::sync::RwLock::new(initial)),
        }
    }

    pub(crate) fn is_retired(&self) -> bool {
        self.relay_fence.retired.load(Ordering::Acquire)
    }

    /// Serialize lifecycle transitions with a shared durable token publication.
    pub fn publication_owner(&self) -> Arc<std::sync::Mutex<()>> {
        self.relay_fence.publication.clone()
    }

    pub fn load(&self) -> Arc<ObserverClient> {
        self.inner.read().unwrap().clone()
    }

    pub fn replace(&self, new_client: Arc<ObserverClient>) {
        let mut guard = self.inner.write().unwrap();
        *guard = new_client;
    }

    /// Rebuild the sole live client from its incumbent while retaining the
    /// observer seam, state path, pairing identity, and shared relay fence.
    pub fn replace_from_incumbent(
        &self,
        credential: Credential,
        cas_key: CasKey,
    ) -> Result<Arc<ObserverClient>, TransportError> {
        if self.relay_fence.retired.load(Ordering::Acquire) {
            return Err(TransportError::NotPaired);
        }
        let enabled = credential.relay_origin.is_some() && credential.device_token.is_some();
        loop {
            let incumbent = self.load();
            let current = self.relay_fence.incarnation.load(Ordering::Acquire);
            let incarnation = current.wrapping_add(1);
            let replacement =
                ObserverClient::rebuild_from(&incumbent, credential.clone(), cas_key, incarnation)?;
            if self.relay_fence.retired.load(Ordering::Acquire) {
                return Err(TransportError::NotPaired);
            }
            if self.relay_fence.advance_from(current, enabled) != Some(incarnation) {
                continue;
            }
            let replacement = Arc::new(replacement);
            self.replace(replacement.clone());
            return Ok(replacement);
        }
    }

    /// Make relay unavailable before any durable operation. LAN remains usable.
    /// Permanently retire this slot's relay incarnation.
    pub fn retire(&self) {
        self.relay_fence.retired.store(true, Ordering::Release);
        self.relay_fence.disable();
    }

    pub fn disable_relay(&self) {
        self.relay_fence.disable();
    }

    pub fn proxy_headers(&self, upstream_headers: &[(String, String)]) -> Vec<(String, String)> {
        self.load().proxy_headers(upstream_headers)
    }
}

enum RefreshAction {
    Redial,
    Terminal,
    Transient,
}

pub(crate) trait CarrierIo: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin> CarrierIo for T {}

pub(crate) struct DialedCarrier {
    pub(crate) stream: Box<dyn CarrierIo>,
    pub(crate) kind: CarrierKind,
}

pub(crate) enum CarrierKind {
    Lan,
    Relay { termination: RelayTerminationHandle },
}

impl From<&CarrierKind> for TransportPath {
    fn from(kind: &CarrierKind) -> Self {
        match kind {
            CarrierKind::Lan => Self::Direct,
            CarrierKind::Relay { .. } => Self::Relay,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendMetadata {
    pub path: TransportPath,
    pub attempts: u32,
}

/// An observer talking to its paired journal over framed-mTLS.
pub struct ObserverClient {
    credential: Credential,
    config: Arc<ClientConfig>,
    boundary_counter: AtomicU64,
    /// Live relay device-token used for dials; the mutex is the refresh single-flight gate.
    device_token: Option<Arc<tokio::sync::Mutex<String>>>,
    refresh_gate: tokio::sync::Mutex<()>,
    /// Optional persisted pairing state path for best-effort refreshed-token write-back.
    state_path: Arc<std::sync::Mutex<Option<PathBuf>>>,
    /// CAS key tracking the pairing and access mutation generation.
    cas_key: Arc<std::sync::Mutex<Option<CasKey>>>,
    /// Optional operation-scoped observation seam. `None` in the GUI.
    observer: ObserverHandle,
    relay_fence: Arc<RelayFence>,
    incarnation: u64,
    pub(crate) token_transaction: Arc<WindowsTokenTransaction>,
    transport: spl_transport::TransportClient,
}

impl ObserverClient {
    /// Build the client and its mTLS config from a stored credential.
    pub fn new(credential: Credential) -> Result<Self, TransportError> {
        let pairing_gen = pairing_generation(&credential.client_cert_pem);
        let device_token = credential
            .device_token
            .clone()
            .map(|token| Arc::new(tokio::sync::Mutex::new(token)));
        let chain = tls::parse_certs(&credential.client_cert_pem)?;
        let key = tls::parse_private_key(&credential.client_key_pem)?;
        let config = Arc::new(tls::mtls_config(&credential.ca_fp_prefix, chain, key)?);
        let relay_enabled = credential.relay_origin.is_some() && credential.device_token.is_some();
        Self::build(
            credential,
            config,
            device_token,
            Arc::new(std::sync::Mutex::new(None)),
            Arc::new(std::sync::Mutex::new(Some(CasKey {
                pairing_generation: pairing_gen,
                access_mutation_generation: 0,
            }))),
            None,
            Arc::new(RelayFence::new(relay_enabled)),
            1,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        credential: Credential,
        config: Arc<ClientConfig>,
        device_token: Option<Arc<tokio::sync::Mutex<String>>>,
        state_path: Arc<std::sync::Mutex<Option<PathBuf>>>,
        cas_key: Arc<std::sync::Mutex<Option<CasKey>>>,
        observer: ObserverHandle,
        relay_fence: Arc<RelayFence>,
        incarnation: u64,
    ) -> Result<Self, TransportError> {
        let transaction = Arc::new(WindowsTokenTransaction::new(
            state_path.clone(),
            pairing_generation(&credential.client_cert_pem),
            relay_fence.clone(),
            incarnation,
        ));
        let publication = TokenPublication {
            transaction: transaction.clone(),
            fence: Some(relay_fence.clone()),
            incarnation,
        };
        let shared_credential = windows_to_shared_credential(&credential);
        let transport = if !shared_credential.endpoints.is_empty() {
            spl_transport::TransportClient::new_with_publication(shared_credential, publication)
        } else if matches!(
            (
                shared_credential.relay_origin.as_deref(),
                shared_credential.device_token.as_deref()
            ),
            (Some(origin), Some(token)) if !origin.is_empty() && !token.is_empty()
        ) {
            spl_transport::TransportClient::new_relay_only_with_publication(
                shared_credential,
                publication,
            )
        } else {
            return Err(TransportError::NoEndpoint);
        }
        .map_err(map_shared_error)?;

        Ok(Self {
            credential,
            config,
            boundary_counter: AtomicU64::new(1),
            device_token,
            refresh_gate: tokio::sync::Mutex::new(()),
            state_path,
            cas_key,
            observer,
            relay_fence,
            incarnation,
            token_transaction: transaction,
            transport,
        })
    }

    fn rebuild_from(
        incumbent: &ObserverClient,
        access_credential: Credential,
        cas_key: CasKey,
        incarnation: u64,
    ) -> Result<Self, TransportError> {
        // Access updates must not replace pairing material. This is also what
        // makes a held pre-replacement client unambiguously retired.
        let mut credential = incumbent.credential.clone();
        credential.relay_origin = access_credential.relay_origin;
        credential.device_token = access_credential.device_token;
        credential.device_token_expires_at = access_credential.device_token_expires_at;
        let device_token = credential
            .device_token
            .clone()
            .map(|token| Arc::new(tokio::sync::Mutex::new(token)));
        Self::build(
            credential,
            incumbent.config.clone(),
            device_token,
            incumbent.state_path.clone(),
            Arc::new(std::sync::Mutex::new(Some(cas_key))),
            incumbent.observer.clone(),
            incumbent.relay_fence.clone(),
            incarnation,
        )
    }

    /// Access the underlying credential.
    pub fn credential(&self) -> &Credential {
        &self.credential
    }

    /// Attach the initial CAS key to this client instance.
    pub fn with_cas_key(self, cas_key: CasKey) -> Self {
        *self.cas_key.lock().unwrap() = Some(cas_key);
        self
    }

    /// Get the current CAS key known to this client.
    pub fn current_cas_key(&self) -> Option<CasKey> {
        *self.cas_key.lock().unwrap()
    }

    pub(crate) fn is_current_incarnation(&self) -> bool {
        self.relay_fence.allows(self.incarnation)
    }

    pub async fn get_clients_self(&self) -> Result<HttpResponse, TransportError> {
        self.post_connect_response(OrdinaryRequest::ClientsSelfGet, self.v3_headers(), &[])
            .await
    }

    pub async fn put_clients_self(&self, body: &[u8]) -> Result<HttpResponse, TransportError> {
        let mut headers = self.v3_headers();
        headers.push(("content-type".to_string(), "application/json".to_string()));
        self.post_connect_response(OrdinaryRequest::ClientsSelfPut, headers, body)
            .await
    }

    pub async fn get_relay_access(&self) -> Result<HttpResponse, TransportError> {
        self.post_connect_response(OrdinaryRequest::RelayAccessGet, self.v3_headers(), &[])
            .await
    }

    /// Attach the persisted pairing state path for best-effort relay token refresh write-back.
    pub fn with_state_path(self, path: PathBuf) -> Self {
        self.token_transaction.set_state_path(path);
        self
    }

    /// Attach an operation-scoped shared observation seam.
    pub fn with_observer(mut self, observer: ObserverHandle) -> Self {
        self.observer = observer;
        self
    }

    pub fn home_label(&self) -> &str {
        &self.credential.home_label
    }

    /// Upload one segment's files with the protocol-v3 envelope. `segment` is
    /// `HHMMSS_LEN`, `day` is `YYYYMMDD`.
    pub async fn ingest(
        &self,
        segment: &str,
        day: &str,
        files: Vec<FilePart>,
    ) -> Result<(IngestResponse, SendMetadata), TransportError> {
        let boundary = self.next_boundary();
        let request = IngestMultipart::new(boundary, day, segment, files)
            .map_err(|error| TransportError::Ingest(error.to_string()))?;
        let body = request.serialize()?;

        let mut headers = self.v3_headers();
        headers.push(("Content-Type".to_string(), request.content_type()));

        let (response, metadata) = self
            .ordinary_request(OrdinaryRequest::IngestPost, None, &headers, &body)
            .await?;
        let parsed = self.parse_ingest_response(response)?;
        Ok((parsed, metadata))
    }

    /// Read the root manifest used by protocol-v3 custody proof.
    pub async fn ingest_manifest(&self) -> Result<(IngestManifest, SendMetadata), TransportError> {
        let headers = self.v3_headers();
        let (response, metadata) = self
            .ordinary_request(OrdinaryRequest::IngestManifestGet, None, &headers, b"")
            .await?;
        Ok((self.parse_v3_read(response)?, metadata))
    }

    /// Read one day manifest used by protocol-v3 custody proof.
    pub async fn ingest_manifest_day(
        &self,
        day: &str,
    ) -> Result<(DayManifest, SendMetadata), TransportError> {
        let headers = self.v3_headers();
        let (response, metadata) = self
            .ordinary_request(
                OrdinaryRequest::IngestManifestDayGet,
                Some(day),
                &headers,
                b"",
            )
            .await?;
        Ok((self.parse_v3_read(response)?, metadata))
    }

    /// Read the protocol-v3 segments envelope for one day.
    pub async fn list_segments(
        &self,
        day: &str,
    ) -> Result<(SegmentsEnvelope, SendMetadata), TransportError> {
        let headers = self.v3_headers();
        let (response, metadata) = self
            .ordinary_request(
                OrdinaryRequest::IngestSegmentsDayGet,
                Some(day),
                &headers,
                b"",
            )
            .await?;
        Ok((self.parse_v3_read(response)?, metadata))
    }

    /// Read the system status and extract the sanitized current journal version string.
    pub async fn system_status(&self) -> Result<String, TransportError> {
        let fetch = async {
            let mut headers = self.v3_headers();
            headers.push(("Cache-Control".into(), "no-cache".into()));
            let (response, _) = self
                .ordinary_request(OrdinaryRequest::SystemStatusGet, None, &headers, b"")
                .await?;
            if response.status != 200 {
                return Err(TransportError::Rejected {
                    status: response.status,
                    body: response.body_text(),
                });
            }
            let parsed: SystemStatusResponse = serde_json::from_slice(&response.body)?;
            let version = parsed.version.current;
            if version.trim().is_empty()
                || version.len() > 128
                || version
                    .chars()
                    .any(|c| c.is_control() || c == '\u{2028}' || c == '\u{2029}')
            {
                return Err(TransportError::Rejected {
                    status: response.status,
                    body: "malformed version string".to_string(),
                });
            }
            Ok(version)
        };

        match tokio::time::timeout(Duration::from_secs(5), fetch).await {
            Ok(res) => res,
            Err(_) => Err(TransportError::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "system_status request timed out",
            ))),
        }
    }

    fn v3_headers(&self) -> Vec<(String, String)> {
        vec![(
            PROTOCOL_VERSION_HEADER.to_string(),
            OBSERVER_PROTOCOL_VERSION.to_string(),
        )]
    }

    async fn post_connect_response(
        &self,
        route: OrdinaryRequest,
        headers: Vec<(String, String)>,
        body: &[u8],
    ) -> Result<HttpResponse, TransportError> {
        let (response, _) = self.ordinary_request(route, None, &headers, body).await?;
        if response.body.len() > MAX_POST_CONNECT_RESPONSE_BYTES {
            return Err(TransportError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "response body exceeds 64 KiB limit",
            )));
        }
        Ok(response)
    }

    async fn ordinary_request(
        &self,
        route: OrdinaryRequest,
        day: Option<&str>,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<(HttpResponse, SendMetadata), TransportError> {
        debug_assert_eq!(OrdinaryRequest::ALL.len(), 8);
        let spec = route.spec();
        let path = route.path(day);
        let observer = self.observer.clone();
        let RequestOutcome {
            response,
            path: selected_path,
            attempts,
        } = self
            .transport
            .request(
                spec.method,
                &path,
                headers,
                body,
                RequestOptions {
                    response_cap: spec.response_cap,
                    replay: spec.replay,
                    observer: observer.as_deref(),
                },
            )
            .await
            .map_err(map_request_error)?;
        let path = match selected_path {
            SelectedPath::Direct => TransportPath::Direct,
            SelectedPath::Relay => TransportPath::Relay,
        };
        Ok((
            HttpResponse {
                status: response.status,
                headers: response.headers,
                body: response.body,
            },
            SendMetadata { path, attempts },
        ))
    }

    fn parse_v3_read<T: serde::de::DeserializeOwned>(
        &self,
        response: HttpResponse,
    ) -> Result<T, TransportError> {
        if !response.is_success() {
            return Err(TransportError::Rejected {
                status: response.status,
                body: response.body_text(),
            });
        }
        Ok(serde_json::from_slice(&response.body)?)
    }

    fn parse_ingest_response(
        &self,
        response: HttpResponse,
    ) -> Result<IngestResponse, TransportError> {
        // The v3 status vocabulary is parsed only on its documented response
        // statuses: accepted variants on 200, conflict on 409, and failed on
        // 500. Every other HTTP status remains an attributable server rejection.
        if !matches!(response.status, 200 | 409 | 500) {
            return Err(TransportError::Rejected {
                status: response.status,
                body: response.body_text(),
            });
        }
        let status = response.status;
        let parsed: IngestResponse = serde_json::from_slice(&response.body)?;
        let expected_status = match parsed.status {
            IngestStatus::Ok | IngestStatus::Duplicate | IngestStatus::Collision => 200,
            IngestStatus::Conflict => 409,
            IngestStatus::Failed => 500,
        };
        if status != expected_status {
            return Err(TransportError::Rejected {
                status,
                body: format!("ingest status does not match HTTP status {status}"),
            });
        }
        Ok(parsed)
    }

    pub(crate) fn proxy_headers(
        &self,
        browser_headers: &[(String, String)],
    ) -> Vec<(String, String)> {
        let mut headers = self.v3_headers();
        headers.extend(
            browser_headers
                .iter()
                .filter(|(name, _)| !is_observer_auth_header(name))
                .cloned(),
        );
        headers
    }

    pub(crate) async fn dial_carrier(&self) -> Result<DialedCarrier, TransportError> {
        const MAX_ATTEMPTS: usize = 5;
        let mut last_err: Option<TransportError> = None;
        for attempt in 0..MAX_ATTEMPTS {
            for endpoint in &self.credential.endpoints {
                if let Some(observer) = self.observer.as_deref() {
                    observer.record_dial_attempt();
                }
                match dial_tls(self.config.clone(), &endpoint.host, endpoint.port).await {
                    Ok(stream) => {
                        if let Some(observer) = self.observer.as_deref() {
                            observer.record_direct_success();
                        }
                        return Ok(DialedCarrier {
                            stream: Box::new(stream),
                            kind: CarrierKind::Lan,
                        });
                    }
                    Err(e) => last_err = Some(e),
                }
            }
            match &last_err {
                Some(TransportError::Tls(_)) | Some(TransportError::Io(_)) => {
                    tokio::time::sleep(Duration::from_millis(250 * (attempt as u64 + 1))).await;
                }
                _ => break,
            }
        }

        let lan_err = last_err.unwrap_or(TransportError::NoEndpoint);
        let lan_unreachable = matches!(
            lan_err,
            TransportError::Tls(_) | TransportError::Io(_) | TransportError::NoEndpoint
        );
        if lan_unreachable && self.relay_eligible() {
            return self.dial_carrier_over_relay().await;
        }
        Err(lan_err)
    }

    fn next_boundary(&self) -> String {
        let n = self.boundary_counter.fetch_add(1, Ordering::Relaxed);
        format!("----solstonewindowsboundary{n}")
    }

    /// True when the stored credential has relay coordinates and a live token.
    fn relay_eligible(&self) -> bool {
        self.is_current_incarnation()
            && self.credential.relay_origin.is_some()
            && self.device_token.is_some()
    }

    /// Clone the current live relay token under the single-flight mutex.
    async fn current_token(&self) -> String {
        self.device_token
            .as_ref()
            .expect("live device token present for relay send")
            .lock()
            .await
            .clone()
    }

    /// Persist a refreshed relay token before publishing it to the live mutex.
    async fn persist_token(&self, expected_cas: CasKey, token: String, expires_at: i64) -> bool {
        let Some(live_token) = self.device_token.clone() else {
            return false;
        };
        let path = self.state_path.lock().unwrap().clone();
        let fence = self.relay_fence.clone();
        let incarnation = self.incarnation;
        let cas_key = self.cas_key.clone();
        #[cfg(test)]
        let failpoint = FS_FAIL_POINT.with(|f| f.get());
        // All disk/CAS/live-token effects remain owned even if the request
        // waiting for this worker is cancelled.
        tokio::task::spawn_blocking(move || {
            let _owner = fence.publication.lock().unwrap();
            if expires_at <= now_secs() || !fence.allows(incarnation) || *cas_key.lock().unwrap() != Some(expected_cas) { return false; }
            let mut next_generation = expected_cas.access_mutation_generation;
            if let Some(path) = path {
                #[cfg(test)]
                FS_FAIL_POINT.with(|f| f.set(failpoint));
                let result = PairedState::mutate(&path, expected_cas, |cred| {
                    if expires_at <= now_secs() || !fence.allows(incarnation) { return Err(StorageError::CasMismatch); }
                    cred.device_token = Some(token.clone());
                    cred.device_token_expires_at = Some(expires_at);
                    Ok(())
                });
                #[cfg(test)]
                FS_FAIL_POINT.with(|f| f.set(0));
                match result {
                    Ok(generation) => next_generation = generation,
                    Err(StorageError::DurabilityUncertain(_)) => {
                        tracing::warn!(target: "sync", reason = "durability_uncertain", "token persistence uncertain");
                        let Ok(state) = PairedState::load(&path) else { return false; };
                        let Some(credential) = state.credential else { return false; };
                        if pairing_generation(&credential.client_cert_pem) != expected_cas.pairing_generation
                            || credential.device_token.as_deref() != Some(&token)
                            || credential.device_token_expires_at != Some(expires_at) { return false; }
                        next_generation = state.access_mutation_generation;
                    }
                    Err(_) => {
                        tracing::warn!(target: "sync", reason = "publication_rejected", "token persistence rejected");
                        return false;
                    }
                }
            }
            if !fence.allows(incarnation) { return false; }
            *live_token.blocking_lock() = token;
            *cas_key.lock().unwrap() = Some(CasKey {
                pairing_generation: expected_cas.pairing_generation,
                access_mutation_generation: next_generation,
            });
            true
        }).await.unwrap_or(false)
    }

    /// Refresh only if the live token still matches the caller's failed token.
    async fn refresh_if_current(&self, origin: &str, expected: &str) -> RefreshAction {
        let _refresh = self.refresh_gate.lock().await;
        let Some(captured_cas) = self.current_cas_key() else {
            return RefreshAction::Terminal;
        };
        if !self.is_current_incarnation() {
            return RefreshAction::Terminal;
        }
        let Some(token) = &self.device_token else {
            return RefreshAction::Terminal;
        };
        if token.lock().await.as_str() != expected {
            return RefreshAction::Redial;
        }
        match refresh_device_token(origin, expected).await {
            RefreshOutcome::Refreshed {
                device_token,
                expires_at,
            } => {
                if self
                    .persist_token(captured_cas, device_token.clone(), expires_at)
                    .await
                    && self.is_current_incarnation()
                {
                    RefreshAction::Redial
                } else {
                    RefreshAction::Terminal
                }
            }
            RefreshOutcome::ReconnectNeeded => RefreshAction::Terminal,
            RefreshOutcome::TransientError => RefreshAction::Transient,
        }
    }

    /// Dial a persistent carrier through the relay after the direct LAN loop has exhausted.
    async fn dial_carrier_over_relay(&self) -> Result<DialedCarrier, TransportError> {
        if !self.is_current_incarnation() {
            return Err(TransportError::Relay(RelayError::Unauthorized));
        }
        let origin = self
            .credential
            .relay_origin
            .as_deref()
            .ok_or(TransportError::NoEndpoint)?;
        let instance_id = &self.credential.instance_id;
        let current = self.current_token().await;
        if token_should_refresh(&current, now_secs()) {
            if let RefreshAction::Terminal = self.refresh_if_current(origin, &current).await {
                return Err(TransportError::Relay(RelayError::Unauthorized));
            }
        }

        let mut reactive_refreshed = false;
        let mut transient_attempt = 0usize;
        loop {
            if !self.is_current_incarnation() {
                return Err(TransportError::Relay(RelayError::Unauthorized));
            }
            let token = self.current_token().await;
            if let Some(observer) = self.observer.as_deref() {
                observer.record_dial_attempt();
            }
            match dial_relay_carrier(self.config.clone(), origin, instance_id, &token).await {
                Ok(carrier) => {
                    if let Some(observer) = self.observer.as_deref() {
                        observer.record_relay_success();
                    }
                    return Ok(DialedCarrier {
                        stream: Box::new(carrier.stream),
                        kind: CarrierKind::Relay {
                            termination: carrier.termination,
                        },
                    });
                }
                Err(TransportError::Relay(RelayError::Unauthorized)) => {
                    if reactive_refreshed {
                        return Err(TransportError::Relay(RelayError::Unauthorized));
                    }
                    reactive_refreshed = true;
                    match self.refresh_if_current(origin, &token).await {
                        RefreshAction::Redial => continue,
                        RefreshAction::Terminal | RefreshAction::Transient => {
                            return Err(TransportError::Relay(RelayError::Unauthorized));
                        }
                    }
                }
                Err(e) if relay_fault_is_transient_err(&e) => {
                    transient_attempt += 1;
                    if transient_attempt >= RELAY_MAX_TRANSIENT_ATTEMPTS {
                        return Err(e);
                    }
                    tokio::time::sleep(Duration::from_millis(250 * transient_attempt as u64)).await;
                }
                Err(e) => return Err(e),
            }
        }
    }
}

/// Decode JWT lifetime and apply the observer-pl proactive refresh threshold.
fn token_should_refresh(token: &str, now_secs: i64) -> bool {
    observer_pl::jwt::decode_unverified_claims(token)
        .map(|claims| observer_pl::jwt::should_refresh(&claims, now_secs))
        .unwrap_or(false)
}

/// Current UNIX time in seconds, falling back to zero on clock errors.
fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn is_observer_auth_header(name: &str) -> bool {
    name.eq_ignore_ascii_case("authorization")
        || name.eq_ignore_ascii_case(OBSERVER_HANDLE_HEADER)
        || name.eq_ignore_ascii_case(PROTOCOL_VERSION_HEADER)
}

/// Relay faults that are worth retrying inside the bounded relay phase.
fn relay_fault_is_transient(err: &RelayError) -> bool {
    matches!(
        err,
        RelayError::HomeOffline | RelayError::Abnormal | RelayError::Overflow | RelayError::Stalled
    )
}

/// Transport-level wrapper around the relay transient retry predicate.
fn relay_fault_is_transient_err(err: &TransportError) -> bool {
    matches!(err, TransportError::Relay(relay) if relay_fault_is_transient(relay))
}

#[derive(serde::Deserialize)]
struct SystemStatusResponse {
    version: SystemStatusVersion,
}

#[derive(serde::Deserialize)]
struct SystemStatusVersion {
    current: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential::{EndpointAddr, FS_FAIL_POINT};
    use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};
    use spl_transport::client::{TokenCommit, TokenCommitContext, TokenTransaction};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    fn relay_credential() -> Credential {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let params = CertificateParams::new(vec!["spl.local".to_string()]).unwrap();
        let cert = params.self_signed(&key).unwrap();
        Credential {
            client_key_pem: key.serialize_pem(),
            client_cert_pem: cert.pem(),
            ca_chain_pem: vec![cert.pem()],
            ca_fp_prefix: vec![0; 16],
            instance_id: "test".into(),
            home_label: "Home".into(),
            endpoints: vec![EndpointAddr {
                host: "127.0.0.1".into(),
                port: 9,
            }],
            relay_origin: Some("https://relay.example.com".into()),
            device_token: Some("old-token".into()),
            device_token_expires_at: Some(1_700_000_000),
        }
    }

    fn temp_pairing_path() -> PathBuf {
        let sequence = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "plw-client-test-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            sequence
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("pairing.json")
    }

    #[test]
    fn carrier_kind_maps_to_transport_path() {
        assert_eq!(
            TransportPath::from(&CarrierKind::Lan),
            TransportPath::Direct
        );

        let relay = CarrierKind::Relay {
            termination: RelayTerminationHandle::new(),
        };
        assert_eq!(TransportPath::from(&relay), TransportPath::Relay);
    }

    #[test]
    fn relay_fault_is_transient_truth_table() {
        for err in [
            RelayError::HomeOffline,
            RelayError::Abnormal,
            RelayError::Overflow,
            RelayError::Stalled,
        ] {
            assert!(relay_fault_is_transient(&err), "{err:?} should retry");
        }
        for err in [
            RelayError::Unauthorized,
            RelayError::Unpaid,
            RelayError::UnknownInstance,
            RelayError::UpgradeRejected,
        ] {
            assert!(!relay_fault_is_transient(&err), "{err:?} should stop");
        }
    }

    #[tokio::test]
    async fn persist_token_reconciles_post_rename_uncertainty() {
        let credential = relay_credential();
        let path = temp_pairing_path();
        PairedState {
            credential: Some(credential.clone()),
            access_mutation_generation: 0,
        }
        .save(&path)
        .unwrap();
        let cas = CasKey {
            pairing_generation: pairing_generation(&credential.client_cert_pem),
            access_mutation_generation: 0,
        };
        let client = ObserverClient::new(credential)
            .unwrap()
            .with_state_path(path.clone())
            .with_cas_key(cas);

        FS_FAIL_POINT.with(|f| f.set(2));
        assert!(
            client
                .persist_token(cas, "fresh-token".to_string(), 1_800_000_000)
                .await
        );
        FS_FAIL_POINT.with(|f| f.set(0));

        assert_eq!(
            client.current_cas_key().unwrap().access_mutation_generation,
            1
        );
        assert_eq!(
            PairedState::load(&path)
                .unwrap()
                .credential
                .unwrap()
                .device_token
                .as_deref(),
            Some("fresh-token")
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[tokio::test]
    async fn retired_refresh_waiting_for_publication_cannot_change_disk() {
        use std::future::Future;
        let credential = relay_credential();
        let path = temp_pairing_path();
        PairedState {
            credential: Some(credential.clone()),
            access_mutation_generation: 0,
        }
        .save(&path)
        .unwrap();
        let client = Arc::new(
            ObserverClient::new(credential)
                .unwrap()
                .with_state_path(path.clone()),
        );
        let slot = ClientSlot::new(client.clone());
        let cas = client.current_cas_key().unwrap();
        let owner = slot.publication_owner();
        let mut pending = Box::pin(client.persist_token(cas, "stale-token".into(), 1_900_000_000));
        {
            let _guard = owner.lock().unwrap();
            let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
            assert!(pending.as_mut().poll(&mut cx).is_pending());
            // The worker exists and must recheck retirement when it gets ownership.
            slot.disable_relay();
        }
        assert!(!pending.await);
        let disk = PairedState::load(&path).unwrap();
        assert_eq!(disk.access_mutation_generation, 0);
        assert_ne!(
            disk.credential.unwrap().device_token.as_deref(),
            Some("stale-token")
        );
        assert!(!client.relay_eligible());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn cancelled_refresh_waiter_keeps_disk_token_and_live_cas_publication_owned() {
        use std::future::Future;
        let credential = relay_credential();
        let path = temp_pairing_path();
        PairedState {
            credential: Some(credential.clone()),
            access_mutation_generation: 0,
        }
        .save(&path)
        .unwrap();
        let client = Arc::new(
            ObserverClient::new(credential)
                .unwrap()
                .with_state_path(path.clone()),
        );
        let slot = ClientSlot::new(client.clone());
        let cas = client.current_cas_key().unwrap();
        let owner = slot.publication_owner();
        {
            let _guard = owner.lock().unwrap();
            let mut pending =
                Box::pin(client.persist_token(cas, "owned-token".into(), 1_900_000_000));
            let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
            assert!(pending.as_mut().poll(&mut cx).is_pending());
            drop(pending);
        }
        tokio::time::timeout(Duration::from_secs(3), async {
            while client.current_cas_key().unwrap().access_mutation_generation == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("owned worker did not complete");
        assert_eq!(client.current_token().await, "owned-token");
        let disk = PairedState::load(&path).unwrap();
        assert_eq!(disk.access_mutation_generation, 1);
        assert_eq!(
            disk.credential.unwrap().device_token.as_deref(),
            Some("owned-token")
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn stale_token_transaction_cannot_fence_or_write_the_successor_incarnation() {
        let credential = relay_credential();
        let path = temp_pairing_path();
        PairedState {
            credential: Some(credential.clone()),
            access_mutation_generation: 0,
        }
        .save(&path)
        .unwrap();
        let fence = Arc::new(RelayFence::new(true));
        let transaction = WindowsTokenTransaction::new(
            Arc::new(std::sync::Mutex::new(Some(path.clone()))),
            pairing_generation(&credential.client_cert_pem),
            fence.clone(),
            1,
        );

        assert_eq!(fence.advance_from(1, true), Some(2));
        assert_eq!(
            transaction.commit(TokenCommitContext {
                token: "stale-token",
                expires_at: 1_900_000_000,
                previous_token: "old-token",
                incarnation: 1,
            }),
            TokenCommit::Unchanged
        );
        assert!(fence.allows(2), "the live successor remains relay-eligible");
        let persisted = PairedState::load(&path).unwrap();
        assert_eq!(persisted.access_mutation_generation, 0);
        assert_ne!(
            persisted.credential.unwrap().device_token.as_deref(),
            Some("stale-token")
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn rejected_transaction_stops_relay_only_requests_before_any_relay_dial() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay_port = listener.local_addr().unwrap().port();
        let mut credential = relay_credential();
        credential.endpoints.clear();
        credential.relay_origin = Some(format!("http://127.0.0.1:{relay_port}"));
        let path = temp_pairing_path();
        PairedState {
            credential: Some(credential.clone()),
            access_mutation_generation: 0,
        }
        .save(&path)
        .unwrap();
        let client = ObserverClient::new(credential)
            .unwrap()
            .with_state_path(path.clone());

        FS_FAIL_POINT.with(|fail_point| fail_point.set(1));
        let commit = client.token_transaction.commit(TokenCommitContext {
            token: "fresh-token",
            expires_at: 1_900_000_000,
            previous_token: "old-token",
            incarnation: 1,
        });
        FS_FAIL_POINT.with(|fail_point| fail_point.set(0));
        assert_eq!(commit, TokenCommit::Unchanged);
        assert!(matches!(
            client.system_status().await,
            Err(TransportError::RelayDisabled)
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "relay-only request dialed after publication rejection"
        );
        let persisted = PairedState::load(&path).unwrap();
        assert_eq!(persisted.access_mutation_generation, 0);
        assert_eq!(
            persisted.credential.unwrap().device_token.as_deref(),
            Some("old-token")
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
