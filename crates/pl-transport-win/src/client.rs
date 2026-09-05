// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The observer client over established mTLS.
//!
//! Wraps a paired [`Credential`] and speaks the linked-device protocol-v3
//! endpoints. Those endpoints authenticate exclusively with the mTLS peer and
//! send only their protocol header.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use observer_model::TransportPath;
use observer_pl::http::HttpResponse;
use observer_pl::ingest::{
    DayManifest, FilePart, IngestManifest, IngestMultipart, IngestResponse, IngestStatus,
    SegmentsEnvelope,
};
use observer_pl::{
    paths, OBSERVER_HANDLE_HEADER, OBSERVER_PROTOCOL_VERSION, PROTOCOL_VERSION_HEADER,
};
use rustls::ClientConfig;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::connection::{dial_tls, request_once_observed};
use crate::credential::{Credential, PairedState};
use crate::observe::{note_dial_attempt, note_dial_success, ObserverHandle};
use crate::relay::{
    dial_relay_carrier, request_once_relay_observed, RelayRequestSpec, RelayTerminationHandle,
};
use crate::relay_token::{refresh_device_token, RefreshOutcome};
use crate::{tls, transport_error_code, RelayError, TransportError};

/// Relay transient retry count. Mirrors the LAN connection/handshake retry bound.
const RELAY_MAX_TRANSIENT_ATTEMPTS: usize = 5;

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

struct SendOutcome {
    response: HttpResponse,
    metadata: SendMetadata,
}

/// An observer talking to its paired journal over framed-mTLS.
pub struct ObserverClient {
    credential: Credential,
    config: Arc<ClientConfig>,
    boundary_counter: AtomicU64,
    /// Live relay device-token used for dials; the mutex is the refresh single-flight gate.
    device_token: Option<tokio::sync::Mutex<String>>,
    /// Optional persisted pairing state path for best-effort refreshed-token write-back.
    state_path: Option<PathBuf>,
    /// Optional operation-scoped observation seam. `None` in the GUI.
    observer: ObserverHandle,
}

impl ObserverClient {
    /// Build the client and its mTLS config from a stored credential.
    pub fn new(credential: Credential) -> Result<Self, TransportError> {
        if credential.relay_origin.is_some() && credential.endpoints.is_empty() {
            return Err(TransportError::Pairing(
                "relay credential has no LAN endpoints".into(),
            ));
        }
        let device_token = credential.device_token.clone().map(tokio::sync::Mutex::new);
        let chain = tls::parse_certs(&credential.client_cert_pem)?;
        let key = tls::parse_private_key(&credential.client_key_pem)?;
        let config = Arc::new(tls::mtls_config(&credential.ca_fp_prefix, chain, key)?);
        Ok(Self {
            credential,
            config,
            boundary_counter: AtomicU64::new(1),
            device_token,
            state_path: None,
            observer: None,
        })
    }

    /// Attach the persisted pairing state path for best-effort relay token refresh write-back.
    pub fn with_state_path(mut self, path: PathBuf) -> Self {
        self.state_path = Some(path);
        self
    }

    /// Attach an operation-scoped observation seam.
    ///
    /// Observation only: every dial site records through it without branching on
    /// what it holds, so retry policy, backoff, and ordering are unchanged.
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

