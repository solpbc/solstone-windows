// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Proof that the transport's observation seam changes nothing it observes.
//!
//! This is a test binary of its own on purpose: the capturing subscriber is
//! installed process-wide, so a sibling test emitting `pl_transport` events would
//! contaminate the captured sequence this test compares.

mod support;

use std::sync::Arc;

use pl_transport_win::client::ObserverClient;
use pl_transport_win::observe::OperationObserver;
use tokio::net::TcpListener;

use support::journal_fake::{direct_credential, self_signed};
use support::log_capture::CapturingSubscriber;

/// D4's inertness requirement: the same scenario, once observed and once not,
/// must produce an identical dial/retry event sequence.
#[tokio::test]
async fn observation_is_inert() {
    let subscriber = CapturingSubscriber::for_target("pl_transport");
    subscriber.install();

    // Two endpoints that both refuse, so the run exhausts the full retry ladder
    // and emits dial-start / transient-retry / dial-failed events.
    async fn run_scenario(observer: Option<Arc<OperationObserver>>) -> u32 {
        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed); // nothing is listening on this port now

        let (cert, _key) = self_signed();
        let pin = observer_pl::ca::sha256(cert.as_ref())[..16].to_vec();
        let client = ObserverClient::new(direct_credential(pin, port))
            .unwrap()
            .with_observer(observer.clone());
        let error = client.list_segments("20260729").await.unwrap_err();
        assert!(matches!(
            error,
            pl_transport_win::TransportError::Io(_) | pl_transport_win::TransportError::Tls(_)
        ));
        observer
            .map(|o| o.counts().dial_attempts as u32)
            .unwrap_or(0)
    }

    let _ = subscriber.take();
    let observed_dials = run_scenario(Some(OperationObserver::new())).await;
    let observed = subscriber.take();

    let unobserved_dials = run_scenario(None).await;
    let unobserved = subscriber.take();

    assert_eq!(unobserved_dials, 0, "an absent handle counts nothing");
    assert!(observed_dials > 0, "an attached handle counts the dials");

    // The event sequences must be identical: same attempts, same retries, same
    // ordering, whether or not anyone is watching.
    let strip = |lines: Vec<String>| -> Vec<String> {
        lines
            .into_iter()
            // duration_ms is wall-clock and legitimately varies between runs.
            .map(|line| {
                line.split(' ')
                    .filter(|field| !field.starts_with("duration_ms="))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    };
    let observed = strip(observed);
    let unobserved = strip(unobserved);
    assert!(!observed.is_empty(), "the scenario must emit dial events");
    assert_eq!(
        observed, unobserved,
        "observation changed the dial/retry sequence"
    );

    // And the counter agreed with the event stream it must not have perturbed.
    let dial_starts = observed
        .iter()
        .filter(|line| line.contains("dial start"))
        .count() as u32;
    assert_eq!(
        observed_dials, dial_starts,
        "the counted dials must match the dials the transport actually logged"
    );
}
