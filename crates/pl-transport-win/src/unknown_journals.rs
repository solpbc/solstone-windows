// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use observer_model::SyncSnapshot;

fn mark_color_from_spl(color: spl_core::mark::MarkColor) -> observer_model::MarkColor {
    observer_model::MarkColor {
        name: color.name,
        hex: color.hex,
    }
}

fn mark_icon_from_spl(icon: spl_core::mark::MarkIconSpec) -> observer_model::MarkIconSpec {
    observer_model::MarkIconSpec {
        name: icon.name,
        svg: icon.svg,
        color: mark_color_from_spl(icon.color),
        rot: icon.rot,
    }
}

pub(crate) fn mark_spec_from_spl(
    spec: spl_core::mark::MarkRenderSpec,
) -> observer_model::MarkRenderSpec {
    observer_model::MarkRenderSpec {
        icon1: mark_icon_from_spl(spec.icon1),
        icon2: mark_icon_from_spl(spec.icon2),
        words: spec.words,
    }
}

/// The render spec for a journal id, or `None` if `jid` isn't a valid
/// mark-bearing journal id (a dummy test id or unpaired placeholder). Shared by
/// the unknown-journal comparison (the "your journal" side) and ordinary
/// pairing (the paired journal's own mark).
pub(crate) fn mark_spec_for_jid(jid: &str) -> Option<observer_model::MarkRenderSpec> {
    spl_core::mark::mark_from_jid(jid)
        .ok()
        .map(|m| mark_spec_from_spl(m.to_render_spec()))
}

