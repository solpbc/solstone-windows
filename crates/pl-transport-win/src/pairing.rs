// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The pairing handshake adapter.
//!
//! Production pairing delegates directly to `spl_transport`'s shared implementation.
//! This module adapts types, translates errors, copies operation observations,
//! and validates route usability across the Windows boundary.

use observer_model::TransportPath;

use crate::credential::{Credential, EndpointAddr};
use crate::observe::ObserverHandle;
use crate::{RelayControlEndpoint, RelayError, TransportError};

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
    let shared_observer = spl_transport::observe::OperationObserver::new_unshared();
    let empty_map = serde_json::Map::new();
    let result = spl_transport::pairing::pair_from_link_observed(
        link,
        device_label,
        &empty_map,
        Some(&shared_observer),
    )
    .await;

    handle_shared_pairing_result(result, &shared_observer, observer)
}

pub(crate) fn handle_shared_pairing_result(
    result: Result<spl_transport::credential::Credential, spl_transport::TransportError>,
    source_observer: &spl_transport::observe::OperationObserver,
    dest_observer: ObserverHandle,
) -> Result<Credential, TransportError> {
    match result {
        Ok(shared_cred) => {
            let cred = convert_shared_credential(shared_cred);
            if let Err(e) = gate_usable_route(&cred) {
                copy_shared_observation(source_observer, &dest_observer, true);
                Err(e)
            } else {
                copy_shared_observation(source_observer, &dest_observer, false);
                Ok(cred)
            }
        }
        Err(e) => {
            copy_shared_observation(source_observer, &dest_observer, true);
            Err(map_shared_error(e))
        }
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

pub(crate) fn gate_usable_route(cred: &Credential) -> Result<(), TransportError> {
    if cred.endpoints.is_empty() && cred.device_token.is_none() {
        Err(TransportError::NoEndpoint)
    } else {
        Ok(())
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
        spl_transport::TransportError::Crypto(e) => TransportError::Crypto(e),
        spl_transport::TransportError::Mux(e) => {
            let converted = match e {
                spl_core::mux::MuxError::Frame(f) => {
                    let f_conv = match f {
                        spl_core::frame::FrameError::PayloadTooLarge(len) => {
                            observer_pl::frame::FrameError::PayloadTooLarge(len)
                        }
                        spl_core::frame::FrameError::ReservedFlag(flag) => {
                            observer_pl::frame::FrameError::ReservedFlag(flag)
                        }
                    };
                    observer_pl::mux::MuxError::Frame(f_conv)
                }
                spl_core::mux::MuxError::StreamReset => observer_pl::mux::MuxError::StreamReset,
                spl_core::mux::MuxError::Incomplete => observer_pl::mux::MuxError::Incomplete,
                spl_core::mux::MuxError::Http(h) => {
                    let h_conv = match h {
                        spl_core::http::HttpError::MissingTerminator => {
                            observer_pl::http::HttpError::MissingTerminator
                        }
                        spl_core::http::HttpError::MissingStatusLine => {
                            observer_pl::http::HttpError::MissingStatusLine
                        }
                        spl_core::http::HttpError::BadStatusLine(s) => {
                            observer_pl::http::HttpError::BadStatusLine(s)
                        }
                        spl_core::http::HttpError::TruncatedBody => {
                            observer_pl::http::HttpError::TruncatedBody
                        }
                        spl_core::http::HttpError::BadChunkedBody(s) => {
                            observer_pl::http::HttpError::BadChunkedBody(s)
                        }
                    };
                    observer_pl::mux::MuxError::Http(h_conv)
                }
                spl_core::mux::MuxError::CapExceeded => observer_pl::mux::MuxError::CapExceeded,
                spl_core::mux::MuxError::FlowControl => observer_pl::mux::MuxError::FlowControl,
                spl_core::mux::MuxError::Protocol(p) => {
                    observer_pl::mux::MuxError::Protocol(observer_pl::frame::FrameViolation {
                        stream_id: p.stream_id,
                        flags: p.flags,
                        length: p.length,
                    })
                }
            };
            TransportError::Mux(converted)
        }
        spl_transport::TransportError::Http(e) => {
            let converted = match e {
                spl_core::http::HttpError::MissingTerminator => {
                    observer_pl::http::HttpError::MissingTerminator
                }
                spl_core::http::HttpError::MissingStatusLine => {
                    observer_pl::http::HttpError::MissingStatusLine
                }
                spl_core::http::HttpError::BadStatusLine(s) => {
                    observer_pl::http::HttpError::BadStatusLine(s)
                }
                spl_core::http::HttpError::TruncatedBody => {
                    observer_pl::http::HttpError::TruncatedBody
                }
                spl_core::http::HttpError::BadChunkedBody(s) => {
                    observer_pl::http::HttpError::BadChunkedBody(s)
                }
            };
            TransportError::Http(converted)
        }
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

pub(crate) fn copy_shared_observation(
    source: &spl_transport::observe::OperationObserver,
    dest: &ObserverHandle,
    suppress_selected_path: bool,
) {
    if let Some(dest) = dest {
        let snapshot = source.snapshot();
        for _ in 0..snapshot.dial_attempts {
            dest.record_dial_attempt();
        }
        for _ in 0..snapshot.direct_successes {
            dest.record_dial_success(TransportPath::Direct);
        }
        for _ in 0..snapshot.relay_successes {
            dest.record_dial_success(TransportPath::Relay);
        }
        dest.record_request_bytes(snapshot.request_bytes_sent);
        if snapshot.close_completed {
            dest.record_close_completed();
        }
        if snapshot.legacy_enrollment_possible {
            dest.record_enrollment_started();
        } else {
            dest.record_stateless_enrollment();
        }
        for _ in 0..snapshot.enrollment_events {
            dest.record_enrollment_event();
        }
        if !suppress_selected_path {
            let mapped_path = match snapshot.selected_path {
                Some(spl_transport::request::SelectedPath::Direct) => Some(TransportPath::Direct),
                Some(spl_transport::request::SelectedPath::Relay) => Some(TransportPath::Relay),
                None => None,
            };
            dest.record_selected_path(mapped_path);
        } else {
            dest.record_selected_path(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observe::OperationObserver;
    use crate::transport_error_code;

    #[test]
    fn usable_route_gate_requires_either_endpoint_or_device_token() {
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
        ];
        for err in cases {
            let mapped = map_shared_error(err);
            let code = transport_error_code(&mapped);
            assert_ne!(code, "tls_access_denied");
            assert_ne!(code, "tls_certificate_unknown");
            assert_eq!(code, "tls");
        }
    }

    #[test]
    fn observation_copy_propagates_counts_and_respects_suppression() {
        let source = spl_transport::observe::OperationObserver::new_unshared();
        source.record_dial_attempt();
        source.record_dial_attempt();
        source.record_relay_success();
        source.record_request_bytes(256);
        source.record_close_completed();
        source.record_legacy_enrollment_possible();
        source.record_enrollment();
        source.record_selected_path(spl_transport::request::SelectedPath::Relay);

        let dest = OperationObserver::new();
        copy_shared_observation(&source, &Some(dest.clone()), false);

        let counts = dest.counts();
        assert_eq!(counts.dial_attempts, 2);
        assert_eq!(counts.relay_successes, 1);
        assert_eq!(counts.request_bytes_sent, 256);
        assert!(counts.close_completed);
        assert!(dest.legacy_enrollment_possible());
        assert_eq!(dest.enrollment_events(), 1);
        assert_eq!(dest.selected_path(), Some(TransportPath::Relay));

        let dest_suppressed = OperationObserver::new();
        copy_shared_observation(&source, &Some(dest_suppressed.clone()), true);
        assert_eq!(dest_suppressed.selected_path(), None);
    }
}
