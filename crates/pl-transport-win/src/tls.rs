// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! rustls client configs for the observer's framed-mTLS transport.
//!
//! Trust is **CA-fingerprint pinning**, not a system trust store: a presented
//! certificate chain is accepted only if some cert in it matches the pinned
//! prefix from the pair-link (`observer_pl::ca`), AND the TLS handshake
//! signature verifies against the leaf the peer presented (delegated to the ring
//! provider). The two together defeat a relay that echoes the real CA chain but
//! terminates TLS with its own key — the same property the journal's
//! leaf-signature check added. Hostname is intentionally not validated (we dial
//! raw IPs from the pair-link and pin the CA), so a fixed `spl.local` server
//! name is used.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error as RustlsError, SignatureScheme};

use crate::TransportError;

/// The fixed TLS server name. Hostname is not validated (CA-fp pin is the trust
/// anchor); this is a stable placeholder so SNI/name handling is deterministic.
pub const PINNED_SERVER_NAME: &str = "spl.local";

/// A rustls verifier that pins the journal CA fingerprint prefix and still
/// verifies the handshake signature against the presented leaf.
#[derive(Debug)]
struct CaFpPinVerifier {
    prefix: Vec<u8>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for CaFpPinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        let pinned = observer_pl::ca::cert_matches_prefix(end_entity.as_ref(), &self.prefix)
            || intermediates
                .iter()
                .any(|c| observer_pl::ca::cert_matches_prefix(c.as_ref(), &self.prefix));
        if pinned {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(RustlsError::General(
                "journal CA fingerprint pin mismatch".to_string(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Client config for the **certless pairing** handshake: pins the CA-fp prefix,
/// presents no client certificate.
pub fn pairing_config(ca_fp_prefix: &[u8]) -> Result<ClientConfig, TransportError> {
    let provider = provider();
    let verifier = Arc::new(CaFpPinVerifier {
        prefix: ca_fp_prefix.to_vec(),
        provider: provider.clone(),
    });
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| TransportError::Tls(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    Ok(config)
}

/// Client config for the **established mTLS** session: pins the same CA-fp
/// prefix and presents the client cert + key minted during pairing.
pub fn mtls_config(
    ca_fp_prefix: &[u8],
    client_cert_chain: Vec<CertificateDer<'static>>,
    client_key: PrivateKeyDer<'static>,
) -> Result<ClientConfig, TransportError> {
    let provider = provider();
    let verifier = Arc::new(CaFpPinVerifier {
        prefix: ca_fp_prefix.to_vec(),
        provider: provider.clone(),
    });
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| TransportError::Tls(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(client_cert_chain, client_key)
        .map_err(|e| TransportError::Tls(e.to_string()))?;
    Ok(config)
}

/// Parse PEM certificate text into rustls DER certs. Uses the PEM parser in
/// `rustls-pki-types` directly (the maintained replacement for `rustls-pemfile`).
pub fn parse_certs(pem: &str) -> Result<Vec<CertificateDer<'static>>, TransportError> {
    CertificateDer::pem_slice_iter(pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| TransportError::Tls(format!("bad certificate PEM: {e}")))
}

/// Parse a PKCS#8 (or other) private key PEM into a rustls key.
pub fn parse_private_key(pem: &str) -> Result<PrivateKeyDer<'static>, TransportError> {
    PrivateKeyDer::from_pem_slice(pem.as_bytes())
        .map_err(|e| TransportError::Tls(format!("bad private key PEM: {e}")))
}

/// The pinned [`ServerName`] used for every dial.
pub fn pinned_server_name() -> ServerName<'static> {
    ServerName::try_from(PINNED_SERVER_NAME).expect("spl.local is a valid DNS name")
}
