// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The pairing handshake adapter.
//!
//! Production pairing delegates directly to `spl_transport`'s shared implementation.
//! This module adapts types, translates errors, and validates route usability
//! across the Windows boundary.

use crate::credential::{Credential, EndpointAddr};
use crate::{ObserverHandle, RelayControlEndpoint, RelayError, TransportError};

/// Parse a `https://go.solstone.app/p#…` pair-link and pair against it.
pub async fn pair_from_link(link: &str, device_label: &str) -> Result<Credential, TransportError> {
    pair_from_link_observed(link, device_label, None).await
}

/// [`pair_from_link`] with an operation-scoped observation seam attached.
pub async fn pair_from_link_observed(
    link: &str,
    device_label: &str,
    observer: ObserverHandle,
) -> Result<Credential, TransportError> {
    let empty_map = serde_json::Map::new();
    let result = spl_transport::pairing::pair_from_link_observed(
        link,
        device_label,
        &empty_map,
        observer.as_deref(),
    )
    .await;

    handle_shared_pairing_result(result)
}

pub(crate) fn handle_shared_pairing_result(
    result: Result<spl_transport::credential::Credential, spl_transport::TransportError>,
) -> Result<Credential, TransportError> {
    match result {
        Ok(shared_cred) => {
            let cred = convert_shared_credential(shared_cred);
            gate_usable_route(&cred)?;
            Ok(cred)
        }
        Err(e) => Err(map_shared_error(e)),
    }
}

pub fn convert_shared_credential(cred: spl_transport::credential::Credential) -> Credential {
    Credential {
        client_key_pem: cred.client_key_pem,
        client_cert_pem: cred.client_cert_pem,
        ca_chain_pem: cred.ca_chain_pem,
        ca_fp_prefix: cred.ca_fp_prefix,
        instance_id: cred.instance_id,
        home_label: cred.home_label,
        endpoints: cred
            .endpoints
            .into_iter()
            .map(|e| EndpointAddr {
                host: e.host,
                port: e.port,
            })
            .collect(),
        relay_origin: cred.relay_origin,
        device_token: cred.device_token,
        device_token_expires_at: cred.device_token_expires_at,
    }
}

/// Convert the persisted Windows credential to the shared transport shape.
///
/// The shared-only pairing hints are intentionally absent from Windows durable
/// state and must remain so for compatibility with existing pairing files.
pub fn windows_to_shared_credential(cred: &Credential) -> spl_transport::credential::Credential {
    spl_transport::credential::Credential {
        client_key_pem: cred.client_key_pem.clone(),
        client_cert_pem: cred.client_cert_pem.clone(),
        ca_chain_pem: cred.ca_chain_pem.clone(),
        ca_fp_prefix: cred.ca_fp_prefix.clone(),
        instance_id: cred.instance_id.clone(),
        home_label: cred.home_label.clone(),
        endpoints: cred
            .endpoints
            .iter()
            .map(|endpoint| spl_transport::credential::EndpointAddr {
                host: endpoint.host.clone(),
                port: endpoint.port,
            })
            .collect(),
        relay_origin: cred.relay_origin.clone(),
        device_token: cred.device_token.clone(),
        device_token_expires_at: cred.device_token_expires_at,
        home_attestation: None,
        local_endpoints: None,
    }
}

pub(crate) fn gate_usable_route(cred: &Credential) -> Result<(), TransportError> {
    if !cred.endpoints.is_empty()
        || matches!(
            (cred.relay_origin.as_deref(), cred.device_token.as_deref()),
            (Some(origin), Some(token)) if !origin.is_empty() && !token.is_empty()
        )
    {
        Ok(())
    } else {
        Err(TransportError::NoEndpoint)
    }
}

