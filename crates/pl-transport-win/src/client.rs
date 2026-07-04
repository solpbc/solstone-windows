// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The observer client over established mTLS.
//!
//! Wraps a paired [`Credential`] and speaks the four observer endpoints:
//! `register`, `ingest`, `ingest/event` (heartbeat), and `ingest/segments/<day>`
//! (reconcile). Every authenticated request carries the observer handle in both
//! `X-Solstone-Observer` (preferred; survives proxy stripping) and
//! `Authorization: Bearer` (fallback), plus `X-Solstone-Protocol-Version: 2` —
//! the journal accepts either auth header, and the two recent Android 401s were
//! both *missing-header* bugs, so the client always sends the full set.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use observer_model::TransportPath;
use observer_pl::http::HttpResponse;
use observer_pl::multipart::{self, FilePart};
use observer_pl::wire::{
    HeartbeatEvent, IngestResponse, RegisterRequest, RegisterResponse, SegmentsResponse,
    ServerSegment,
};
use observer_pl::{
    paths, OBSERVER_HANDLE_HEADER, OBSERVER_PROTOCOL_VERSION, PROTOCOL_VERSION_HEADER,
};
use rustls::ClientConfig;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::connection::{dial_tls, request_once};
use crate::credential::{Credential, PairedState};
use crate::relay::{dial_relay_carrier, request_once_relay, RelayTerminationHandle};
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
    observer_key: Option<String>,
    boundary_counter: AtomicU64,
    /// Live relay device-token used for dials; the mutex is the refresh single-flight gate.
    device_token: Option<tokio::sync::Mutex<String>>,
    /// Optional persisted pairing state path for best-effort refreshed-token write-back.
    state_path: Option<PathBuf>,
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
            observer_key: None,
            boundary_counter: AtomicU64::new(1),
            device_token,
            state_path: None,
        })
    }

    /// Attach a previously-registered observer handle (resumed from disk).
    pub fn with_observer_key(mut self, key: Option<String>) -> Self {
        self.observer_key = key;
        self
    }

    /// Attach the persisted pairing state path for best-effort relay token refresh write-back.
    pub fn with_state_path(mut self, path: PathBuf) -> Self {
        self.state_path = Some(path);
        self
    }

    pub fn observer_key(&self) -> Option<&str> {
        self.observer_key.as_deref()
    }

    pub fn home_label(&self) -> &str {
        &self.credential.home_label
    }

    /// Self-register and lock the observer stream. Stores the returned handle.
    /// Register is authorized by the PL identity (the mTLS client cert), so it
    /// carries no observer header.
    pub async fn register(
        &mut self,
        platform: &str,
        hostname: &str,
        stream_type: &str,
        version: &str,
        label: Option<String>,
    ) -> Result<RegisterResponse, TransportError> {
        let request = RegisterRequest {
            platform: platform.to_string(),
            hostname: hostname.to_string(),
            stream_type: stream_type.to_string(),
            version: version.to_string(),
            label,
        };
        let body = serde_json::to_vec(&request)?;
        let headers = vec![("Content-Type".to_string(), "application/json".to_string())];
        let SendOutcome { response, .. } =
            self.send("POST", paths::REGISTER, &headers, &body).await?;
        if !response.is_success() {
            return Err(TransportError::Rejected {
                status: response.status,
                body: response.body_text(),
            });
        }
        let parsed: RegisterResponse = serde_json::from_slice(&response.body)?;
        self.observer_key = Some(parsed.key.clone());
        Ok(parsed)
    }

    /// Upload one segment's files. `segment` is `HHMMSS_LEN`, `day` is
    /// `YYYYMMDD`.
    pub async fn ingest(
        &self,
        segment: &str,
        day: &str,
        platform: &str,
        files: &[FilePart],
    ) -> Result<(IngestResponse, SendMetadata), TransportError> {
        let boundary = self.next_boundary();
        let fields = [("segment", segment), ("day", day), ("platform", platform)];
        let body = multipart::build(&boundary, &fields, files);

        let mut headers = self.auth_headers()?;
        headers.push((
            "Content-Type".to_string(),
            multipart::content_type(&boundary),
        ));

        let SendOutcome { response, metadata } =
            self.send("POST", paths::INGEST, &headers, &body).await?;
        if !response.is_success() {
            return Err(TransportError::Rejected {
                status: response.status,
                body: response.body_text(),
            });
        }
        Ok((serde_json::from_slice(&response.body)?, metadata))
    }

    /// Post the `observe.status` heartbeat so the journal sees the observer live.
    pub async fn heartbeat(&self, event: &HeartbeatEvent) -> Result<(), TransportError> {
        let body = serde_json::to_vec(event)?;
        let mut headers = self.auth_headers()?;
        headers.push(("Content-Type".to_string(), "application/json".to_string()));
        let SendOutcome { response, .. } = self
            .send("POST", paths::INGEST_EVENT, &headers, &body)
            .await?;
        if !response.is_success() {
            return Err(TransportError::Rejected {
                status: response.status,
                body: response.body_text(),
            });
        }
        Ok(())
    }

    /// List the journal's recorded segments for a day (reconciliation source).
    pub async fn list_segments(&self, day: &str) -> Result<SegmentsResponse, TransportError> {
        let path = format!("{}/{}", paths::INGEST_SEGMENTS, day);
        let headers = self.auth_headers()?;
        let SendOutcome { response, .. } = self.send("GET", &path, &headers, b"").await?;
        if !response.is_success() {
            return Err(TransportError::Rejected {
                status: response.status,
                body: response.body_text(),
            });
        }
        // v2 returns the {items,total,protocol_version} envelope; tolerate a bare
        // array from a pre-v2 journal too.
        if let Ok(envelope) = serde_json::from_slice::<SegmentsResponse>(&response.body) {
            return Ok(envelope);
        }
        let items: Vec<ServerSegment> = serde_json::from_slice(&response.body)?;
        Ok(SegmentsResponse {
            total: Some(items.len() as u64),
            items,
            protocol_version: None,
        })
    }

    fn auth_headers(&self) -> Result<Vec<(String, String)>, TransportError> {
        let key = self
            .observer_key
            .as_ref()
            .ok_or(TransportError::NotPaired)?;
        Ok(vec![
            (OBSERVER_HANDLE_HEADER.to_string(), key.clone()),
            ("Authorization".to_string(), format!("Bearer {key}")),
            (
                PROTOCOL_VERSION_HEADER.to_string(),
                OBSERVER_PROTOCOL_VERSION.to_string(),
            ),
        ])
    }

    pub(crate) fn proxy_headers(
        &self,
        browser_headers: &[(String, String)],
    ) -> Result<Vec<(String, String)>, TransportError> {
        let mut headers = self.auth_headers()?;
        headers.extend(
            browser_headers
                .iter()
                .filter(|(name, _)| !is_observer_auth_header(name))
                .cloned(),
        );
        Ok(headers)
    }

    pub(crate) async fn dial_carrier(&self) -> Result<DialedCarrier, TransportError> {
        const MAX_ATTEMPTS: usize = 5;
        let mut last_err: Option<TransportError> = None;
        for attempt in 0..MAX_ATTEMPTS {
            for endpoint in &self.credential.endpoints {
                match dial_tls(self.config.clone(), &endpoint.host, endpoint.port).await {
                    Ok(stream) => {
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
            log_dial_start(path, attempts);
            let started = Instant::now();
            match request_once_relay(
                self.config.clone(),
                origin,
                instance_id,
                &token,
                method,
                path,
                headers,
                body,
            )
            .await
            {
                Ok(response) => {
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
            match dial_relay_carrier(self.config.clone(), origin, instance_id, &token).await {
                Ok(carrier) => {
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
                log_dial_start(path, attempts);
                let started = Instant::now();
                match request_once(
                    self.config.clone(),
                    &endpoint.host,
                    endpoint.port,
                    method,
                    path,
                    headers,
                    body,
                )
                .await
                {
                    Ok(response) => {
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
                    attempts: relay_attempts,
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