        let SendOutcome { response, metadata } =
            self.send("POST", paths::INGEST, &headers, &body).await?;
        let parsed = self.parse_ingest_response(response)?;
        Ok((parsed, metadata))
    }

    /// Read the root manifest used by protocol-v3 custody proof.
    pub async fn ingest_manifest(&self) -> Result<(IngestManifest, SendMetadata), TransportError> {
        let headers = self.v3_headers();
        let SendOutcome { response, metadata } = self
            .send("GET", paths::INGEST_MANIFEST, &headers, b"")
            .await?;
        Ok((self.parse_v3_read(response)?, metadata))
    }

    /// Read one day manifest used by protocol-v3 custody proof.
    pub async fn ingest_manifest_day(
        &self,
        day: &str,
    ) -> Result<(DayManifest, SendMetadata), TransportError> {
        let path = format!("{}/{}", paths::INGEST_MANIFEST, day);
        let headers = self.v3_headers();
        let SendOutcome { response, metadata } = self.send("GET", &path, &headers, b"").await?;
        Ok((self.parse_v3_read(response)?, metadata))
    }

    /// Read the protocol-v3 segments envelope for one day.
    pub async fn list_segments(
        &self,
        day: &str,
    ) -> Result<(SegmentsEnvelope, SendMetadata), TransportError> {
        let path = format!("{}/{}", paths::INGEST_SEGMENTS, day);
        let headers = self.v3_headers();
        let SendOutcome { response, metadata } = self.send("GET", &path, &headers, b"").await?;
        Ok((self.parse_v3_read(response)?, metadata))
    }

    /// Read the system status and extract the sanitized current journal version string.
    pub async fn system_status(&self) -> Result<String, TransportError> {
        let fetch = async {
            let mut headers = self.v3_headers();
            headers.push(("Cache-Control".into(), "no-cache".into()));
            let SendOutcome { response, .. } = self
                .send("GET", "/api/system/status", &headers, b"")
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
                note_dial_attempt(&self.observer);
                match dial_tls(self.config.clone(), &endpoint.host, endpoint.port).await {
                    Ok(stream) => {
                        note_dial_success(&self.observer, TransportPath::Direct);
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
        self.credential.relay_origin.is_some() && self.device_token.is_some()
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

    /// Best-effort write-back of a refreshed relay token into the persisted pairing state.
    async fn persist_token(&self, token: &str, expires_at: i64) {
        let Some(path) = &self.state_path else {
            return;
        };
        let Ok(mut state) = PairedState::load(path) else {
            return;
        };
        let Some(credential) = state.credential.as_mut() else {
            return;
        };
        credential.device_token = Some(token.to_string());
        credential.device_token_expires_at = Some(expires_at);
        let _ = state.save(path);
    }

    /// Refresh only if the live token still matches the caller's failed token.
    async fn refresh_if_current(&self, origin: &str, expected: &str) -> RefreshAction {
        let Some(token) = &self.device_token else {
            return RefreshAction::Terminal;
        };
        let mut guard = token.lock().await;
        if guard.as_str() != expected {
            return RefreshAction::Redial;
        }
        match refresh_device_token(origin, expected).await {
            RefreshOutcome::Refreshed {
                device_token,
                expires_at,
            } => {
                *guard = device_token.clone();
                drop(guard);
                self.persist_token(&device_token, expires_at).await;
                RefreshAction::Redial
            }
            RefreshOutcome::ReconnectNeeded => RefreshAction::Terminal,
            RefreshOutcome::TransientError => RefreshAction::Transient,
        }
    }

    /// Send through the relay after the direct LAN loop has exhausted.
    async fn send_over_relay(
        &self,
        method: &str,
        path: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<(HttpResponse, u32), TransportError> {
        let Some(origin) = self.credential.relay_origin.as_deref() else {
            let err = TransportError::NoEndpoint;
            log_dial_failed(path, 0, &err);
            return Err(err);
        };
        let instance_id = &self.credential.instance_id;
        let current = self.current_token().await;
        if token_should_refresh(&current, now_secs()) {
            if let RefreshAction::Terminal = self.refresh_if_current(origin, &current).await {
                let err = TransportError::Relay(RelayError::Unauthorized);
                log_dial_failed(path, 0, &err);
                return Err(err);
            }
        }

        let mut reactive_refreshed = false;
        let mut transient_attempt = 0usize;
        let mut attempts = 0u32;
        loop {
            let token = self.current_token().await;
            attempts = attempts.saturating_add(1);
            note_dial_attempt(&self.observer);
            log_dial_start(path, attempts);
            let started = Instant::now();
            let request = RelayRequestSpec::new(method, path, headers, body, &self.observer);
            match request_once_relay_observed(
                self.config.clone(),
                origin,
                instance_id,
                &token,
                request,
            )
            .await
            {
                Ok(response) => {
                    note_dial_success(&self.observer, TransportPath::Relay);
                    log_dial_success(path, attempts, elapsed_ms(started));
                    log_path_selected(TransportPath::Relay);
                    return Ok((response, attempts));
                }
                Err(TransportError::Relay(RelayError::Unauthorized)) => {
                    if reactive_refreshed {
                        let err = TransportError::Relay(RelayError::Unauthorized);
                        log_dial_failed(path, attempts, &err);
                        return Err(err);
                    }
                    reactive_refreshed = true;
                    match self.refresh_if_current(origin, &token).await {
                        RefreshAction::Redial => continue,
                        RefreshAction::Terminal | RefreshAction::Transient => {
                            let err = TransportError::Relay(RelayError::Unauthorized);
                            log_dial_failed(path, attempts, &err);
                            return Err(err);
                        }
                    }
                }
                Err(e) if relay_fault_is_transient_err(&e) => {
                    transient_attempt += 1;
                    if transient_attempt >= RELAY_MAX_TRANSIENT_ATTEMPTS {
                        log_dial_failed(path, attempts, &e);
                        return Err(e);
                    }
                    let backoff_ms = 250 * transient_attempt as u64;
                    log_transient_retry(path, attempts, backoff_ms, &e);
                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                }
                Err(e) => {
                    log_dial_failed(path, attempts, &e);
                    return Err(e);
                }
            }
        }
    }

    /// Dial a persistent carrier through the relay after the direct LAN loop has exhausted.
    async fn dial_carrier_over_relay(&self) -> Result<DialedCarrier, TransportError> {
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
            let token = self.current_token().await;
            note_dial_attempt(&self.observer);
            match dial_relay_carrier(self.config.clone(), origin, instance_id, &token).await {
                Ok(carrier) => {
                    note_dial_success(&self.observer, TransportPath::Relay);
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

    /// Send a request, trying each journal endpoint and retrying transient
    /// connection/handshake failures. Connection-per-request means each call
    /// re-handshakes; a freshly-paired fingerprint can take a moment to reach
    /// every journal worker (the box fans :7657 across SO_REUSEPORT processes),
    /// so a `tls handshake eof` / connection error is retried with linear
    /// backoff before giving up.
    async fn send(
        &self,
        method: &str,
        path: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<SendOutcome, TransportError> {
        const MAX_ATTEMPTS: usize = 5;
        let mut last_err: Option<TransportError> = None;
        let mut attempts = 0u32;
        for attempt in 0..MAX_ATTEMPTS {
            for endpoint in &self.credential.endpoints {
                attempts = attempts.saturating_add(1);
                note_dial_attempt(&self.observer);
                log_dial_start(path, attempts);
                let started = Instant::now();
                match request_once_observed(
                    self.config.clone(),
                    &endpoint.host,
                    endpoint.port,
                    method,
                    path,
                    headers,
                    body,
                    &self.observer,
                )
                .await
                {
                    Ok(response) => {
                        note_dial_success(&self.observer, TransportPath::Direct);
                        log_dial_success(path, attempts, elapsed_ms(started));
                        let transport_path = TransportPath::Direct;
                        log_path_selected(transport_path);
                        return Ok(SendOutcome {
                            response,
                            metadata: SendMetadata {
                                path: transport_path,
                                attempts,
                            },
                        });
                    }
                    Err(e) => last_err = Some(e),
                }
            }
            // Only connection/handshake faults are worth retrying; a parsed HTTP
            // error (e.g. 401) is deterministic and returned immediately.
            match &last_err {
                Some(TransportError::Tls(_)) | Some(TransportError::Io(_)) => {
                    let backoff_ms = 250 * (attempt as u64 + 1);
                    if let Some(error) = &last_err {
                        log_transient_retry(path, attempts, backoff_ms, error);
                    }
                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
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
            tracing::info!(
                target: "pl_transport",
                route = path,
                from = "direct",
                to = "relay",
                "transport fallback"
            );
            let (response, relay_attempts) =
                self.send_over_relay(method, path, headers, body).await?;
            return Ok(SendOutcome {
                response,
                metadata: SendMetadata {
                    path: TransportPath::Relay,
                    // The exhausted LAN legs were real dials on the way here, so
                    // the reported total spans both. Reporting only the relay
                    // count would understate the work this request cost.
                    attempts: attempts.saturating_add(relay_attempts),
                },
            });
        }
        log_dial_failed(path, attempts, &lan_err);
        Err(lan_err)
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

fn log_dial_start(route: &str, attempt: u32) {
    tracing::info!(
        target: "pl_transport",
        route,
        attempt,
        "dial start"
    );
}

fn log_dial_success(route: &str, attempts: u32, duration_ms: u64) {
    tracing::info!(
        target: "pl_transport",
        route,
        attempts,
        duration_ms,
        "dial success"
    );
}

fn log_path_selected(path: TransportPath) {
    tracing::info!(
        target: "pl_transport",
        path = path.as_str(),
        "path selected"
    );
}

fn log_transient_retry(route: &str, attempt: u32, backoff_ms: u64, err: &TransportError) {
    tracing::info!(
        target: "pl_transport",
        route,
        attempt,
        backoff_ms,
        reason = %transport_error_code(err),
        "transient retry"
    );
}

fn log_dial_failed(route: &str, attempts: u32, err: &TransportError) {
    tracing::warn!(
        target: "pl_transport",
        route,
        attempts,
        reason = %transport_error_code(err),
        "dial failed"
    );
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
}