pub(crate) fn map_shared_error(err: spl_transport::TransportError) -> TransportError {
    match err {
        spl_transport::TransportError::Io(e) => TransportError::Io(e),
        spl_transport::TransportError::Tls(e) => TransportError::Tls(e),
        spl_transport::TransportError::TlsAccessDenied => {
            TransportError::Tls("tls access denied".into())
        }
        spl_transport::TransportError::TlsCertificateUnknown => {
            TransportError::Tls("tls certificate unknown".into())
        }
        spl_transport::TransportError::TlsRefused => TransportError::Tls("tls refused".into()),
        spl_transport::TransportError::UnknownJournal(_) => {
            TransportError::Tls("unknown journal".into())
        }
        spl_transport::TransportError::Crypto(e) => TransportError::Crypto(e),
        spl_transport::TransportError::Mux(e) => TransportError::Mux(e),
        spl_transport::TransportError::Http(e) => TransportError::Http(e),
        spl_transport::TransportError::Json(e) => TransportError::Json(e),
        spl_transport::TransportError::PairLink(e) => TransportError::PairLink(e),
        spl_transport::TransportError::Pairing(e) => TransportError::Pairing(e),
        spl_transport::TransportError::Rejected { status, body } => {
            TransportError::Rejected { status, body }
        }
        spl_transport::TransportError::Relay(r) => match r {
            spl_transport::RelayError::HomeOffline => {
                TransportError::Relay(RelayError::HomeOffline)
            }
            spl_transport::RelayError::Unauthorized => {
                TransportError::Relay(RelayError::Unauthorized)
            }
            spl_transport::RelayError::Unpaid => TransportError::Relay(RelayError::Unpaid),
            spl_transport::RelayError::UnknownInstance => {
                TransportError::Relay(RelayError::UnknownInstance)
            }
            spl_transport::RelayError::PairWindowClosed => {
                TransportError::Relay(RelayError::PairWindowClosed)
            }
            spl_transport::RelayError::Overflow => TransportError::Relay(RelayError::Overflow),
            spl_transport::RelayError::Abnormal => TransportError::Relay(RelayError::Abnormal),
            spl_transport::RelayError::UpgradeRejected
            | spl_transport::RelayError::HomeListenConnection
            | spl_transport::RelayError::HomeRelayConfiguration
            | spl_transport::RelayError::HomeTunnelRejected(_) => {
                TransportError::Relay(RelayError::UpgradeRejected)
            }
            spl_transport::RelayError::Stalled => TransportError::Relay(RelayError::Stalled),
        },
        spl_transport::TransportError::RelayControlRejected { endpoint, status } => {
            let ep = match endpoint {
                spl_transport::RelayControlEndpoint::EnrollDevice => {
                    RelayControlEndpoint::EnrollDevice
                }
                spl_transport::RelayControlEndpoint::TokenRefresh => {
                    RelayControlEndpoint::TokenRefresh
                }
            };
            TransportError::RelayControlRejected {
                endpoint: ep,
                status,
            }
        }
        spl_transport::TransportError::NoEndpoint => TransportError::NoEndpoint,
        spl_transport::TransportError::NotPaired => TransportError::NotPaired,
        spl_transport::TransportError::LocalOffset => TransportError::LocalOffset,
    }
}

