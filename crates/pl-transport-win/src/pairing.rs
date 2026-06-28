// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The pairing handshake.
//!
//! Over a certless, CA-fp-pinned TLS connection, POST a freshly-minted CSR to
//! `/app/network/pair?token=<nonce>`; the journal signs it and returns the client
//! cert + CA chain + its identity. We verify the returned `fingerprint` equals
//! `sha256:<hex>` of the signed client cert (the integrity check the Android/iOS
//! clients also do) before trusting the credential. Multi-address pair-links are
//! tried in order; the first candidate that completes wins.

use std::sync::Arc;

use observer_pl::pairlink::{self, Endpoint, ParsedPairLink};
use observer_pl::wire::{PairRequest, PairResponse};
use observer_pl::{ca, paths};

use crate::connection::request_once;
use crate::credential::{generate_csr, Credential, EndpointAddr};
use crate::relay_pairing;
use crate::{tls, TransportError};

/// Pair against the given candidate endpoints using the one-shot `nonce_hex` and
/// the pinned `ca_fp_prefix`. Returns the signed [`Credential`] on success.
pub async fn pair(
    endpoints: &[Endpoint],
    nonce_hex: &str,
    ca_fp_prefix: &[u8],
    device_label: &str,
) -> Result<Credential, TransportError> {
    if endpoints.is_empty() {
        return Err(TransportError::NoEndpoint);
    }
    let config = Arc::new(tls::pairing_config(ca_fp_prefix)?);
    let path = format!("{}?token={}", paths::PAIR, nonce_hex);

    let mut last_err: Option<TransportError> = None;
    for endpoint in endpoints {
        match pair_one(
            config.clone(),
            endpoint,
            &path,
            ca_fp_prefix,
            device_label,
            endpoints,
        )
        .await
        {
            Ok(cred) => return Ok(cred),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or(TransportError::NoEndpoint))
}

/// Parse a `https://go.solstone.app/p#…` pair-link and pair against it.
pub async fn pair_from_link(link: &str, device_label: &str) -> Result<Credential, TransportError> {
    let parsed = pairlink::parse(link).map_err(|e| TransportError::PairLink(e.to_string()))?;
    match parsed {
        ParsedPairLink::Direct(pl) => {
            pair(
                &pl.candidates,
                &pl.nonce_hex,
                &pl.ca_fp_prefix,
                device_label,
            )
            .await
        }
        ParsedPairLink::Relay(rl) => relay_pairing::pair_over_relay(&rl, device_label).await,
    }
}

async fn pair_one(
    config: Arc<rustls::ClientConfig>,
    endpoint: &Endpoint,
    path: &str,
    ca_fp_prefix: &[u8],
    device_label: &str,
    all_endpoints: &[Endpoint],
) -> Result<Credential, TransportError> {
    let generated = generate_csr(device_label)?;
    let request = PairRequest {
        csr: generated.csr_pem,
        device_label: device_label.to_string(),
    };
    let body = serde_json::to_vec(&request)?;
    let headers = vec![("Content-Type".to_string(), "application/json".to_string())];

    let response = request_once(
        config,
        &endpoint.host,
        endpoint.port,
        "POST",
        path,
        &headers,
        &body,
    )
    .await?;
    if !response.is_success() {
        return Err(TransportError::Rejected {
            status: response.status,
            body: response.body_text(),
        });
    }

    let pair: PairResponse = serde_json::from_slice(&response.body)?;
    let cert_der = tls::parse_certs(&pair.client_cert)?
        .into_iter()
        .next()
        .ok_or_else(|| TransportError::Pairing("pair response carried no client cert".into()))?;
    let computed = format!("sha256:{}", ca::sha256_hex(cert_der.as_ref()));
    if pair.fingerprint != computed {
        return Err(TransportError::Pairing(format!(
            "client cert fingerprint mismatch (journal: {}, computed: {})",
            pair.fingerprint, computed
        )));
    }

    Ok(Credential {
        client_key_pem: generated.key_pem,
        client_cert_pem: pair.client_cert,
        ca_chain_pem: pair.ca_chain,
        ca_fp_prefix: ca_fp_prefix.to_vec(),
        instance_id: pair.instance_id,
        home_label: pair.home_label,
        endpoints: all_endpoints.iter().map(EndpointAddr::from).collect(),
        relay_origin: None,
        device_token: None,
        device_token_expires_at: None,
    })
}
