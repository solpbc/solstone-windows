// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Proof that the transport's observation seam changes nothing it observes.
//!
mod support;

use std::sync::Arc;

use pl_transport_win::client::ObserverClient;
use spl_transport::observe::OperationObserver;
use tokio::net::TcpListener;

use support::journal_fake::{direct_credential, self_signed};

/// Shared observation is inert and counts the physical shared-client dials.
#[tokio::test]
async fn observation_is_inert() {
    async fn run_scenario(observer: Option<Arc<OperationObserver>>) -> (String, u64) {
        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed); // nothing is listening on this port now

        let (cert, _key) = self_signed();
        let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
        let client = ObserverClient::new(direct_credential(pin, port))
            .unwrap()
            .with_observer(observer.clone());
        let error = client.list_segments("20260729").await.unwrap_err();
        assert!(matches!(
            error,
            pl_transport_win::TransportError::Io(_) | pl_transport_win::TransportError::Tls(_)
        ));
        (
            pl_transport_win::transport_error_code(&error),
            observer.map(|o| o.snapshot().dial_attempts).unwrap_or(0),
        )
    }

    let (observed_error, observed_dials) = run_scenario(Some(OperationObserver::new())).await;
    let (unobserved_error, unobserved_dials) = run_scenario(None).await;

    assert_eq!(observed_error, unobserved_error);
    assert_eq!(unobserved_dials, 0, "an absent observer has no state");
    assert!(observed_dials > 0, "shared observer counts physical dials");
}
