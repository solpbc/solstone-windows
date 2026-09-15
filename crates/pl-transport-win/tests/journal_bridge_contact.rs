// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use std::sync::{Arc, Mutex, RwLock};

use observer_model::{LocalOffset, LocalOffsetError, SyncSnapshot};
use observer_retention::RetentionConfig;
use pl_transport_win::credential::{Credential, EndpointAddr, PairedState};
use pl_transport_win::service::SyncConfig;
use pl_transport_win::CredentialAccess;
use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};
use spl_core::bridge::BridgeNames;
use spl_transport::journal_bridge::{
    self as shared_bridge, BridgePolicy, CarrierOpener, JournalBridgeConfig,
};

static TEST_PATH_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn paired_state() -> PairedState {
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
    let params = CertificateParams::new(vec!["observer.test".to_string()]).unwrap();
    let cert = params.self_signed(&key).unwrap();
    let credential = Credential {
        client_key_pem: key.serialize_pem(),
        client_cert_pem: cert.pem(),
        ca_chain_pem: vec![cert.pem()],
        ca_fp_prefix: vec![0u8; 16],
        instance_id: "test-instance".into(),
        home_label: "Home".into(),
        endpoints: vec![EndpointAddr {
            host: "127.0.0.1".into(),
            port: 1,
        }],
        relay_origin: None,
        device_token: None,
        device_token_expires_at: None,
    };
    PairedState {
        credential: Some(credential),
        ..Default::default()
    }
}

#[derive(Debug)]
struct TestOffset;

impl LocalOffset for TestOffset {
    fn local_offset_secs(&self, _epoch_secs: u64) -> Result<i64, LocalOffsetError> {
        Ok(0)
    }
}

async fn start_windows_bridge(
    name: &str,
) -> (
    pl_transport_win::journal_bridge::JournalBridgeHandle,
    Arc<pl_transport_win::JournalVersionController>,
    Arc<Mutex<SyncSnapshot>>,
) {
    let paired = paired_state();
    let unique = TEST_PATH_COUNTER.fetch_add(1, Ordering::SeqCst);
    let state_path = std::path::PathBuf::from("/var/tmp").join(format!(
        "journal-bridge-contact-{name}-{}-{unique}.json",
        std::process::id()
    ));
    let jv_path = state_path.with_file_name("journal-version.json");
    let jv = Arc::new(pl_transport_win::JournalVersionController::new(jv_path));
    let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
    let cfg = SyncConfig {
        device_label: "bridge-contact-test".into(),
        period_secs: 300,
        segments_root: state_path.with_extension("segments"),
        state_path,
        retention: Arc::new(RwLock::new(RetentionConfig::default())),
        local_offset: Arc::new(TestOffset),
        journal_version: jv.clone(),
        facts_fn: Arc::new(pl_transport_win::RawDeviceFacts::default),
    };
    let access = CredentialAccess::bind(&paired, &cfg, sync.clone(), None).expect("access bind");
    let handle = pl_transport_win::journal_bridge::start(access)
        .await
        .expect("bridge start");
    (handle, jv, sync)
}

#[tokio::test]
async fn contacted_flips_on_first_accept_before_http_parse() {
    let (handle, _, _) = start_windows_bridge("contacted").await;
    // Flag starts false before any connection.
    assert!(!handle.contacted(), "flag must start false");

    let port = handle.port();
    // Bare TCP connection that sends NO parseable HTTP request. The flag must
    // still flip, proving the seam is at accept (not after HTTP parse).
    let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect to bridge");

    // accept + flag store happen in the spawned accept_loop; bounded poll.
    let mut flipped = false;
    for _ in 0..200 {
        if handle.contacted() {
            flipped = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        flipped,
        "contacted() must flip true on the first accepted TCP connection"
    );

    drop(stream);
    handle.begin_shutdown();
}

#[tokio::test]
async fn status_task_ignores_contact_and_active_request_noise() {
    let (handle, journal_version, sync) = start_windows_bridge("noise").await;
    let token = journal_version.current_token();
    journal_version.publish_journal_metadata(None, Some("test"), token, &sync);
    assert!(sync.lock().unwrap().journal_version_fresh);

    let stream = tokio::net::TcpStream::connect(("127.0.0.1", handle.port()))
        .await
        .expect("connect to bridge");
    drop(stream);
    tokio::time::sleep(Duration::from_millis(25)).await;

    assert!(
        sync.lock().unwrap().journal_version_fresh,
        "contacted and active-request transitions must not be treated as carrier loss"
    );
    handle.shutdown_and_wait().await;
}

struct NoDialOpener {
    opens: AtomicUsize,
}

impl CarrierOpener for NoDialOpener {
    fn proxy_headers(
        &self,
        upstream_headers: &[(String, String)],
    ) -> Result<Vec<(String, String)>, spl_transport::TransportError> {
        Ok(upstream_headers.to_vec())
    }

    fn dial_carrier(
        &self,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<spl_transport::DialedCarrier, spl_transport::TransportError>>
                + Send
                + '_,
        >,
    > {
        self.opens.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(spl_transport::TransportError::NoEndpoint) })
    }
}

async fn start_shared_bridge(opener: Arc<NoDialOpener>) -> shared_bridge::JournalBridgeHandle {
    shared_bridge::start(JournalBridgeConfig {
        opener,
        bridge_names: BridgeNames {
            capability_cookie_name: observer_pl::CAP_COOKIE_NAME.into(),
            upstream_cookie_prefix: observer_pl::UPSTREAM_COOKIE_PREFIX.into(),
            observer_header_name: observer_pl::OBSERVER_HANDLE_HEADER.to_ascii_lowercase(),
            protocol_version_header_name: observer_pl::PROTOCOL_VERSION_HEADER.to_ascii_lowercase(),
        },
        endpoint_hosts: Vec::new(),
        policy: BridgePolicy::default(),
    })
    .await
    .expect("shared bridge start")
}

#[tokio::test]
async fn status_task_lag_marks_unknown_then_reconciles_snapshot() {
    let opener = Arc::new(NoDialOpener {
        opens: AtomicUsize::new(0),
    });
    let handle = start_shared_bridge(opener).await;
    let mut subscription = handle.subscribe_status();

    let mut streams = Vec::new();
    for _ in 0..80 {
        streams.push(
            tokio::net::TcpStream::connect(("127.0.0.1", handle.port()))
                .await
                .expect("connect to bridge"),
        );
    }
    drop(streams);
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(
        matches!(
            subscription.recv().await,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_))
        ),
        "the bounded status stream must report a lag instead of silently dropping a carrier edge"
    );
    let snapshot = handle.status();
    assert!(!snapshot.carrier_live);
    assert!(snapshot.contacted);
    handle.shutdown_and_wait().await;
}

#[tokio::test]
async fn bridge_shutdown_stops_subscription_and_marks_sessions_disconnected() {
    let (handle, journal_version, sync) = start_windows_bridge("shutdown").await;
    let token = journal_version.current_token();
    journal_version.publish_journal_metadata(None, Some("test"), token, &sync);
    assert!(sync.lock().unwrap().journal_version_fresh);

    handle.shutdown_and_wait().await;
    assert!(
        !sync.lock().unwrap().journal_version_fresh,
        "shutdown must retire the status task and mark its journal session disconnected"
    );
}
