// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Relay-form pairing ceremony.

use std::sync::Arc;

use observer_pl::pairlink::RelayPairLink;
use observer_pl::wire::{PairRequest, PairResponse};
use observer_pl::{ca, paths};
use rustls::pki_types::CertificateDer;
use serde::Deserialize;
use serde_json::json;

use crate::credential::{endpoint_addrs_from_local_endpoints, generate_csr, Credential};
use crate::{relay, relay_http, spki_pin, tls, RelayControlEndpoint, TransportError};

#[derive(Deserialize)]
struct EnrollResponse {
    device_token: String,
}

pub async fn pair_over_relay(
    link: &RelayPairLink,
    device_label: &str,
) -> Result<Credential, TransportError> {
    let rk = observer_pl::relay_window::derive_rk(&link.s);
    let url = observer_pl::relay::pair_dial_url(&link.relay_origin)
        .map_err(|e| TransportError::PairLink(format!("relay origin: {e}")))?;
    let ws = relay::dial_pair_relay_ws(&url, &hex_lower(&rk), relay::outer_config()).await?;

    let generated = generate_csr(device_label)?;
    let request = PairRequest {
        csr: generated.csr_pem,
        device_label: device_label.to_string(),
    };
    let body = serde_json::to_vec(&request)?;
    let headers = vec![("Content-Type".to_string(), "application/json".to_string())];
    let path = format!("{}?token={}", paths::PAIR, hex_lower(&link.s));
    let inner_config = Arc::new(tls::trust_all_pairing_config()?);
    let (response, peer_leaf) = relay::request_once_over_ws_with_peer_leaf(
        ws,
        inner_config,
        relay::RELAY_HANDSHAKE_TIMEOUT,
        "POST",
        &path,
        &headers,
        &body,
    )
    .await?;
    let peer_leaf =
        peer_leaf.ok_or_else(|| TransportError::Pairing("relay missing peer leaf".into()))?;
    if !response.is_success() {
        return Err(TransportError::Rejected {
            status: response.status,
            body: response.body_text(),
        });
    }

    let pair: PairResponse = serde_json::from_slice(&response.body)?;
    let ca_chain_der = parse_ca_chain(&pair.ca_chain)?;
    let pinned_ca = ca_chain_der
        .iter()
        .find(|cert| ca::spki_matches_prefix(cert.as_ref(), &link.ca_fp_spki))
        .cloned()
        .ok_or_else(|| TransportError::Pairing("relay pinned ca not found".into()))?;
    spki_pin::verify_live_peer_binding(&peer_leaf, &pinned_ca)?;
    spki_pin::verify_ca_self_signed(&pinned_ca)?;

    let spki = ca::extract_spki_der(pinned_ca.as_ref())
        .map_err(|_| TransportError::Pairing("relay ca spki".into()))?;
    let expected = observer_pl::relay_window::jid_from_spki(&spki)
        .map_err(|_| TransportError::Pairing("relay ca not p-256".into()))?;
    if pair.instance_id != expected {
        return Err(TransportError::Pairing("relay instance mismatch".into()));
    }

    let client_cert_der = tls::parse_certs(&pair.client_cert)?
        .into_iter()
        .next()
        .ok_or_else(|| TransportError::Pairing("relay response missing client cert".into()))?;
    let computed = format!("sha256:{}", ca::sha256_hex(client_cert_der.as_ref()));
    if pair.fingerprint != computed {
        return Err(TransportError::Pairing(
            "relay client cert fingerprint mismatch".into(),
        ));
    }

    let home_attestation = pair
        .home_attestation
        .as_deref()
        .ok_or_else(|| TransportError::Pairing("relay response missing home attestation".into()))?;
    let device_token =
        enroll_device(&link.relay_origin, &pair.instance_id, home_attestation).await?;
    let device_token_expires_at =
        observer_pl::jwt::decode_unverified_claims(&device_token).map(|c| c.exp);
    let ca_fp_prefix = ca::sha256(pinned_ca.as_ref())[..16].to_vec();
    let endpoints = endpoint_addrs_from_local_endpoints(pair.local_endpoints.as_ref());

    Ok(Credential {
        client_key_pem: generated.key_pem,
        client_cert_pem: pair.client_cert,
        ca_chain_pem: pair.ca_chain,
        ca_fp_prefix,
        instance_id: pair.instance_id,
        home_label: pair.home_label,
        endpoints,
        relay_origin: Some(link.relay_origin.clone()),
        device_token: Some(device_token),
        device_token_expires_at,
    })
}

async fn enroll_device(
    relay_origin: &str,
    instance_id: &str,
    home_attestation: &str,
) -> Result<String, TransportError> {
    let body = serde_json::to_vec(&json!({
        "instance_id": instance_id,
        "home_attestation": home_attestation,
    }))?;
    let response = relay_http::relay_https_post_json(relay_origin, "/enroll/device", &body).await?;
    if !response.is_success() {
        return Err(TransportError::RelayControlRejected {
            endpoint: RelayControlEndpoint::EnrollDevice,
            status: response.status,
        });
    }
    let parsed: EnrollResponse = serde_json::from_slice(&response.body)
        .map_err(|_| TransportError::Pairing("relay enroll response malformed".into()))?;
    if parsed.device_token.is_empty() {
        return Err(TransportError::Pairing(
            "relay enroll response malformed".into(),
        ));
    }
    Ok(parsed.device_token)
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn parse_ca_chain(chain: &[String]) -> Result<Vec<CertificateDer<'static>>, TransportError> {
    let mut out = Vec::new();
    for pem in chain {
        out.extend(tls::parse_certs(pem)?);
    }
    if out.is_empty() {
        Err(TransportError::Pairing(
            "relay response missing ca chain".into(),
        ))
    } else {
        Ok(out)
    }
}
