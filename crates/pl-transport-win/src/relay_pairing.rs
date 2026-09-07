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

use crate::credential::{endpoint_addrs_from_local_endpoints, generate_csr, hex_lower, Credential};
use crate::observe::{note_dial_attempt, note_dial_success, ObserverHandle};
use crate::{relay, relay_http, spki_pin, tls, RelayControlEndpoint, TransportError};
use observer_model::TransportPath;

#[derive(Deserialize)]
struct EnrollResponse {
    device_token: String,
    #[serde(default, deserialize_with = "negotiated_version")]
    protocol_version: Option<u8>,
    expires_at: Option<String>,
}

pub async fn pair_over_relay(
    link: &RelayPairLink,
    device_label: &str,
) -> Result<Credential, TransportError> {
    pair_over_relay_observed(link, device_label, None).await
}

/// [`pair_over_relay`] with an operation-scoped observation seam attached.
///
/// The ceremony's dials are the pair-dial websocket, the inner TLS handshake
/// carried on it, and the relay `/enroll/device` control request. All three are
/// counted, so a reported "total dial attempts" for `pair` omits no class of dial.
pub async fn pair_over_relay_observed(
    link: &RelayPairLink,
    device_label: &str,
    observer: ObserverHandle,
) -> Result<Credential, TransportError> {
    let rk = observer_pl::relay_window::derive_rk(&link.s);
    let url = observer_pl::relay::pair_dial_url(&link.relay_origin)
        .map_err(|e| TransportError::PairLink(format!("relay origin: {e}")))?;
    note_dial_attempt(&observer);
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
    note_dial_attempt(&observer);
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
    note_dial_success(&observer, TransportPath::Relay);
    let peer_leaf =
        peer_leaf.ok_or_else(|| TransportError::Pairing("relay missing peer leaf".into()))?;
    if !response.is_success() {
        return Err(TransportError::Rejected {
            status: response.status,
            body: "relay pairing rejected".into(),
        });
    }

    let pair: PairResponse = serde_json::from_slice(&response.body)
        .map_err(|_| TransportError::Pairing("relay pair response malformed".into()))?;
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

    let cert_spki = ca::extract_spki_der(client_cert_der.as_ref())
        .map_err(|_| TransportError::Pairing("client certificate public key malformed".into()))?;
    if cert_spki != generated.public_key_spki_der {
        return Err(TransportError::Pairing(
            "client certificate public key does not match generated key".into(),
        ));
    }
    let device_token = if let Some(raw) = pair.relay_access.as_ref() {
        let access: observer_pl::relay_access::RelayAccess = serde_json::from_value(raw.clone())
            .map_err(|_| TransportError::Pairing("relay bootstrap malformed".into()))?;
        if !relay_http::same_relay_origin(&access.relay_origin, &link.relay_origin)?
            || access.claims(&pair.instance_id, unix_now()).is_none()
        {
            return Err(TransportError::Pairing("relay bootstrap malformed".into()));
        }
        Some(access.device_token)
    } else {
        // An older home may omit bootstrap. Its committed pairing survives a
        // failed optional enrollment and retains any supplied LAN endpoints.
        match pair.home_attestation.as_deref() {
            Some(attestation) => enroll_device(
                &link.relay_origin,
                &pair.instance_id,
                attestation,
                &observer,
            )
            .await
            .ok(),
            None => None,
        }
    };
    let device_token_expires_at = device_token
        .as_deref()
        .and_then(observer_pl::jwt::decode_unverified_claims)
        .map(|claims| claims.exp);
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
        relay_origin: device_token.as_ref().map(|_| link.relay_origin.clone()),
        device_token,
        device_token_expires_at,
    })
}

async fn enroll_device(
    relay_origin: &str,
    instance_id: &str,
    home_attestation: &str,
    observer: &ObserverHandle,
) -> Result<String, TransportError> {
    let body = serde_json::to_vec(&json!({
        "protocol_version": 2,
        "instance_id": instance_id,
        "home_attestation": home_attestation,
    }))?;
    note_dial_attempt(observer);
    if let Some(observer) = observer {
        observer.record_enrollment_started();
    }
    let response = relay_http::relay_https_post_json(relay_origin, "/enroll/device", &body).await?;
    note_dial_success(observer, TransportPath::Relay);
    if !response.is_success() {
        return Err(TransportError::RelayControlRejected {
            endpoint: RelayControlEndpoint::EnrollDevice,
            status: response.status,
        });
    }
    let parsed: EnrollResponse = serde_json::from_slice(&response.body)
        .map_err(|_| TransportError::Pairing("relay enroll response malformed".into()))?;
    let valid = match parsed.protocol_version {
        Some(2) => parsed.expires_at.as_deref().is_some_and(|expiry| {
            observer_pl::relay_access::negotiated_claims(
                2,
                &parsed.device_token,
                expiry,
                instance_id,
                unix_now(),
            )
            .is_some()
        }),
        None => {
            observer_pl::relay_access::legacy_claims(&parsed.device_token, instance_id, unix_now())
                .is_some()
        }
        Some(_) => false,
    };
    if !valid {
        return Err(TransportError::Pairing(
            "relay enroll response malformed".into(),
        ));
    }
    if parsed.protocol_version == Some(2) {
        if let Some(observer) = observer {
            observer.record_stateless_enrollment();
        }
    }
    Ok(parsed.device_token)
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

pub(crate) fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
}

fn negotiated_version<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u8>, D::Error> {
    u8::deserialize(deserializer).map(Some)
}