/// Preserve the established Windows transport vocabulary at the shared request
/// boundary while giving replay and publication outcomes their own stable
/// local classifications.
pub(crate) fn map_request_error(err: spl_transport::request::RequestError) -> TransportError {
    match err {
        spl_transport::request::RequestError::Transport(inner) => map_shared_error(inner),
        spl_transport::request::RequestError::ReplayUnsafe(_) => TransportError::ReplayUnsafe,
        spl_transport::request::RequestError::RelayDisabled => TransportError::RelayDisabled,
        spl_transport::request::RequestError::RelayRetired => TransportError::RelayRetired,
        spl_transport::request::RequestError::PublicationRejected => {
            TransportError::RelayPublicationRejected
        }
        spl_transport::request::RequestError::PublicationIndeterminate => {
            TransportError::RelayPublicationIndeterminate
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport_error_code;

    #[test]
    fn usable_route_gate_matches_transport_constructor_predicate() {
        let mut cred = Credential {
            client_key_pem: "key".into(),
            client_cert_pem: "cert".into(),
            ca_chain_pem: vec!["ca".into()],
            ca_fp_prefix: vec![1, 2, 3],
            instance_id: "inst".into(),
            home_label: "Home".into(),
            endpoints: vec![],
            relay_origin: None,
            device_token: None,
            device_token_expires_at: None,
        };
        assert!(matches!(
            gate_usable_route(&cred),
            Err(TransportError::NoEndpoint)
        ));

        cred.endpoints.push(EndpointAddr {
            host: "127.0.0.1".into(),
            port: 7657,
        });
        assert!(gate_usable_route(&cred).is_ok());

        cred.endpoints.clear();
        cred.device_token = Some("token".into());
        assert!(matches!(
            gate_usable_route(&cred),
            Err(TransportError::NoEndpoint)
        ));

        cred.relay_origin = Some("".into());
        assert!(matches!(
            gate_usable_route(&cred),
            Err(TransportError::NoEndpoint)
        ));

        cred.relay_origin = Some("https://relay.example.com".into());
        cred.device_token = Some("".into());
        assert!(matches!(
            gate_usable_route(&cred),
            Err(TransportError::NoEndpoint)
        ));

        cred.device_token = Some("token".into());
        assert!(gate_usable_route(&cred).is_ok());
    }

    fn transport_error_is_retryable(error: &TransportError) -> bool {
        match error {
            TransportError::Io(_) | TransportError::Tls(_) | TransportError::NoEndpoint => true,
            TransportError::Relay(relay) => matches!(
                relay,
                RelayError::HomeOffline
                    | RelayError::Abnormal
                    | RelayError::Overflow
                    | RelayError::Stalled
            ),
            TransportError::Crypto(_)
            | TransportError::Mux(_)
            | TransportError::Http(_)
            | TransportError::Json(_)
            | TransportError::PairLink(_)
            | TransportError::Pairing(_)
            | TransportError::Ingest(_)
            | TransportError::Rejected { .. }
            | TransportError::RelayControlRejected { .. }
            | TransportError::ReplayUnsafe
            | TransportError::RelayDisabled
            | TransportError::RelayRetired
            | TransportError::RelayPublicationRejected
            | TransportError::RelayPublicationIndeterminate
            | TransportError::NotPaired
            | TransportError::LocalOffset => false,
        }
    }

    #[test]
    fn error_mapper_covers_all_shared_variants_without_secret_leakage() {
        let cases = [
            (
                spl_transport::TransportError::Io(std::io::Error::other("secret-path")),
                "io",
                true,
            ),
            (
                spl_transport::TransportError::Tls("secret-host".into()),
                "tls",
                true,
            ),
            (spl_transport::TransportError::TlsAccessDenied, "tls", true),
            (
                spl_transport::TransportError::TlsCertificateUnknown,
                "tls",
                true,
            ),
            (spl_transport::TransportError::TlsRefused, "tls", true),
            (
                spl_transport::TransportError::UnknownJournal(spl_transport::UnknownJournal {
                    address: Some("secret-host:7657".into()),
                    jid: Some("secret-jid".into()),
                }),
                "tls",
                true,
            ),
            (
                spl_transport::TransportError::Crypto("secret-key".into()),
                "crypto",
                false,
            ),
            (
                spl_transport::TransportError::Mux(spl_core::mux::MuxError::Incomplete),
                "mux",
                false,
            ),
            (
                spl_transport::TransportError::Http(spl_core::http::HttpError::MissingStatusLine),
                "http",
                false,
            ),
            (
                spl_transport::TransportError::Json(
                    serde_json::from_str::<serde_json::Value>("{").unwrap_err(),
                ),
                "json",
                false,
            ),
            (
                spl_transport::TransportError::PairLink("secret-link".into()),
                "pair_link",
                false,
            ),
            (
                spl_transport::TransportError::Pairing("secret-material".into()),
                "pairing",
                false,
            ),
            (
                spl_transport::TransportError::Rejected {
                    status: 503,
                    body: "secret-body".into(),
                },
                "http_503",
                false,
            ),
            (
                spl_transport::TransportError::Relay(spl_transport::RelayError::HomeOffline),
                "relay_home_offline",
                true,
            ),
            (
                spl_transport::TransportError::Relay(spl_transport::RelayError::Unauthorized),
                "relay_unauthorized",
                false,
            ),
            (
                spl_transport::TransportError::Relay(spl_transport::RelayError::Unpaid),
                "relay_unpaid",
                false,
            ),
            (
                spl_transport::TransportError::Relay(spl_transport::RelayError::UnknownInstance),
                "relay_unknown_instance",
                false,
            ),
            (
                spl_transport::TransportError::Relay(spl_transport::RelayError::PairWindowClosed),
                "relay_pair_window_closed",
                false,
            ),
            (
                spl_transport::TransportError::Relay(spl_transport::RelayError::Overflow),
                "relay_overflow",
                true,
            ),
            (
                spl_transport::TransportError::Relay(spl_transport::RelayError::Abnormal),
                "relay_abnormal",
                true,
            ),
            (
                spl_transport::TransportError::Relay(spl_transport::RelayError::UpgradeRejected),
                "relay_upgrade_rejected",
                false,
            ),
            (
                spl_transport::TransportError::Relay(
                    spl_transport::RelayError::HomeListenConnection,
                ),
                "relay_upgrade_rejected",
                false,
            ),
            (
                spl_transport::TransportError::Relay(
                    spl_transport::RelayError::HomeRelayConfiguration,
                ),
                "relay_upgrade_rejected",
                false,
            ),
            (
                spl_transport::TransportError::Relay(
                    spl_transport::RelayError::HomeTunnelRejected(500),
                ),
                "relay_upgrade_rejected",
                false,
            ),
            (
                spl_transport::TransportError::Relay(spl_transport::RelayError::Stalled),
                "relay_stalled",
                true,
            ),
            (
                spl_transport::TransportError::RelayControlRejected {
                    endpoint: spl_transport::RelayControlEndpoint::EnrollDevice,
                    status: 409,
                },
                "relay_enroll_device_http_409",
                false,
            ),
            (
                spl_transport::TransportError::RelayControlRejected {
                    endpoint: spl_transport::RelayControlEndpoint::TokenRefresh,
                    status: 404,
                },
                "relay_refresh_http_404",
                false,
            ),
            (
                spl_transport::TransportError::NoEndpoint,
                "no_endpoint",
                true,
            ),
            (
                spl_transport::TransportError::NotPaired,
                "not_paired",
                false,
            ),
            (
                spl_transport::TransportError::LocalOffset,
                "local_offset",
                false,
            ),
        ];

        for (shared_err, expected_code, expected_retryable) in cases {
            let mapped = map_shared_error(shared_err);
            assert_eq!(transport_error_code(&mapped), expected_code);
            assert_eq!(transport_error_is_retryable(&mapped), expected_retryable);
        }
    }

    #[test]
    fn pairing_adapter_never_emits_shared_tls_extra_tokens() {
        let cases = [
            spl_transport::TransportError::TlsAccessDenied,
            spl_transport::TransportError::TlsCertificateUnknown,
            spl_transport::TransportError::TlsRefused,
            spl_transport::TransportError::UnknownJournal(spl_transport::UnknownJournal {
                address: None,
                jid: None,
            }),
        ];
        for err in cases {
            let mapped = map_shared_error(err);
            let code = transport_error_code(&mapped);
            assert_ne!(code, "tls_access_denied");
            assert_ne!(code, "tls_certificate_unknown");
            assert_ne!(code, "tls_refused");
            assert_ne!(code, "unknown_journal");
            assert_eq!(code, "tls");
        }
    }
}
