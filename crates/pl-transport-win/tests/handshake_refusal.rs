// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! A journal's refusal of this device, delivered after the TLS 1.3 handshake,
//! reaches the owner the same way whichever path saw it: the sync client stops
//! dialing and the pairing reads as failed with the refused detail. A refusal
//! that means "try again" never stops anything.

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use observer_model::{LocalOffset, LocalOffsetError, PairingPhase, SyncSnapshot};
use pl_transport_win::client::ObserverClient;
use pl_transport_win::coordinator::PAIRING_REFUSED_DETAIL;
use pl_transport_win::credential::PairedState;
use pl_transport_win::service::SyncConfig;
use pl_transport_win::CredentialAccess;
use rustls::CertificateError;
use spl_transport::handshake::HandshakeStop;
use support::journal_fake::{direct_credential, refusing_server_config, self_signed};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

static TEST_PATH_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A journal that refuses every client certificate with `error`, counting the
/// connections it receives. Like the journal's own listener, it lingers after
/// refusing so a client that is still writing reads the refusal instead of a
/// reset (Windows discards unread data when a reset arrives).
async fn refusing_journal(
    error: CertificateError,
) -> (Vec<u8>, u16, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let (cert, key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let acceptor = TlsAcceptor::from(Arc::new(refusing_server_config(cert, key, error)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = accepted.clone();
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            counter.fetch_add(1, Ordering::SeqCst);
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Err((_, mut io)) = acceptor.accept(stream).into_fallible().await {
                    let _ = io.shutdown().await;
                    let mut discard = [0_u8; 4096];
                    let _ = tokio::time::timeout(Duration::from_secs(2), async {
                        while matches!(io.read(&mut discard).await, Ok(count) if count > 0) {}
                    })
                    .await;
                }
            });
        }
    });
    (pin, port, accepted, task)
}

fn other_refusal() -> CertificateError {
    CertificateError::Other(rustls::OtherError(Arc::new(std::io::Error::other(
        "authorization unreadable",
    ))))
}

// SPL session § 7. Falsified by not feeding request outcomes to the refusal rule: the client
// keeps dialing a journal that said this device is not paired.
#[tokio::test]
async fn access_denied_stops_the_sync_client_and_certificate_unknown_does_not() {
    let (pin, port, accepted, journal) =
        refusing_journal(CertificateError::ApplicationVerificationFailure).await;
    let client = ObserverClient::new(direct_credential(pin, port)).unwrap();

    assert!(client.ingest_manifest().await.is_err());
    assert_eq!(client.refusal_stop(), Some(HandshakeStop::TlsAccessDenied));
    let dials = accepted.load(Ordering::SeqCst);
    assert!(client.ingest_manifest().await.is_err());
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        dials,
        "a stopped pairing must not dial"
    );
    journal.abort();

    let (pin, port, accepted, journal) = refusing_journal(other_refusal()).await;
    let client = ObserverClient::new(direct_credential(pin, port)).unwrap();
    assert!(client.ingest_manifest().await.is_err());
    assert_eq!(client.refusal_stop(), None);
    let dials = accepted.load(Ordering::SeqCst);
    assert!(client.ingest_manifest().await.is_err());
    assert!(
        accepted.load(Ordering::SeqCst) > dials,
        "certificate unknown keeps retrying"
    );
    journal.abort();

    // Any other refusal counts toward the bound but does not stop at once.
    let (pin, port, _, journal) = refusing_journal(CertificateError::UnknownIssuer).await;
    let client = ObserverClient::new(direct_credential(pin, port)).unwrap();
    assert!(client.ingest_manifest().await.is_err());
    assert_eq!(client.refusal_stop(), None);
    journal.abort();
}

#[derive(Debug)]
struct TestOffset;

impl LocalOffset for TestOffset {
    fn local_offset_secs(&self, _epoch_secs: u64) -> Result<i64, LocalOffsetError> {
        Ok(0)
    }
}

async fn open_journal_request(handle: &pl_transport_win::journal_bridge::JournalBridgeHandle) {
    let bootstrap = handle.bootstrap_url();
    let cap = bootstrap
        .split_once("cap=")
        .map(|(_, cap)| cap.to_string())
        .expect("bootstrap capability");
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", handle.port()))
        .await
        .expect("bridge connect");
    let request = format!(
        "GET / HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: {}={}\r\n\r\n",
        handle.port(),
        observer_pl::CAP_COOKIE_NAME,
        cap
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response).await;
}

// Falsified by reading only carrier liveness from the bridge: the "open journal" window hits a
// refusal and the pairing still reads as paired.
#[tokio::test]
async fn a_bridge_the_journal_refused_marks_the_pairing_refused() {
    let (pin, port, _, journal) =
        refusing_journal(CertificateError::ApplicationVerificationFailure).await;
    let paired = PairedState {
        credential: Some(direct_credential(pin, port)),
        ..Default::default()
    };
    let unique = TEST_PATH_COUNTER.fetch_add(1, Ordering::SeqCst);
    let state_path = std::path::PathBuf::from("/var/tmp").join(format!(
        "handshake-refusal-{}-{unique}.json",
        std::process::id()
    ));
    let journal_version = Arc::new(pl_transport_win::JournalVersionController::new(
        state_path.with_file_name(format!("handshake-refusal-jv-{unique}.json")),
    ));
    let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
    let cfg = SyncConfig {
        device_label: "handshake-refusal-test".into(),
        period_secs: 300,
        segments_root: state_path.with_extension("segments"),
        state_path,
        local_offset: Arc::new(TestOffset),
        journal_version,
        facts_fn: Arc::new(pl_transport_win::RawDeviceFacts::default),
    };
    let access = CredentialAccess::bind(&paired, &cfg, sync.clone(), None).expect("access bind");
    let handle = pl_transport_win::journal_bridge::start(access)
        .await
        .expect("bridge start");

    open_journal_request(&handle).await;
    let mut refused = false;
    for _ in 0..2000 {
        let pairing = sync.lock().unwrap().pairing.clone();
        if pairing.phase == PairingPhase::Failed
            && pairing.detail.as_deref() == Some(PAIRING_REFUSED_DETAIL)
        {
            refused = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(refused, "the pairing must read as refused");
    handle.shutdown_and_wait().await;
    journal.abort();
}
