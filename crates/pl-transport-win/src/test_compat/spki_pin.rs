// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rcgen::{
        BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair,
        KeyUsagePurpose, PKCS_ECDSA_P256_SHA256,
    };
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use rustls::ServerConfig;
    use spl_core::ca::sha256;
    use spl_core::frame::{Frame, FrameDecoder, FLAG_CLOSE, FLAG_DATA};
    use spl_transport::connection::request_once;
    use spl_transport::tls::{mtls_config, pairing_config};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;

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

    struct Leaf {
        cert: CertificateDer<'static>,
        key: PrivateKeyDer<'static>,
    }

    fn leaf_from(ca: &TestCa) -> Leaf {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).expect("leaf key");
        let mut params = CertificateParams::new(vec!["spl.local".to_owned()]).expect("leaf params");
        params.is_ca = IsCa::NoCa;
        params
            .extended_key_usages
            .push(ExtendedKeyUsagePurpose::ServerAuth);
        let cert = params
            .signed_by(&key, &ca.cert, &ca.key)
            .expect("leaf certificate");
        Leaf {
            cert: CertificateDer::from(cert.der().to_vec()),
            key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
        }
    }

    fn server_config(leaf: &Leaf, chain: &CertificateDer<'static>) -> ServerConfig {
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("TLS versions")
            .with_no_client_auth()
            .with_single_cert(vec![leaf.cert.clone(), chain.clone()], leaf.key.clone_key())
            .expect("server certificate")
    }

    async fn next_frame<S: tokio::io::AsyncRead + Unpin>(
        stream: &mut S,
        decoder: &mut FrameDecoder,
    ) -> Frame {
        loop {
            if let Some(frame) = decoder.next_frame().expect("frame decode") {
                return frame;
            }
            let mut bytes = [0u8; 4096];
            let count = stream.read(&mut bytes).await.expect("client frame");
            assert_ne!(count, 0, "client closed before request close");
            decoder.feed(&bytes[..count]);
        }
    }

    async fn serve_one(listener: TcpListener, config: ServerConfig) {
        let (tcp, _) = listener.accept().await.expect("TLS accept");
        let mut tls = TlsAcceptor::from(Arc::new(config))
            .accept(tcp)
            .await
            .expect("TLS handshake");
        let mut decoder = FrameDecoder::new();
        let stream_id = loop {
            let frame = next_frame(&mut tls, &mut decoder).await;
            if frame.flags & FLAG_CLOSE != 0 {
                break frame.stream_id;
            }
        };
        let response = Frame::new(
            stream_id,
            FLAG_DATA | FLAG_CLOSE,
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(),
        )
        .encode()
        .expect("response frame");
        tls.write_all(&response).await.expect("response write");
        tls.flush().await.expect("response flush");
    }

    async fn request(
        config: rustls::ClientConfig,
        server: ServerConfig,
    ) -> Result<(), spl_transport::TransportError> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("TLS listener");
        let port = listener.local_addr().expect("TLS address").port();
        let task = tokio::spawn(serve_one(listener, server));
        let result = request_once(Arc::new(config), "127.0.0.1", port, "GET", "/", &[], b"")
            .await
            .map(|response| {
                assert_eq!(response.status, 200);
            });
        if result.is_ok() {
            task.await.expect("server task");
        } else {
            task.abort();
        }
        result
    }

    #[tokio::test]
    async fn live_peer_leaf_signed_by_self_signed_ca_verifies() {
        let ca = test_ca();
        let ca_der = CertificateDer::from(ca.cert.der().to_vec());
        let leaf = leaf_from(&ca);
        request(
            pairing_config(&sha256(ca_der.as_ref())[..16]).expect("pairing config"),
            server_config(&leaf, &ca_der),
        )
        .await
        .expect("pinned self-signed CA must verify its leaf");
    }

    #[tokio::test]
    async fn live_peer_leaf_signed_by_unrelated_ca_rejects() {
        let pinned = test_ca();
        let unrelated = test_ca();
        let pinned_der = CertificateDer::from(pinned.cert.der().to_vec());
        let unrelated_der = CertificateDer::from(unrelated.cert.der().to_vec());
        let leaf = leaf_from(&unrelated);
        assert!(request(
            pairing_config(&sha256(pinned_der.as_ref())[..16]).expect("pairing config"),
            server_config(&leaf, &unrelated_der),
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn tls_verifier_requires_a_valid_pinned_certificate_binding() {
        let ca = test_ca();
        let ca_der = CertificateDer::from(ca.cert.der().to_vec());
        let leaf = leaf_from(&ca);
        let client = leaf_from(&ca);
        request(
            mtls_config(
                &sha256(ca_der.as_ref())[..16],
                vec![client.cert.clone()],
                client.key,
            )
            .expect("mTLS config"),
            server_config(&leaf, &ca_der),
        )
        .await
        .expect("valid mTLS peer binding");

        let mut corrupt = leaf.cert.as_ref().to_vec();
        *corrupt.last_mut().expect("certificate signature") ^= 1;
        let corrupt_leaf = Leaf {
            cert: CertificateDer::from(corrupt),
            key: leaf.key,
        };
        assert!(request(
            pairing_config(&sha256(ca_der.as_ref())[..16]).expect("pairing config"),
            server_config(&corrupt_leaf, &ca_der),
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn non_self_signed_ca_rejects() {
        let issuer = test_ca();
        let intermediate_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).expect("CA key");
        let mut intermediate_params =
            CertificateParams::new(Vec::<String>::new()).expect("CA params");
        intermediate_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        intermediate_params
            .key_usages
            .push(KeyUsagePurpose::KeyCertSign);
        let intermediate_cert = intermediate_params
            .signed_by(&intermediate_key, &issuer.cert, &issuer.key)
            .expect("intermediate certificate");
        let intermediate = TestCa {
            cert: intermediate_cert,
            key: intermediate_key,
        };
        let intermediate_der = CertificateDer::from(intermediate.cert.der().to_vec());
        let leaf = leaf_from(&intermediate);
        assert!(request(
            pairing_config(&sha256(intermediate_der.as_ref())[..16]).expect("pairing config"),
            server_config(&leaf, &intermediate_der),
        )
        .await
        .is_err());
    }
}