/// Reflect sightings of unknown peer journals into the health snapshot.
///
/// If no unknown journals were seen, or if `expected_jid` is not a valid
/// journal ID (such as a dummy test ID or unpaired placeholder), this publishes
/// an empty list. It never mutates `pairing.phase`, `pairing.detail`, or
/// `upload`.
pub(crate) fn publish_unknown_journals(
    snapshot: &mut SyncSnapshot,
    transport: &spl_transport::TransportClient,
    expected_jid: &str,
) {
    let sightings = transport.unknown_journals();
    if sightings.is_empty() {
        snapshot.unknown_journals = Vec::new();
        return;
    }

    let Some(expected_mark) = mark_spec_for_jid(expected_jid) else {
        snapshot.unknown_journals = Vec::new();
        return;
    };

    let mut result = Vec::with_capacity(sightings.len());
    for sighting in sightings {
        let responding_mark = sighting.jid.as_deref().and_then(mark_spec_for_jid);
        result.push(observer_model::UnknownJournalSighting {
            address: sighting.address,
            expected_mark: expected_mark.clone(),
            responding_mark,
        });
    }

    snapshot.unknown_journals = result;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::ObserverClient;
    use crate::credential::{Credential, EndpointAddr};
    use observer_model::PairingPhase;
    use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};

    fn test_credential(instance_id: &str, port: u16, pin: Vec<u8>) -> Credential {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let params = CertificateParams::new(vec!["observer.test".to_string()]).unwrap();
        let cert = params.self_signed(&key).unwrap();
        Credential {
            client_key_pem: key.serialize_pem(),
            client_cert_pem: cert.pem(),
            ca_chain_pem: vec![cert.pem()],
            ca_fp_prefix: pin,
            instance_id: instance_id.to_string(),
            home_label: "Home".into(),
            endpoints: vec![EndpointAddr {
                host: "127.0.0.1".into(),
                port,
            }],
            relay_origin: None,
            device_token: None,
            device_token_expires_at: None,
        }
    }

    #[test]
    fn invalid_expected_jid_publishes_empty_list_and_preserves_phase() {
        let cred = test_credential("test", 1234, vec![0; 16]);
        let client = ObserverClient::new(cred).unwrap();
        let mut snapshot = SyncSnapshot::default();
        snapshot.pairing.phase = PairingPhase::Paired;

        publish_unknown_journals(&mut snapshot, client.transport_client(), "test");
        assert!(snapshot.unknown_journals.is_empty());
        assert_eq!(snapshot.pairing.phase, PairingPhase::Paired);
    }

    #[test]
    fn mark_spec_from_spl_converts_correctly() {
        let spl_spec = spl_core::mark::MarkRenderSpec {
            icon1: spl_core::mark::MarkIconSpec {
                name: "piano".into(),
                svg: "<path d=\"...\"/>".into(),
                color: spl_core::mark::MarkColor {
                    name: "blue".into(),
                    hex: "#3b82f6".into(),
                },
                rot: 45,
            },
            icon2: spl_core::mark::MarkIconSpec {
                name: "key".into(),
                svg: "<path d=\"...\"/>".into(),
                color: spl_core::mark::MarkColor {
                    name: "purple".into(),
                    hex: "#a855f7".into(),
                },
                rot: 0,
            },
            words: ["liquefy".into(), "smock".into()],
        };

        let converted = mark_spec_from_spl(spl_spec);
        assert_eq!(converted.icon1.name, "piano");
        assert_eq!(converted.icon1.color.name, "blue");
        assert_eq!(converted.icon1.color.hex, "#3b82f6");
        assert_eq!(converted.icon1.rot, 45);
        assert_eq!(converted.icon2.name, "key");
        assert_eq!(converted.icon2.color.name, "purple");
        assert_eq!(converted.icon2.color.hex, "#a855f7");
        assert_eq!(converted.icon2.rot, 0);
        assert_eq!(converted.words, ["liquefy", "smock"]);
    }

    #[test]
    fn mark_spec_for_jid_is_none_for_invalid_jid() {
        assert!(mark_spec_for_jid("not-a-jid").is_none());
    }

    #[test]
    fn mark_spec_for_jid_matches_direct_conversion_for_valid_jid() {
        let jid = "f30ed159-ef46-8e9c-913f-e49f0fe7d201";
        let direct =
            mark_spec_from_spl(spl_core::mark::mark_from_jid(jid).unwrap().to_render_spec());
        let via_helper = mark_spec_for_jid(jid).expect("valid jid yields a mark");
        assert_eq!(direct, via_helper);
    }

    #[test]
    fn valid_expected_jid_with_empty_sightings_publishes_empty_list() {
        let valid_jid = "f30ed159-ef46-8e9c-913f-e49f0fe7d201";
        let cred = test_credential(valid_jid, 1234, vec![0; 16]);
        let client = ObserverClient::new(cred).unwrap();
        let mut snapshot = SyncSnapshot::default();
        snapshot.pairing.phase = PairingPhase::Paired;

        publish_unknown_journals(&mut snapshot, client.transport_client(), valid_jid);
        assert!(snapshot.unknown_journals.is_empty());
        assert_eq!(snapshot.pairing.phase, PairingPhase::Paired);
    }

    #[tokio::test]
    async fn unknown_journal_sighting_populates_snapshot_marks_without_mutating_pairing_phase() {
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
        use rustls::ServerConfig;
        use std::sync::Arc;
        use tokio::net::TcpListener;
        use tokio_rustls::TlsAcceptor;

        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let params = CertificateParams::new(vec!["spl.local".to_string()]).unwrap();
        let cert = params.self_signed(&key).unwrap();
        let cert_der = CertificateDer::from(cert.der().to_vec());
        let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));

        let server_spki = spl_core::ca::extract_spki_der(cert_der.as_ref()).unwrap();
        let responding_jid = spl_core::relay_window::jid_from_spki(&server_spki).unwrap();

        let server_cfg =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![cert_der], key_der)
                .unwrap();

        let acceptor = TlsAcceptor::from(Arc::new(server_cfg));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server_task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let _ = acceptor.accept(stream).await;
                });
            }
        });

        // Credential with a mismatched pin and valid expected JID
        let expected_jid = "f30ed159-ef46-8e9c-913f-e49f0fe7d201";
        let cred = test_credential(expected_jid, port, vec![0xAA; 16]);

        let client = ObserverClient::new(cred).unwrap();
        let _ = client.ingest_manifest().await;

        let mut snapshot = SyncSnapshot::default();
        snapshot.pairing.phase = PairingPhase::Paired;
        snapshot.pairing.detail = None;

        publish_unknown_journals(
            &mut snapshot,
            client.transport_client(),
            &client.credential().instance_id,
        );

        assert_eq!(snapshot.pairing.phase, PairingPhase::Paired);
        assert_eq!(snapshot.pairing.detail, None);
        assert_eq!(snapshot.unknown_journals.len(), 1);

        let sighting = &snapshot.unknown_journals[0];
        assert!(sighting
            .address
            .as_ref()
            .unwrap()
            .contains(&port.to_string()));

        let expected_mark_model = spl_core::mark::mark_from_jid(expected_jid)
            .unwrap()
            .to_render_spec();
        assert_eq!(sighting.expected_mark.words, expected_mark_model.words);
        assert_eq!(
            sighting.expected_mark.icon1.name,
            expected_mark_model.icon1.name
        );

        let responding_mark = sighting
            .responding_mark
            .as_ref()
            .expect("responding mark must be present");
        let responding_mark_model = spl_core::mark::mark_from_jid(&responding_jid)
            .unwrap()
            .to_render_spec();
        assert_eq!(responding_mark.words, responding_mark_model.words);
        assert_eq!(responding_mark.icon1.name, responding_mark_model.icon1.name);

        server_task.abort();
    }
}
