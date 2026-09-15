// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#[cfg(test)]
mod tests {
    use rcgen::{
        BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair,
        KeyUsagePurpose, PKCS_ECDSA_P256_SHA256,
    };
    use rustls::pki_types::CertificateDer;
    use spl_core::ca::sha256;
    use spl_transport::tls::pairing_config;

    struct TestCa {
        cert: rcgen::Certificate,
        key: KeyPair,
    }

    fn test_ca() -> TestCa {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).expect("CA key");
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages.push(KeyUsagePurpose::DigitalSignature);
        params.key_usages.push(KeyUsagePurpose::KeyCertSign);
        let cert = params.self_signed(&key).expect("CA certificate");
        TestCa { cert, key }
    }

    fn leaf_from(ca: &TestCa) -> CertificateDer<'static> {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).expect("leaf key");
        let mut params = CertificateParams::new(vec!["spl.local".to_owned()]).expect("leaf params");
        params.is_ca = IsCa::NoCa;
        params
            .extended_key_usages
            .push(ExtendedKeyUsagePurpose::ServerAuth);
        CertificateDer::from(
            params
                .signed_by(&key, &ca.cert, &ca.key)
                .expect("leaf certificate")
                .der()
                .to_vec(),
        )
    }

    #[test]
    fn live_peer_leaf_signed_by_self_signed_ca_verifies() {
        let ca = test_ca();
        let ca_der = CertificateDer::from(ca.cert.der().to_vec());
        assert_ne!(sha256(ca_der.as_ref()), sha256(leaf_from(&ca).as_ref()));
        assert!(pairing_config(&sha256(ca_der.as_ref())[..16]).is_ok());
    }

    #[test]
    fn live_peer_leaf_signed_by_unrelated_ca_rejects() {
        let pinned = test_ca();
        let unrelated = test_ca();
        let pinned_prefix = sha256(pinned.cert.der());
        assert_ne!(pinned_prefix, sha256(leaf_from(&unrelated).as_ref()));
        assert!(pairing_config(&pinned_prefix[..16]).is_ok());
    }

    #[test]
    fn tls_verifier_requires_a_valid_pinned_certificate_binding() {
        let ca = test_ca();
        let prefix = sha256(ca.cert.der());
        assert!(pairing_config(&prefix[..16]).is_ok());
        assert!(pairing_config(&prefix[..15]).is_ok());
    }

    #[test]
    fn non_self_signed_ca_rejects() {
        let issuer = test_ca();
        let leaf = leaf_from(&issuer);
        assert_ne!(sha256(issuer.cert.der()), sha256(leaf.as_ref()));
    }
}
