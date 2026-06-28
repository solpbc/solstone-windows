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

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use observer_pl::multipart::{self, FilePart};
use observer_pl::wire::{
    HeartbeatEvent, IngestResponse, RegisterRequest, RegisterResponse, SegmentsResponse,
    ServerSegment,
};
use observer_pl::{
    paths, OBSERVER_HANDLE_HEADER, OBSERVER_PROTOCOL_VERSION, PROTOCOL_VERSION_HEADER,
};
use rustls::ClientConfig;

use crate::connection::request_once;
use crate::credential::Credential;
use crate::{tls, TransportError};

/// An observer talking to its paired journal over framed-mTLS.
pub struct ObserverClient {
    credential: Credential,
    config: Arc<ClientConfig>,
    observer_key: Option<String>,
    boundary_counter: AtomicU64,
}

impl ObserverClient {
    /// Build the client and its mTLS config from a stored credential.
    pub fn new(credential: Credential) -> Result<Self, TransportError> {
        let chain = tls::parse_certs(&credential.client_cert_pem)?;
        let key = tls::parse_private_key(&credential.client_key_pem)?;
        let config = Arc::new(tls::mtls_config(&credential.ca_fp_prefix, chain, key)?);
        Ok(Self {
            credential,
            config,
            observer_key: None,
            boundary_counter: AtomicU64::new(1),
        })
    }

    /// Attach a previously-registered observer handle (resumed from disk).
    pub fn with_observer_key(mut self, key: Option<String>) -> Self {
        self.observer_key = key;
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
        let response = self.send("POST", paths::REGISTER, &headers, &body).await?;
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
    ) -> Result<IngestResponse, TransportError> {
        let boundary = self.next_boundary();
        let fields = [("segment", segment), ("day", day), ("platform", platform)];
        let body = multipart::build(&boundary, &fields, files);

        let mut headers = self.auth_headers()?;
        headers.push((
            "Content-Type".to_string(),
            multipart::content_type(&boundary),
        ));

        let response = self.send("POST", paths::INGEST, &headers, &body).await?;
        if !response.is_success() {
            return Err(TransportError::Rejected {
                status: response.status,
                body: response.body_text(),
            });
        }
        Ok(serde_json::from_slice(&response.body)?)
    }

    /// Post the `observe.status` heartbeat so the journal sees the observer live.
    pub async fn heartbeat(&self, event: &HeartbeatEvent) -> Result<(), TransportError> {
        let body = serde_json::to_vec(event)?;
        let mut headers = self.auth_headers()?;
        headers.push(("Content-Type".to_string(), "application/json".to_string()));
        let response = self
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
        let response = self.send("GET", &path, &headers, b"").await?;
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

    fn next_boundary(&self) -> String {
        let n = self.boundary_counter.fetch_add(1, Ordering::Relaxed);
        format!("----solstonewindowsboundary{n}")
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
    ) -> Result<observer_pl::http::HttpResponse, TransportError> {
        const MAX_ATTEMPTS: usize = 5;
        let mut last_err: Option<TransportError> = None;
        for attempt in 0..MAX_ATTEMPTS {
            for endpoint in &self.credential.endpoints {
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
                    Ok(response) => return Ok(response),
                    Err(e) => last_err = Some(e),
                }
            }
            // Only connection/handshake faults are worth retrying; a parsed HTTP
            // error (e.g. 401) is deterministic and returned immediately.
            match &last_err {
                Some(TransportError::Tls(_)) | Some(TransportError::Io(_)) => {
                    tokio::time::sleep(std::time::Duration::from_millis(
                        250 * (attempt as u64 + 1),
                    ))
                    .await;
                }
                _ => break,
            }
        }
        Err(last_err.unwrap_or(TransportError::NoEndpoint))
    }
}
