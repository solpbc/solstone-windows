// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Integration tests for the journal-mark confirmation gate.

mod support;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use observer_model::{
    LocalOffset, LocalOffsetError, LocalZone, PairingPhase, SyncSnapshot, MARK_REJECTED_DETAIL,
};
use observer_pl::ingest::FilePart;
use pl_transport_win::ack::JournalIdentity;
use pl_transport_win::answer::{
    answer_path, read_answer, settle_grandfather, settle_rejected_on_launch, write_answer,
    AnswerState,
};
use pl_transport_win::client::{ObserverClient, RouteError};
#[cfg(feature = "awaiting-hold")]
use pl_transport_win::coordinator::AwaitingHold;
use pl_transport_win::coordinator::UploadCoordinator;
use pl_transport_win::credential::{CasKey, PairedState};
use pl_transport_win::sealed::{LocalSealedStore, SealedStore};
use pl_transport_win::service::{
    publish_pairing, run_uploader, BoundKind, PairingWrite, SyncConfig,
};
use pl_transport_win::session::{answer, PairingAction};
use pl_transport_win::slot::UploaderSlot;
use pl_transport_win::unknown_journals::mark_spec_for_jid;
use pl_transport_win::{CredentialAccess, JournalVersionController, DEFAULT_UPLOAD_INTERVAL_SECS};
use spl_core::frame::{Frame, FrameDecoder, FLAG_CLOSE, FLAG_DATA, FLAG_WINDOW, RECOMMENDED_CHUNK};
use spl_core::mux::INITIAL_WINDOW;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use support::journal_fake::{direct_credential, read_framed_request, self_signed, server_config};
use support::log_capture::CapturingSubscriber;

static TEST_PATH_COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let unique = TEST_PATH_COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "mark-confirmation-{name}-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug)]
struct UtcOffset;

impl LocalOffset for UtcOffset {
    fn local_zone(&self, _epoch_secs: u64) -> Result<LocalZone, LocalOffsetError> {
        Ok(LocalZone {
            utc_offset_seconds: 0,
            tz: None,
        })
    }
}

fn test_file_parts() -> Vec<FilePart> {
    vec![FilePart {
        filename: "screen.mp4".into(),
        content_type: "video/mp4".into(),
        bytes: b"video-frame-bytes".to_vec(),
    }]
}

async fn write_response(
    tls: &mut tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    stream_id: u32,
    response: Vec<u8>,
) {
    let chunks = response.chunks(RECOMMENDED_CHUNK / 4).collect::<Vec<_>>();
    let mut credit = INITIAL_WINDOW as i64;
    let mut decoder = FrameDecoder::new();
    let mut read_buf = [0u8; 4096];
    for (index, chunk) in chunks.iter().enumerate() {
        while credit < chunk.len() as i64 {
            let read = tls.read(&mut read_buf).await.unwrap();
            assert!(read > 0, "client closed before granting response credit");
            decoder.feed(&read_buf[..read]);
            for frame in decoder.drain().unwrap() {
                if frame.stream_id == stream_id
                    && frame.flags & FLAG_WINDOW != 0
                    && frame.payload.len() == 4
                {
                    credit += i64::from(u32::from_be_bytes(frame.payload[..4].try_into().unwrap()));
                }
            }
        }
        let flags = FLAG_DATA | ((index + 1 == chunks.len()) as u8 * FLAG_CLOSE);
        let frame = Frame::new(stream_id, flags, chunk.to_vec());
        tls.write_all(&frame.encode().unwrap()).await.unwrap();
        credit -= chunk.len() as i64;
    }
    tls.flush().await.unwrap();
    let mut drain = [0u8; 1024];
    while tls.read(&mut drain).await.unwrap_or(0) != 0 {}
}

/// Gated routes do not dial while awaiting confirmation.
/// Confirmed client dials.
/// With journal A confirmed, a second client B whose cell was bound while cache has A
/// leaves B's ingest refused / closed.
#[tokio::test]
async fn mark_confirmation_gated_routes_do_not_dial() {
    let (cert, key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    let accepts = Arc::new(AtomicUsize::new(0));

    let accepts_clone = accepts.clone();
    let server_task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            accepts_clone.fetch_add(1, Ordering::SeqCst);
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Ok(mut tls) = acceptor.accept(stream).await {
                    let (stream_id, _) = read_framed_request(&mut tls).await;
                    let resp = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 15\r\n\r\n{\"status\":\"ok\"}"
                        .to_vec();
                    write_response(&mut tls, stream_id, resp).await;
                }
            });
        }
    });

    let cred_a = direct_credential(pin.clone(), port);
    let gate_a = Arc::new(AtomicBool::new(false));
    let client_a = ObserverClient::new(cred_a.clone(), gate_a.clone()).unwrap();

    // 1. Calling gated routes while gate is closed produces 0 dials and returns AwaitingConfirmation
    let res = client_a
        .ingest("000000_300", "20260930", test_file_parts(), None)
        .await;
    assert!(matches!(res, Err(RouteError::AwaitingConfirmation)));

    let res = client_a.list_segments("20260930").await;
    assert!(matches!(res, Err(RouteError::AwaitingConfirmation)));

    let res = client_a.ingest_manifest().await;
    assert!(matches!(res, Err(RouteError::AwaitingConfirmation)));

    let res = client_a.ingest_manifest_day("20260930").await;
    assert!(matches!(res, Err(RouteError::AwaitingConfirmation)));

    assert_eq!(accepts.load(Ordering::SeqCst), 0);

    // 2. Confirmed client dials
    gate_a.store(true, Ordering::SeqCst);
    let res = client_a
        .ingest("000000_300", "20260930", test_file_parts(), None)
        .await;
    assert!(res.is_ok());
    assert!(accepts.load(Ordering::SeqCst) > 0);

    // 3. With journal A confirmed in confirmation cache, binding client B with cert B
    // leaves client B's gate closed.
    let dir = TempDir::new("gated-cache");
    let state_path = dir.path().join("pairing.json");
    let digest_a = JournalIdentity::from_credential(&cred_a).client_cert_sha256;
    let confirmation = Arc::new(Mutex::new(digest_a));
    let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
    let jv = Arc::new(JournalVersionController::new(dir.path().join("jv.json")));
    let cfg = SyncConfig {
        device_label: "test".into(),
        period_secs: 300,
        segments_root: dir.path().join("segments"),
        state_path,
        local_offset: Arc::new(UtcOffset),
        journal_version: jv,
        facts_fn: Arc::new(pl_transport_win::RawDeviceFacts::default),
        confirmation: confirmation.clone(),
        tombstone: Arc::new(Mutex::new(None)),
        #[cfg(feature = "awaiting-hold")]
        awaiting_hold: None,
    };

    let cred_b = direct_credential(pin, port);
    let paired_b = PairedState {
        credential: Some(cred_b.clone()),
        ..Default::default()
    };
    let access_b = CredentialAccess::bind(&paired_b, &cfg, sync.clone(), None).unwrap();
    let client_b = access_b.client_slot().load();
    assert!(!client_b.gate_open());
    let res = client_b
        .ingest("000000_300", "20260930", test_file_parts(), None)
        .await;
    assert!(matches!(res, Err(RouteError::AwaitingConfirmation)));

    server_task.abort();
}

/// Rebuilding access keeps the closed gate until opened.
#[tokio::test]
async fn mark_confirmation_rebuild_keeps_the_closed_gate() {
    let (cert, _key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let dir = TempDir::new("rebuild-gate");
    let state_path = dir.path().join("pairing.json");
    let confirmation = Arc::new(Mutex::new(String::new()));
    let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
    let jv = Arc::new(JournalVersionController::new(dir.path().join("jv.json")));
    let cfg = SyncConfig {
        device_label: "test".into(),
        period_secs: 300,
        segments_root: dir.path().join("segments"),
        state_path,
        local_offset: Arc::new(UtcOffset),
        journal_version: jv,
        facts_fn: Arc::new(pl_transport_win::RawDeviceFacts::default),
        confirmation: confirmation.clone(),
        tombstone: Arc::new(Mutex::new(None)),
        #[cfg(feature = "awaiting-hold")]
        awaiting_hold: None,
    };

    let cred = direct_credential(pin, 1);
    let paired = PairedState {
        credential: Some(cred.clone()),
        ..Default::default()
    };
    let access = CredentialAccess::bind(&paired, &cfg, sync.clone(), None).unwrap();
    assert!(!access.client_slot().load().gate_open());

    // Replace from incumbent
    let mut cred2 = cred.clone();
    cred2.home_label = "Home Rebuilt".into();
    let client_slot = access.client_slot();
    let client2 = client_slot
        .replace_from_incumbent(
            cred2,
            CasKey {
                pairing_generation: 0,
                access_mutation_generation: 0,
            },
        )
        .unwrap();
    assert!(!client2.gate_open());

    let res = client2
        .ingest("000000_300", "20260930", test_file_parts(), None)
        .await;
    assert!(matches!(res, Err(RouteError::AwaitingConfirmation)));

    // Open gate
    client_slot.load().open_gate();
    assert!(client_slot.load().gate_open());
}

/// An awaiting tick does not count as a failure.
#[tokio::test(start_paused = true)]
async fn mark_confirmation_awaiting_tick_is_not_a_failure() {
    let (cert, _key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let dir = TempDir::new("awaiting-tick");
    let segments_dir = dir.path().join("segments");
    std::fs::create_dir_all(&segments_dir).unwrap();

    let state_path = dir.path().join("pairing.json");
    let confirmation = Arc::new(Mutex::new(String::new()));
    let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
    let jv = Arc::new(JournalVersionController::new(dir.path().join("jv.json")));
    let cfg = SyncConfig {
        device_label: "test".into(),
        period_secs: 300,
        segments_root: segments_dir.clone(),
        state_path,
        local_offset: Arc::new(UtcOffset),
        journal_version: jv.clone(),
        facts_fn: Arc::new(pl_transport_win::RawDeviceFacts::default),
        confirmation: confirmation.clone(),
        tombstone: Arc::new(Mutex::new(None)),
        #[cfg(feature = "awaiting-hold")]
        awaiting_hold: None,
    };

    let cred = direct_credential(pin, 1);
    let paired = PairedState {
        credential: Some(cred),
        ..Default::default()
    };
    let access = CredentialAccess::bind(&paired, &cfg, sync.clone(), None).unwrap();
    let client_slot = access.client_slot();
    let post_connect = access.post_connect();

    post_connect.trigger();

    let store: Box<dyn SealedStore> =
        Box::new(LocalSealedStore::new(&cfg.segments_root, cfg.period_secs));
    let coordinator = Arc::new(UploadCoordinator::new_with_slot(
        client_slot,
        store,
        sync.clone(),
        cfg.period_secs,
        cfg.local_offset.clone(),
        cfg.journal_version.clone(),
        Some(post_connect.clone()),
        access.journal_version_token(),
        Some(access.post_connect_token()),
        cfg.confirmation.clone(),
        cfg.tombstone.clone(),
    ));

    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(pl_transport_win::SlotExit::Run);
    let wake = Arc::new(tokio::sync::Notify::new());
    let coord = coordinator.clone();
    let uploader_task = tokio::spawn(async move { coord.run(cancel_rx, wake).await });

    for _ in 0..10 {
        tokio::task::yield_now().await;
    }

    let subscriber = CapturingSubscriber::for_target("pl_upload");
    subscriber.install();
    let _ = subscriber.take();

    let before_last_sync = sync.lock().unwrap().upload.last_successful_sync;
    let before_seg_count = coordinator.segment_bound_count();
    let before_day_count = coordinator.day_bound_count();
    let before_last_connected = post_connect.last_connected_epoch();

    tokio::time::advance(Duration::from_secs(DEFAULT_UPLOAD_INTERVAL_SECS + 1)).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }

    assert_eq!(
        sync.lock().unwrap().upload.last_successful_sync,
        before_last_sync
    );
    assert_eq!(coordinator.segment_bound_count(), before_seg_count);
    assert_eq!(coordinator.day_bound_count(), before_day_count);
    assert_eq!(post_connect.last_connected_epoch(), before_last_connected);

    let lines = subscriber.take();
    assert!(!lines.iter().any(|line| line.contains("upload event")));

    let _ = cancel_tx.send(pl_transport_win::SlotExit::Shutdown);
    let _ = uploader_task.await;
}

/// Kicking the slot wakes a 300s backoff immediately without advancing the clock.
#[tokio::test(start_paused = true)]
async fn mark_confirmation_kick_wakes_a_300s_backoff() {
    let (cert, _key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();

    // Use a closed port so every dial returns RouteError::Transport
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let dir = TempDir::new("kick-backoff");
    let segments_dir = dir.path().join("segments");
    let seg_dir = segments_dir.join("1");
    std::fs::create_dir_all(&seg_dir).unwrap();
    std::fs::write(seg_dir.join("screen.mp4"), b"dummy-data").unwrap();

    let state_path = dir.path().join("pairing.json");
    let cred = direct_credential(pin, port);
    let binding = JournalIdentity::from_credential(&cred).client_cert_sha256;
    let confirmation = Arc::new(Mutex::new(binding.clone())); // Gate open
    let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
    let jv = Arc::new(JournalVersionController::new(dir.path().join("jv.json")));
    let cfg = SyncConfig {
        device_label: "test".into(),
        period_secs: 300,
        segments_root: segments_dir,
        state_path: state_path.clone(),
        local_offset: Arc::new(UtcOffset),
        journal_version: jv,
        facts_fn: Arc::new(pl_transport_win::RawDeviceFacts::default),
        confirmation: confirmation.clone(),
        tombstone: Arc::new(Mutex::new(None)),
        #[cfg(feature = "awaiting-hold")]
        awaiting_hold: None,
    };

    let paired = PairedState {
        credential: Some(cred),
        ..Default::default()
    };
    paired.save(&state_path).unwrap();

    let slot = Arc::new(tokio::sync::Mutex::new(UploaderSlot::new()));
    let access = CredentialAccess::bind(&paired, &cfg, sync.clone(), None).unwrap();

    let wake = slot.lock().await.wake();
    let cfg_clone = cfg.clone();
    let sync_clone = sync.clone();
    slot.lock()
        .await
        .replace(move |rx| run_uploader(access, cfg_clone, sync_clone, rx, wake))
        .await;

    async fn wait_for_recent_errors(sync: &Arc<Mutex<SyncSnapshot>>, target: u32) {
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            if u32::from(sync.lock().unwrap().upload.recent_error_count) >= target {
                return;
            }
            tokio::time::advance(Duration::from_millis(10)).await;
            tokio::task::yield_now().await;
        }
        let count = u32::from(sync.lock().unwrap().upload.recent_error_count);
        panic!(
            "tick timed out waiting for recent_error_count >= {}, current = {}",
            target, count
        );
    }

    // Drive kicks: 5->10->20->40->80->160->300 (total 7 kicks + 1 initial = 8 errors)
    for target in 1..=8 {
        slot.lock().await.kick();
        wait_for_recent_errors(&sync, target).await;
    }

    assert_eq!(sync.lock().unwrap().upload.recent_error_count, 8);
}

/// Kicking under a paused clock wakes the loop and completes upload.
#[cfg(feature = "awaiting-hold")]
#[tokio::test]
async fn mark_confirmation_kick_under_a_paused_clock() {
    let (cert, key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    let upload_received = Arc::new(AtomicBool::new(false));

    let upload_flag = upload_received.clone();
    let server_task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            let upload_flag = upload_flag.clone();
            tokio::spawn(async move {
                if let Ok(mut tls) = acceptor.accept(stream).await {
                    let (stream_id, req) = read_framed_request(&mut tls).await;
                    let req_str = String::from_utf8_lossy(&req);
                    if req_str.contains("/app/devices/ingest") {
                        upload_flag.store(true, Ordering::SeqCst);
                    }
                    let resp = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 38\r\n\r\n{\"status\":\"ok\",\"segment\":\"000000_300\"}"
                        .to_vec();
                    write_response(&mut tls, stream_id, resp).await;
                }
            });
        }
    });

    let dir = TempDir::new("paused-clock");
    let segments_dir = dir.path().join("segments");
    let day_dir = segments_dir.join("20260930");
    std::fs::create_dir_all(&day_dir).unwrap();
    std::fs::write(day_dir.join("000000_300.tar.gz"), b"dummy-data").unwrap();

    let state_path = dir.path().join("pairing.json");
    let confirmation = Arc::new(Mutex::new(String::new()));
    let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
    let jv = Arc::new(JournalVersionController::new(dir.path().join("jv.json")));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let awaiting_hold = Some(AwaitingHold {
        entered: entered.clone(),
        release: release.clone(),
    });

    let cfg = SyncConfig {
        device_label: "test".into(),
        period_secs: 300,
        segments_root: segments_dir,
        state_path: state_path.clone(),
        local_offset: Arc::new(UtcOffset),
        journal_version: jv,
        facts_fn: Arc::new(pl_transport_win::RawDeviceFacts::default),
        confirmation: confirmation.clone(),
        tombstone: Arc::new(Mutex::new(None)),
        awaiting_hold,
    };

    let cred = direct_credential(pin, port);
    let binding = JournalIdentity::from_credential(&cred).client_cert_sha256;
    let paired = PairedState {
        credential: Some(cred),
        ..Default::default()
    };
    paired.save(&state_path).unwrap();

    let slot = Arc::new(tokio::sync::Mutex::new(UploaderSlot::new()));
    let access = CredentialAccess::bind(&paired, &cfg, sync.clone(), None).unwrap();
    let access_mutex = Arc::new(tokio::sync::Mutex::new(Some(access.clone())));

    let wake = slot.lock().await.wake();
    let cfg_clone = cfg.clone();
    let sync_clone = sync.clone();
    slot.lock()
        .await
        .replace(move |rx| run_uploader(access, cfg_clone, sync_clone, rx, wake))
        .await;

    slot.lock().await.kick();

    // Wait until coordinator tick enters the awaiting hold
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("entered awaiting hold");

    // Confirm while awaiting
    answer(
        PairingAction::Confirm,
        &binding,
        &cfg,
        &sync,
        &access_mutex,
        &slot,
    )
    .await
    .expect("answer confirm");

    // Release the hold
    release.notify_one();

    // Verify upload occurs
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(upload_received.load(Ordering::SeqCst));

    server_task.abort();
}

/// Reject deletes the DER ID and cleans up state.
#[tokio::test]
async fn mark_confirmation_reject_deletes_the_der_id() {
    let (cert, key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    let delete_received = Arc::new(Mutex::new(Vec::new()));
    let slot = Arc::new(tokio::sync::Mutex::new(UploaderSlot::new()));
    let lock_held_during_delete = Arc::new(AtomicBool::new(false));

    let cred = direct_credential(pin, port);
    let der_certs = spl_transport::tls::parse_certs(&cred.client_cert_pem).unwrap();
    let expected_client_id = format!("sha256:{}", spl_core::ca::sha256_hex(der_certs[0].as_ref()));

    let delete_rec = delete_received.clone();
    let slot_clone = slot.clone();
    let lock_held = lock_held_during_delete.clone();
    let expected_id = expected_client_id.clone();

    let server_task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            let delete_rec = delete_rec.clone();
            let slot_clone = slot_clone.clone();
            let lock_held = lock_held.clone();
            let expected_id = expected_id.clone();
            tokio::spawn(async move {
                if let Ok(mut tls) = acceptor.accept(stream).await {
                    let (stream_id, req) = read_framed_request(&mut tls).await;
                    let req_str = String::from_utf8_lossy(&req);
                    if req_str.contains("/app/network/api/clients/") {
                        if slot_clone.try_lock().is_err() {
                            lock_held.store(true, Ordering::SeqCst);
                        }
                        delete_rec.lock().unwrap().push(req_str.to_string());
                        if req_str.contains(&expected_id) {
                            let resp = b"HTTP/1.1 204 No Content\r\n\r\n".to_vec();
                            write_response(&mut tls, stream_id, resp).await;
                        } else {
                            let resp = b"HTTP/1.1 404 Not Found\r\n\r\n".to_vec();
                            write_response(&mut tls, stream_id, resp).await;
                        }
                    }
                }
            });
        }
    });

    let dir = TempDir::new("reject-delete");
    let segments_dir = dir.path().join("segments");
    let day_dir = segments_dir.join("20260930");
    std::fs::create_dir_all(&day_dir).unwrap();
    std::fs::write(day_dir.join("000000_300.tar.gz"), b"dummy-data").unwrap();

    let state_path = dir.path().join("pairing.json");
    let confirmation = Arc::new(Mutex::new(String::new()));
    let tombstone = Arc::new(Mutex::new(None));
    let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
    let jv = Arc::new(JournalVersionController::new(dir.path().join("jv.json")));
    let cfg = SyncConfig {
        device_label: "test".into(),
        period_secs: 300,
        segments_root: segments_dir.clone(),
        state_path: state_path.clone(),
        local_offset: Arc::new(UtcOffset),
        journal_version: jv,
        facts_fn: Arc::new(pl_transport_win::RawDeviceFacts::default),
        confirmation: confirmation.clone(),
        tombstone: tombstone.clone(),
        #[cfg(feature = "awaiting-hold")]
        awaiting_hold: None,
    };

    let binding = JournalIdentity::from_credential(&cred).client_cert_sha256;
    let paired = PairedState {
        credential: Some(cred.clone()),
        ..Default::default()
    };
    paired.save(&state_path).unwrap();

    let access = CredentialAccess::bind(&paired, &cfg, sync.clone(), None).unwrap();
    let access_mutex = Arc::new(tokio::sync::Mutex::new(Some(access.clone())));

    let wake = slot.lock().await.wake();
    let cfg_clone = cfg.clone();
    let sync_clone = sync.clone();
    slot.lock()
        .await
        .replace(move |rx| run_uploader(access, cfg_clone, sync_clone, rx, wake))
        .await;

    // Set active pairing in snapshot
    {
        let mut snap = sync.lock().unwrap();
        snap.pairing.binding = binding.clone();
        snap.pairing.phase = PairingPhase::AwaitingConfirmation;
    }

    // Perform reject
    answer(
        PairingAction::Reject,
        &binding,
        &cfg,
        &sync,
        &access_mutex,
        &slot,
    )
    .await
    .expect("answer reject");

    assert!(lock_held_during_delete.load(Ordering::SeqCst));
    assert_eq!(delete_received.lock().unwrap().len(), 1);
    let delete_req = &delete_received.lock().unwrap()[0];
    assert!(delete_req.contains(&expected_client_id));

    // pairing.json deleted, segments remain, phase is NotPaired(MARK_REJECTED_DETAIL)
    assert!(!state_path.exists());
    assert!(day_dir.join("000000_300.tar.gz").exists());
    {
        let snap = sync.lock().unwrap();
        assert_eq!(snap.pairing.phase, PairingPhase::NotPaired);
        assert_eq!(snap.pairing.detail.as_deref(), Some(MARK_REJECTED_DETAIL));
    }

    // A later Bound { Failed } leaves phase NotPaired with mark_rejected
    publish_pairing(
        &sync,
        &confirmation,
        &tombstone,
        PairingWrite::Bound {
            binding: binding.clone(),
            label: "Home".into(),
            mark: None,
            kind: BoundKind::Failed {
                detail: Some("late_error".into()),
            },
        },
    );
    {
        let snap = sync.lock().unwrap();
        assert_eq!(snap.pairing.phase, PairingPhase::NotPaired);
        assert_eq!(snap.pairing.detail.as_deref(), Some(MARK_REJECTED_DETAIL));
    }

    server_task.abort();
}

/// Cancel deletes pairing.json and sets phase NotPaired with pairing_cancelled.
#[tokio::test]
async fn mark_confirmation_cancel_cleans_up_state() {
    let (cert, key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    let delete_received = Arc::new(Mutex::new(Vec::new()));
    let slot = Arc::new(tokio::sync::Mutex::new(UploaderSlot::new()));

    let cred = direct_credential(pin, port);
    let der_certs = spl_transport::tls::parse_certs(&cred.client_cert_pem).unwrap();
    let expected_client_id = format!("sha256:{}", spl_core::ca::sha256_hex(der_certs[0].as_ref()));

    let delete_rec = delete_received.clone();
    let expected_id = expected_client_id.clone();
    let server_task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            let delete_rec = delete_rec.clone();
            let expected_id = expected_id.clone();
            tokio::spawn(async move {
                if let Ok(mut tls) = acceptor.accept(stream).await {
                    let (stream_id, req) = read_framed_request(&mut tls).await;
                    let req_str = String::from_utf8_lossy(&req);
                    if req_str.contains("/app/network/api/clients/") {
                        delete_rec.lock().unwrap().push(req_str.to_string());
                        if req_str.contains(&expected_id) {
                            let resp = b"HTTP/1.1 204 No Content\r\n\r\n".to_vec();
                            write_response(&mut tls, stream_id, resp).await;
                        } else {
                            let resp = b"HTTP/1.1 404 Not Found\r\n\r\n".to_vec();
                            write_response(&mut tls, stream_id, resp).await;
                        }
                    }
                }
            });
        }
    });

    let dir = TempDir::new("cancel-clean");
    let state_path = dir.path().join("pairing.json");
    let confirmation = Arc::new(Mutex::new(String::new()));
    let tombstone = Arc::new(Mutex::new(None));
    let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
    let jv = Arc::new(JournalVersionController::new(dir.path().join("jv.json")));
    let cfg = SyncConfig {
        device_label: "test".into(),
        period_secs: 300,
        segments_root: dir.path().join("segments"),
        state_path: state_path.clone(),
        local_offset: Arc::new(UtcOffset),
        journal_version: jv,
        facts_fn: Arc::new(pl_transport_win::RawDeviceFacts::default),
        confirmation: confirmation.clone(),
        tombstone: tombstone.clone(),
        #[cfg(feature = "awaiting-hold")]
        awaiting_hold: None,
    };

    let binding = JournalIdentity::from_credential(&cred).client_cert_sha256;
    let paired = PairedState {
        credential: Some(cred),
        ..Default::default()
    };
    paired.save(&state_path).unwrap();

    let access = CredentialAccess::bind(&paired, &cfg, sync.clone(), None).unwrap();
    let access_mutex = Arc::new(tokio::sync::Mutex::new(Some(access)));

    {
        let mut snap = sync.lock().unwrap();
        snap.pairing.binding = binding.clone();
        snap.pairing.phase = PairingPhase::AwaitingConfirmation;
    }

    answer(
        PairingAction::Cancel,
        &binding,
        &cfg,
        &sync,
        &access_mutex,
        &slot,
    )
    .await
    .expect("answer cancel");

    assert!(!state_path.exists());
    {
        let snap = sync.lock().unwrap();
        assert_eq!(snap.pairing.phase, PairingPhase::NotPaired);
        assert_eq!(
            snap.pairing.detail.as_deref(),
            Some(observer_model::PAIRING_CANCELLED_DETAIL)
        );
    }

    server_task.abort();
}

/// Reject against an unreachable journal still completes local retire and deletes pairing.json.
#[tokio::test]
async fn mark_confirmation_reject_unreachable_journal_completes_local_retire() {
    let (cert, _key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener); // Closed port

    let dir = TempDir::new("reject-unreachable");
    let state_path = dir.path().join("pairing.json");
    let confirmation = Arc::new(Mutex::new(String::new()));
    let tombstone = Arc::new(Mutex::new(None));
    let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
    let jv = Arc::new(JournalVersionController::new(dir.path().join("jv.json")));
    let cfg = SyncConfig {
        device_label: "test".into(),
        period_secs: 300,
        segments_root: dir.path().join("segments"),
        state_path: state_path.clone(),
        local_offset: Arc::new(UtcOffset),
        journal_version: jv,
        facts_fn: Arc::new(pl_transport_win::RawDeviceFacts::default),
        confirmation: confirmation.clone(),
        tombstone: tombstone.clone(),
        #[cfg(feature = "awaiting-hold")]
        awaiting_hold: None,
    };

    let cred = direct_credential(pin, port);
    let binding = JournalIdentity::from_credential(&cred).client_cert_sha256;
    let paired = PairedState {
        credential: Some(cred),
        ..Default::default()
    };
    paired.save(&state_path).unwrap();

    let slot = Arc::new(tokio::sync::Mutex::new(UploaderSlot::new()));
    let access = CredentialAccess::bind(&paired, &cfg, sync.clone(), None).unwrap();
    let access_mutex = Arc::new(tokio::sync::Mutex::new(Some(access)));

    {
        let mut snap = sync.lock().unwrap();
        snap.pairing.binding = binding.clone();
        snap.pairing.phase = PairingPhase::AwaitingConfirmation;
    }

    let _ = answer(
        PairingAction::Reject,
        &binding,
        &cfg,
        &sync,
        &access_mutex,
        &slot,
    )
    .await;

    assert!(!state_path.exists());
    {
        let snap = sync.lock().unwrap();
        assert_eq!(snap.pairing.phase, PairingPhase::NotPaired);
        assert_eq!(snap.pairing.detail.as_deref(), Some(MARK_REJECTED_DETAIL));
    }
}

/// Reject receiving non-success HTTP status on computed id still completes local retire.
#[tokio::test]
async fn mark_confirmation_reject_non_success_status_completes_local_retire() {
    let (cert, key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));

    let cred = direct_credential(pin, port);
    let der_certs = spl_transport::tls::parse_certs(&cred.client_cert_pem).unwrap();
    let expected_client_id = format!("sha256:{}", spl_core::ca::sha256_hex(der_certs[0].as_ref()));

    let expected_id = expected_client_id.clone();
    let server_task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            let expected_id = expected_id.clone();
            tokio::spawn(async move {
                if let Ok(mut tls) = acceptor.accept(stream).await {
                    let (stream_id, req) = read_framed_request(&mut tls).await;
                    let req_str = String::from_utf8_lossy(&req);
                    if req_str.contains("/app/network/api/clients/") {
                        if req_str.contains(&expected_id) {
                            let resp = b"HTTP/1.1 500 Internal Server Error\r\n\r\n".to_vec();
                            write_response(&mut tls, stream_id, resp).await;
                        } else {
                            let resp = b"HTTP/1.1 404 Not Found\r\n\r\n".to_vec();
                            write_response(&mut tls, stream_id, resp).await;
                        }
                    }
                }
            });
        }
    });

    let dir = TempDir::new("reject-500");
    let state_path = dir.path().join("pairing.json");
    let confirmation = Arc::new(Mutex::new(String::new()));
    let tombstone = Arc::new(Mutex::new(None));
    let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
    let jv = Arc::new(JournalVersionController::new(dir.path().join("jv.json")));
    let cfg = SyncConfig {
        device_label: "test".into(),
        period_secs: 300,
        segments_root: dir.path().join("segments"),
        state_path: state_path.clone(),
        local_offset: Arc::new(UtcOffset),
        journal_version: jv,
        facts_fn: Arc::new(pl_transport_win::RawDeviceFacts::default),
        confirmation: confirmation.clone(),
        tombstone: tombstone.clone(),
        #[cfg(feature = "awaiting-hold")]
        awaiting_hold: None,
    };

    let binding = JournalIdentity::from_credential(&cred).client_cert_sha256;
    let paired = PairedState {
        credential: Some(cred),
        ..Default::default()
    };
    paired.save(&state_path).unwrap();

    let slot = Arc::new(tokio::sync::Mutex::new(UploaderSlot::new()));
    let access = CredentialAccess::bind(&paired, &cfg, sync.clone(), None).unwrap();
    let access_mutex = Arc::new(tokio::sync::Mutex::new(Some(access)));

    {
        let mut snap = sync.lock().unwrap();
        snap.pairing.binding = binding.clone();
        snap.pairing.phase = PairingPhase::AwaitingConfirmation;
    }

    let _ = answer(
        PairingAction::Reject,
        &binding,
        &cfg,
        &sync,
        &access_mutex,
        &slot,
    )
    .await;

    assert!(!state_path.exists());
    {
        let snap = sync.lock().unwrap();
        assert_eq!(snap.pairing.phase, PairingPhase::NotPaired);
        assert_eq!(snap.pairing.detail.as_deref(), Some(MARK_REJECTED_DETAIL));
    }

    server_task.abort();
}

/// A failed rejected write still stops uploader, sends one DELETE, and deletes pairing.json.
#[tokio::test]
async fn mark_confirmation_reject_failed_answer_write_stops_and_deletes() {
    let (cert, key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    let delete_received = Arc::new(Mutex::new(Vec::new()));

    let cred = direct_credential(pin, port);
    let der_certs = spl_transport::tls::parse_certs(&cred.client_cert_pem).unwrap();
    let expected_client_id = format!("sha256:{}", spl_core::ca::sha256_hex(der_certs[0].as_ref()));

    let delete_rec = delete_received.clone();
    let expected_id = expected_client_id.clone();
    let server_task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            let delete_rec = delete_rec.clone();
            let expected_id = expected_id.clone();
            tokio::spawn(async move {
                if let Ok(mut tls) = acceptor.accept(stream).await {
                    let (stream_id, req) = read_framed_request(&mut tls).await;
                    let req_str = String::from_utf8_lossy(&req);
                    if req_str.contains("/app/network/api/clients/") {
                        delete_rec.lock().unwrap().push(req_str.to_string());
                        if req_str.contains(&expected_id) {
                            let resp = b"HTTP/1.1 204 No Content\r\n\r\n".to_vec();
                            write_response(&mut tls, stream_id, resp).await;
                        } else {
                            let resp = b"HTTP/1.1 404 Not Found\r\n\r\n".to_vec();
                            write_response(&mut tls, stream_id, resp).await;
                        }
                    }
                }
            });
        }
    });

    let dir = TempDir::new("reject-fail-write");
    let state_path = dir.path().join("pairing.json");
    let confirmation = Arc::new(Mutex::new(String::new()));
    let tombstone = Arc::new(Mutex::new(None));
    let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
    let jv = Arc::new(JournalVersionController::new(dir.path().join("jv.json")));
    let cfg = SyncConfig {
        device_label: "test".into(),
        period_secs: 300,
        segments_root: dir.path().join("segments"),
        state_path: state_path.clone(),
        local_offset: Arc::new(UtcOffset),
        journal_version: jv,
        facts_fn: Arc::new(pl_transport_win::RawDeviceFacts::default),
        confirmation: confirmation.clone(),
        tombstone: tombstone.clone(),
        #[cfg(feature = "awaiting-hold")]
        awaiting_hold: None,
    };

    let binding = JournalIdentity::from_credential(&cred).client_cert_sha256;
    let paired = PairedState {
        credential: Some(cred),
        ..Default::default()
    };
    paired.save(&state_path).unwrap();

    // Pre-create pairing-answer.json.tmp as a directory so write_answer fails
    std::fs::create_dir_all(dir.path().join("pairing-answer.json.tmp")).unwrap();

    let slot = Arc::new(tokio::sync::Mutex::new(UploaderSlot::new()));
    let access = CredentialAccess::bind(&paired, &cfg, sync.clone(), None).unwrap();
    let access_mutex = Arc::new(tokio::sync::Mutex::new(Some(access)));

    {
        let mut snap = sync.lock().unwrap();
        snap.pairing.binding = binding.clone();
        snap.pairing.phase = PairingPhase::AwaitingConfirmation;
    }

    let _ = answer(
        PairingAction::Reject,
        &binding,
        &cfg,
        &sync,
        &access_mutex,
        &slot,
    )
    .await;

    assert_eq!(delete_received.lock().unwrap().len(), 1);
    assert!(!state_path.exists());

    server_task.abort();
}

/// Stale answer changes nothing.
#[tokio::test]
async fn mark_confirmation_stale_answer_changes_nothing() {
    let (cert, _key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let dir = TempDir::new("stale-answer");
    let state_path = dir.path().join("pairing.json");
    let confirmation = Arc::new(Mutex::new(String::new()));
    let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
    let jv = Arc::new(JournalVersionController::new(dir.path().join("jv.json")));
    let cfg = SyncConfig {
        device_label: "test".into(),
        period_secs: 300,
        segments_root: dir.path().join("segments"),
        state_path: state_path.clone(),
        local_offset: Arc::new(UtcOffset),
        journal_version: jv,
        facts_fn: Arc::new(pl_transport_win::RawDeviceFacts::default),
        confirmation: confirmation.clone(),
        tombstone: Arc::new(Mutex::new(None)),
        #[cfg(feature = "awaiting-hold")]
        awaiting_hold: None,
    };

    let cred = direct_credential(pin, 1);
    let paired = PairedState {
        credential: Some(cred),
        ..Default::default()
    };
    paired.save(&state_path).unwrap();

    let slot = Arc::new(tokio::sync::Mutex::new(UploaderSlot::new()));
    let access = CredentialAccess::bind(&paired, &cfg, sync.clone(), None).unwrap();
    let access_mutex = Arc::new(tokio::sync::Mutex::new(Some(access)));

    // Modify sync snapshot binding to simulate a different active pairing
    {
        let mut snap = sync.lock().unwrap();
        snap.pairing.binding = "different-binding".to_string();
        snap.pairing.phase = PairingPhase::AwaitingConfirmation;
    }

    // Call answer with old binding
    answer(
        PairingAction::Reject,
        "old-binding",
        &cfg,
        &sync,
        &access_mutex,
        &slot,
    )
    .await
    .expect("answer stale");

    // Nothing touched: pairing.json still exists, snapshot unchanged
    assert!(state_path.exists());
    {
        let snap = sync.lock().unwrap();
        assert_eq!(snap.pairing.binding, "different-binding");
        assert_eq!(snap.pairing.phase, PairingPhase::AwaitingConfirmation);
    }
}

/// Fresh pairing publishes AwaitingConfirmation before bind.
#[tokio::test]
async fn mark_confirmation_pair_publishes_awaiting_before_bind() {
    let (cert, _key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let _dir = TempDir::new("pair-publish");
    let confirmation = Arc::new(Mutex::new(String::new()));
    let tombstone = Arc::new(Mutex::new(None));
    let sync = Arc::new(Mutex::new(SyncSnapshot::default()));

    let cred = direct_credential(pin, 1);
    let binding = JournalIdentity::from_credential(&cred).client_cert_sha256;

    // Publish bound with empty confirmation
    publish_pairing(
        &sync,
        &confirmation,
        &tombstone,
        PairingWrite::Bound {
            binding: binding.clone(),
            label: "Home".into(),
            mark: None,
            kind: BoundKind::Paired,
        },
    );

    {
        let snap = sync.lock().unwrap();
        assert_eq!(snap.pairing.phase, PairingPhase::AwaitingConfirmation);
    }

    // Confirm
    *confirmation.lock().unwrap() = binding.clone();
    publish_pairing(
        &sync,
        &confirmation,
        &tombstone,
        PairingWrite::Bound {
            binding: binding.clone(),
            label: "Home".into(),
            mark: None,
            kind: BoundKind::Paired,
        },
    );

    {
        let snap = sync.lock().unwrap();
        assert_eq!(snap.pairing.phase, PairingPhase::Paired);
    }
}

/// Launch finish precedes grandfather.
#[tokio::test]
async fn mark_confirmation_launch_finish_precedes_grandfather() {
    let (cert, _key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let dir = TempDir::new("launch-finish");
    let state_path = dir.path().join("pairing.json");
    let ans_path = answer_path(&state_path);
    let confirmation = Arc::new(Mutex::new(String::new()));
    let tombstone = Arc::new(Mutex::new(None));
    let sync = Arc::new(Mutex::new(SyncSnapshot::default()));

    let cred = direct_credential(pin, 1);
    let digest = JournalIdentity::from_credential(&cred).client_cert_sha256;
    let paired = PairedState {
        credential: Some(cred),
        ..Default::default()
    };
    paired.save(&state_path).unwrap();

    // 1. Answer file has rejected = digest
    let ans = AnswerState {
        confirmed: String::new(),
        rejected: digest.clone(),
    };
    write_answer(&ans_path, &ans).unwrap();

    let skip_resume =
        settle_rejected_on_launch(&state_path, &confirmation, &tombstone, &sync).await;
    assert!(!skip_resume);
    assert!(!state_path.exists());
    let ans_after = read_answer(&ans_path).unwrap().unwrap();
    assert_eq!(ans_after.rejected, "");
    assert_eq!(ans_after.confirmed, "");
    {
        let snap = sync.lock().unwrap();
        assert_eq!(snap.pairing.phase, PairingPhase::NotPaired);
        assert_eq!(snap.pairing.detail, None);
    }

    // 2. Absent answer file + pairing.json becomes confirmed
    paired.save(&state_path).unwrap();
    let _ = std::fs::remove_file(&ans_path);
    settle_grandfather(&state_path, &confirmation).unwrap();
    let ans_after = read_answer(&ans_path).unwrap().unwrap();
    assert_eq!(ans_after.confirmed, digest);
    assert_eq!(*confirmation.lock().unwrap(), digest);

    // 3. Present answer file (empty confirmed) is not rewritten
    let ans_empty = AnswerState::default();
    write_answer(&ans_path, &ans_empty).unwrap();
    *confirmation.lock().unwrap() = String::new();
    settle_grandfather(&state_path, &confirmation).unwrap();
    let ans_after = read_answer(&ans_path).unwrap().unwrap();
    assert_eq!(ans_after.confirmed, "");
    assert_eq!(*confirmation.lock().unwrap(), "");
}

/// Pair mark gates the save in integration ops.
#[tokio::test]
async fn mark_confirmation_pair_mark_gates_the_save() {
    // 1. 0-connection validation failure when --mark is missing from pair args
    let parse_err = pl_transport_win::integration::args::parse(&[
        "--integration",
        "pair",
        "--deadline-secs",
        "20",
        "--carrier",
        "relay",
    ])
    .unwrap_err();
    assert_eq!(parse_err.reason, "arg_missing");
    assert_eq!(parse_err.operation, "pair");

    // 2. Setup mock relay and direct server with real mark
    let state = Arc::new(support::relay_pairing::MockState::normal().with_same_tls_ca());
    let jid = support::relay_pairing::jid_for_ca(&state.json_ca);
    let mark_spec = mark_spec_for_jid(&jid).expect("valid mark for jid");
    let (w0, w1) = (&mark_spec.words[0], &mark_spec.words[1]);

    let direct_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let direct_port = direct_listener.local_addr().unwrap().port();
    *state.local_endpoint_override.lock().unwrap() = Some(("127.0.0.1".into(), direct_port));

    let delete_paths = Arc::new(Mutex::new(Vec::<String>::new()));
    let delete_paths_clone = delete_paths.clone();
    let acceptor = TlsAcceptor::from(Arc::new(support::relay_pairing::leaf_config(
        state.json_ca.as_ref(),
    )));

    let direct_task = tokio::spawn(async move {
        while let Ok((stream, _)) = direct_listener.accept().await {
            let acceptor = acceptor.clone();
            let delete_paths = delete_paths_clone.clone();
            tokio::spawn(async move {
                if let Ok(mut tls) = acceptor.accept(stream).await {
                    loop {
                        let (stream_id, req) = read_framed_request(&mut tls).await;
                        if req.is_empty() {
                            break;
                        }
                        let req_str = String::from_utf8_lossy(&req);
                        if let Some(first_line) = req_str.lines().next() {
                            if first_line.starts_with("DELETE ") {
                                let path = first_line
                                    .split_whitespace()
                                    .nth(1)
                                    .unwrap_or("")
                                    .to_string();
                                delete_paths.lock().unwrap().push(path);
                                let body = br#"{"error":"not_found"}"#;
                                let resp = format!(
                                    "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                                    body.len(),
                                    String::from_utf8_lossy(body)
                                )
                                .into_bytes();
                                write_response(&mut tls, stream_id, resp).await;
                            } else if first_line.starts_with("POST /app/devices/ingest") {
                                let body = br#"{"status":"ok","segment":"000000_300"}"#;
                                let resp = format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                                    body.len(),
                                    String::from_utf8_lossy(body)
                                )
                                .into_bytes();
                                write_response(&mut tls, stream_id, resp).await;
                            } else {
                                let body = br#"{"error":"not_found"}"#;
                                let resp = format!(
                                    "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                                    body.len(),
                                    String::from_utf8_lossy(body)
                                )
                                .into_bytes();
                                write_response(&mut tls, stream_id, resp).await;
                            }
                        }
                    }
                }
            });
        }
    });

    let relay_origin = support::relay_pairing::spawn_mock_relay(state.clone()).await;
    let link = support::relay_pairing::relay_form_link(
        &relay_origin,
        &support::relay_pairing::PAIR_SECRET,
        &state.json_ca.spki_pin(),
    );

    // The first pairing advertises an address whose TCP listener never completes
    // TLS, just as an unreachable advertised address can exhaust retirement.
    let stale_lan = TcpListener::bind("127.0.0.1:0").await.unwrap();
    *state.local_endpoint_override.lock().unwrap() =
        Some(("127.0.0.1".into(), stale_lan.local_addr().unwrap().port()));

    let dir = TempDir::new("pair-mark");
    let env = pl_transport_win::integration::Environment {
        state_path: dir.path().join("pairing.json"),
        segments_root: dir.path().join("segments"),
        device_label: "test-device".into(),
        app_version: "0.0.0".into(),
        period_secs: 300,
        executable: None,
        source_commit: None,
    };
    let pair_cmd = pl_transport_win::integration::args::Command {
        operation: pl_transport_win::integration::args::Operation::Pair,
        deadline: Duration::from_secs(20),
        max_dials: None,
        args: pl_transport_win::integration::args::OperationArgs::Pair {
            carrier: pl_transport_win::integration::args::Carrier::Relay,
            mark: ("wrong1".into(), "wrong2".into()),
        },
    };

    // 3. Mismatch run: supplied words do not match mark_spec_for_jid
    let (failure, evidence) = pl_transport_win::integration::ops::pair(
        &pair_cmd,
        &env,
        None,
        link.clone(),
        pl_transport_win::integration::args::Carrier::Relay,
        ("wrong1".into(), "wrong2".into()),
    )
    .await;

    let failure = failure.expect("mismatched mark must fail");
    assert!(
        matches!(&failure, pl_transport_win::integration::report::Failure::Assertion { reason, .. } if reason == "mark_mismatch")
    );
    assert!(!env.state_path.exists(), "pairing.json must be absent");
    assert!(
        !env.state_tmp_path().exists(),
        "pairing.json.tmp must be absent"
    );
    assert!(
        !answer_path(&env.state_path).exists(),
        "answer file must be unchanged"
    );
    assert_eq!(
        state.delete_paths.lock().unwrap().len(),
        1,
        "exactly one DELETE request"
    );
    assert!(
        delete_paths.lock().unwrap().is_empty(),
        "retirement used the direct path"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), stale_lan.accept())
            .await
            .is_err(),
        "retirement dialed stale LAN"
    );
    let delete_path = state.delete_paths.lock().unwrap()[0].clone();
    assert!(
        delete_path.starts_with("/app/network/api/clients/sha256:"),
        "DELETE path must be /app/network/api/clients/sha256:..., got {delete_path}"
    );
    assert_eq!(
        evidence.remote_residue.unwrap().journal_pairing_identity,
        pl_transport_win::integration::report::Residue::None,
        "journal_pairing_identity must be None on 404 delete"
    );

    *state.local_endpoint_override.lock().unwrap() = Some(("127.0.0.1".into(), direct_port));

    // 4. Match run: matching --mark (any case, middot allowed) writes confirmed and saves pairing.json
    let parsed_cmd = pl_transport_win::integration::args::parse(&[
        "--integration",
        "pair",
        "--deadline-secs",
        "20",
        "--carrier",
        "relay",
        "--mark",
        &format!("{} · {}", w0.to_uppercase(), w1.to_lowercase()),
    ])
    .unwrap();
    let mark = match parsed_cmd.args {
        pl_transport_win::integration::args::OperationArgs::Pair { mark, .. } => mark,
        _ => unreachable!(),
    };
    let (failure, evidence) = pl_transport_win::integration::ops::pair(
        &pair_cmd,
        &env,
        None,
        link.clone(),
        pl_transport_win::integration::args::Carrier::Relay,
        mark,
    )
    .await;

    assert!(failure.is_none(), "matching mark must succeed: {failure:?}");
    assert_eq!(evidence.state_written, Some(true));
    assert!(env.state_path.exists(), "pairing.json must exist");

    let ans_path = answer_path(&env.state_path);
    let ans_state = read_answer(&ans_path)
        .unwrap()
        .expect("answer state must exist");
    let paired = PairedState::load(&env.state_path).unwrap();
    let cred = paired.credential.as_ref().unwrap();
    let digest = JournalIdentity::from_credential(cred).client_cert_sha256;
    assert_eq!(ans_state.confirmed, digest);

    // 5. Client built by client_for can ingest (gate is open)
    let client =
        pl_transport_win::integration::ops::client_for(&env, &paired, None).expect("client_for");
    assert!(
        client.gate_open(),
        "gate must be open for confirmed pairing"
    );
    let (response, _) = client
        .ingest("000000_300", "20260930", test_file_parts(), None)
        .await
        .expect("ingest must succeed");
    assert_eq!(response.status, observer_pl::ingest::IngestStatus::Ok);

    direct_task.abort();
}

/// Metadata routes dial while awaiting confirmation.
#[tokio::test]
async fn mark_confirmation_metadata_routes_dial_while_awaiting() {
    let (cert, key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    let system_status_count = Arc::new(AtomicUsize::new(0));
    let clients_self_count = Arc::new(AtomicUsize::new(0));
    let relay_access_count = Arc::new(AtomicUsize::new(0));

    let status_count = system_status_count.clone();
    let self_count = clients_self_count.clone();
    let relay_count = relay_access_count.clone();
    let server_task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            let status_count = status_count.clone();
            let self_count = self_count.clone();
            let relay_count = relay_count.clone();
            tokio::spawn(async move {
                if let Ok(mut tls) = acceptor.accept(stream).await {
                    loop {
                        let (stream_id, req) = read_framed_request(&mut tls).await;
                        if req.is_empty() {
                            break;
                        }
                        let req_str = String::from_utf8_lossy(&req);
                        if req_str.starts_with("GET /api/system/status") {
                            status_count.fetch_add(1, Ordering::SeqCst);
                            let body = br#"{"version":{"current":"1.2.3"}}"#;
                            let resp = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                                body.len(),
                                String::from_utf8_lossy(body)
                            )
                            .into_bytes();
                            write_response(&mut tls, stream_id, resp).await;
                        } else if req_str.starts_with("GET /app/network/api/clients/self") {
                            self_count.fetch_add(1, Ordering::SeqCst);
                            let body = br#"{"status":"ok"}"#;
                            let resp = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                                body.len(),
                                String::from_utf8_lossy(body)
                            )
                            .into_bytes();
                            write_response(&mut tls, stream_id, resp).await;
                        } else if req_str.starts_with("GET /app/network/api/relay/access") {
                            relay_count.fetch_add(1, Ordering::SeqCst);
                            let body = br#"{"status":"ok"}"#;
                            let resp = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                                body.len(),
                                String::from_utf8_lossy(body)
                            )
                            .into_bytes();
                            write_response(&mut tls, stream_id, resp).await;
                        }
                    }
                }
            });
        }
    });

    let cred = direct_credential(pin, port);
    let gate = Arc::new(AtomicBool::new(false)); // Awaiting confirmation
    let client = ObserverClient::new(cred, gate).unwrap();
    assert!(!client.gate_open());

    // 1. system_status dials while awaiting confirmation
    let version = client.system_status().await.expect("system status");
    assert_eq!(version, "1.2.3");
    assert_eq!(system_status_count.load(Ordering::SeqCst), 1);

    // 2. get_clients_self dials while awaiting confirmation
    let clients_self = client.get_clients_self().await.expect("get clients self");
    assert_eq!(clients_self.status, 200);
    assert_eq!(clients_self_count.load(Ordering::SeqCst), 1);

    // 3. get_relay_access dials while awaiting confirmation
    let relay_access = client.get_relay_access().await.expect("get relay access");
    assert_eq!(relay_access.status, 200);
    assert_eq!(relay_access_count.load(Ordering::SeqCst), 1);

    // Gate remains closed throughout
    assert!(!client.gate_open());

    server_task.abort();
}

#[tokio::test]
async fn mark_confirmation_integration_unconfirmed_operations_fail() {
    let (cert, key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    let accepts = Arc::new(AtomicUsize::new(0));

    let accepts_clone = accepts.clone();
    let server_task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            accepts_clone.fetch_add(1, Ordering::SeqCst);
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Ok(mut tls) = acceptor.accept(stream).await {
                    loop {
                        let (stream_id, req) = read_framed_request(&mut tls).await;
                        if req.is_empty() {
                            break;
                        }
                        let resp = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 15\r\n\r\n{\"status\":\"ok\"}".to_vec();
                        write_response(&mut tls, stream_id, resp).await;
                    }
                }
            });
        }
    });

    let dir = TempDir::new("int-unconfirmed");
    let cred = direct_credential(pin, port);
    let paired = PairedState {
        credential: Some(cred),
        ..Default::default()
    };
    paired.save(&dir.path().join("pairing.json")).unwrap();

    let ans_empty = AnswerState::default();
    write_answer(&answer_path(&dir.path().join("pairing.json")), &ans_empty).unwrap();

    let env = pl_transport_win::integration::Environment {
        state_path: dir.path().join("pairing.json"),
        segments_root: dir.path().join("segments"),
        device_label: "test-device".into(),
        app_version: "0.0.0".into(),
        period_secs: 300,
        executable: None,
        source_commit: None,
    };

    // 1. roundtrip unconfirmed -> awaiting_confirmation
    let observer = Arc::new(spl_transport::observe::OperationObserver::default());
    let handle = Some(observer.clone());
    let roundtrip_cmd = pl_transport_win::integration::args::Command {
        operation: pl_transport_win::integration::args::Operation::Roundtrip,
        deadline: Duration::from_secs(5),
        max_dials: None,
        args: pl_transport_win::integration::args::OperationArgs::Roundtrip {
            carrier: pl_transport_win::integration::args::Carrier::Direct,
        },
    };
    let (failure, _) = pl_transport_win::integration::ops::roundtrip(
        &roundtrip_cmd,
        &env,
        handle.clone(),
        &observer,
        pl_transport_win::integration::args::Carrier::Direct,
    )
    .await;
    let failure = failure.expect("unconfirmed roundtrip must fail");
    assert!(
        matches!(&failure, pl_transport_win::integration::report::Failure::Error { reason, .. } if reason == "awaiting_confirmation"),
        "roundtrip must fail with awaiting_confirmation"
    );

    // 2. fetch unconfirmed -> awaiting_confirmation
    let fetch_cmd = pl_transport_win::integration::args::Command {
        operation: pl_transport_win::integration::args::Operation::Fetch,
        deadline: Duration::from_secs(5),
        max_dials: None,
        args: pl_transport_win::integration::args::OperationArgs::Fetch {
            journal_path: "/test".into(),
            expected_bytes: 15,
            expected_sha256: "dummy".into(),
            expected_status: 200,
            carrier: pl_transport_win::integration::args::Carrier::Direct,
        },
    };
    let (failure, _) = pl_transport_win::integration::ops::fetch(
        &fetch_cmd,
        &env,
        handle.clone(),
        &observer,
        "/test",
        15,
        "dummy",
        200,
        pl_transport_win::integration::args::Carrier::Direct,
    )
    .await;
    let failure = failure.expect("unconfirmed fetch must fail");
    assert!(
        matches!(&failure, pl_transport_win::integration::report::Failure::Error { reason, .. } if reason == "awaiting_confirmation"),
        "fetch must fail with awaiting_confirmation"
    );

    // 3. upload unconfirmed -> awaiting_confirmation
    let payload_path = dir.path().join("test_payload.txt");
    std::fs::write(&payload_path, b"test-payload-bytes").unwrap();
    let upload_cmd = pl_transport_win::integration::args::Command {
        operation: pl_transport_win::integration::args::Operation::Upload,
        deadline: Duration::from_secs(5),
        max_dials: None,
        args: pl_transport_win::integration::args::OperationArgs::Upload {
            payload: payload_path.clone(),
            day: "20260930".into(),
            segment: "000000_300".into(),
            carrier: pl_transport_win::integration::args::Carrier::Direct,
        },
    };
    let (failure, _) = pl_transport_win::integration::ops::upload(
        &upload_cmd,
        &env,
        handle,
        &observer,
        &payload_path,
        "20260930",
        "000000_300",
        pl_transport_win::integration::args::Carrier::Direct,
    )
    .await;
    let failure = failure.expect("unconfirmed upload must fail");
    assert!(
        matches!(&failure, pl_transport_win::integration::report::Failure::Error { reason, .. } if reason == "awaiting_confirmation"),
        "upload must fail with awaiting_confirmation"
    );

    assert_eq!(accepts.load(Ordering::SeqCst), 0);

    server_task.abort();
}

#[tokio::test]
async fn mark_confirmation_tombstone_and_re_pair() {
    let _dir = TempDir::new("tombstone");
    let confirmation = Arc::new(Mutex::new(String::new()));
    let tombstone = Arc::new(Mutex::new(None));
    let sync = Arc::new(Mutex::new(SyncSnapshot::default()));

    let (cert_b, _key_b) = self_signed();
    let pin_b = spl_core::ca::sha256(cert_b.as_ref())[..16].to_vec();
    let cred_b = direct_credential(pin_b, 1);
    let digest_b = JournalIdentity::from_credential(&cred_b).client_cert_sha256;

    let (cert_c, _key_c) = self_signed();
    let pin_c = spl_core::ca::sha256(cert_c.as_ref())[..16].to_vec();
    let cred_c = direct_credential(pin_c, 2);
    let digest_c = JournalIdentity::from_credential(&cred_c).client_cert_sha256;

    // Set active binding B and reject -> records tombstone B
    {
        let mut snap = sync.lock().unwrap();
        snap.pairing.binding = digest_b.clone();
    }
    publish_pairing(
        &sync,
        &confirmation,
        &tombstone,
        PairingWrite::NotPaired {
            detail: Some(MARK_REJECTED_DETAIL.to_string()),
        },
    );
    assert_eq!(*tombstone.lock().unwrap(), Some(digest_b.clone()));

    // Bound { Paired, B } and Bound { Failed, B } leave phase NotPaired and detail mark_rejected
    publish_pairing(
        &sync,
        &confirmation,
        &tombstone,
        PairingWrite::Bound {
            binding: digest_b.clone(),
            label: "Home B".into(),
            mark: None,
            kind: BoundKind::Paired,
        },
    );
    assert_eq!(*tombstone.lock().unwrap(), Some(digest_b.clone()));
    {
        let snap = sync.lock().unwrap();
        assert_eq!(snap.pairing.phase, PairingPhase::NotPaired);
        assert_eq!(snap.pairing.detail.as_deref(), Some(MARK_REJECTED_DETAIL));
    }

    publish_pairing(
        &sync,
        &confirmation,
        &tombstone,
        PairingWrite::Bound {
            binding: digest_b.clone(),
            label: "Home B".into(),
            mark: None,
            kind: BoundKind::Failed {
                detail: Some("failure".into()),
            },
        },
    );
    assert_eq!(*tombstone.lock().unwrap(), Some(digest_b.clone()));
    {
        let snap = sync.lock().unwrap();
        assert_eq!(snap.pairing.phase, PairingPhase::NotPaired);
        assert_eq!(snap.pairing.detail.as_deref(), Some(MARK_REJECTED_DETAIL));
    }

    // BeginCeremony sets phase Pairing and leaves tombstone as B
    publish_pairing(
        &sync,
        &confirmation,
        &tombstone,
        PairingWrite::BeginCeremony,
    );
    assert_eq!(*tombstone.lock().unwrap(), Some(digest_b.clone()));
    assert_eq!(sync.lock().unwrap().pairing.phase, PairingPhase::Pairing);

    // Bound { Paired, B } during that ceremony leaves phase Pairing
    publish_pairing(
        &sync,
        &confirmation,
        &tombstone,
        PairingWrite::Bound {
            binding: digest_b.clone(),
            label: "Home B".into(),
            mark: None,
            kind: BoundKind::Paired,
        },
    );
    assert_eq!(*tombstone.lock().unwrap(), Some(digest_b.clone()));
    assert_eq!(sync.lock().unwrap().pairing.phase, PairingPhase::Pairing);

    // Bound { Paired, C } lands as AwaitingConfirmation
    publish_pairing(
        &sync,
        &confirmation,
        &tombstone,
        PairingWrite::Bound {
            binding: digest_c.clone(),
            label: "Home C".into(),
            mark: None,
            kind: BoundKind::Paired,
        },
    );
    assert_eq!(
        sync.lock().unwrap().pairing.phase,
        PairingPhase::AwaitingConfirmation
    );

    // Non-empty matching Bound { Failed } publishes Failed phase with that detail and binding
    publish_pairing(
        &sync,
        &confirmation,
        &tombstone,
        PairingWrite::Bound {
            binding: digest_c.clone(),
            label: "Home C".into(),
            mark: None,
            kind: BoundKind::Failed {
                detail: Some("journal_refused".into()),
            },
        },
    );
    {
        let snap = sync.lock().unwrap();
        assert_eq!(snap.pairing.phase, PairingPhase::Failed);
        assert_eq!(snap.pairing.detail.as_deref(), Some("journal_refused"));
        assert_eq!(snap.pairing.binding, digest_c);
    }

    // With an empty snapshot binding, Bound { Failed } does not publish (phase and detail unchanged)
    {
        let mut snap = sync.lock().unwrap();
        snap.pairing.binding = String::new();
        snap.pairing.phase = PairingPhase::NotPaired;
        snap.pairing.detail = Some(MARK_REJECTED_DETAIL.into());
    }
    publish_pairing(
        &sync,
        &confirmation,
        &tombstone,
        PairingWrite::Bound {
            binding: digest_c.clone(),
            label: "Home C".into(),
            mark: None,
            kind: BoundKind::Failed {
                detail: Some("failed_detail".into()),
            },
        },
    );
    {
        let snap = sync.lock().unwrap();
        assert_eq!(snap.pairing.phase, PairingPhase::NotPaired);
        assert_eq!(snap.pairing.detail.as_deref(), Some(MARK_REJECTED_DETAIL));
        assert_eq!(snap.pairing.binding, "");
    }

    // Unbound PairingWrite::Failed { detail } still sets phase Failed and empty binding
    publish_pairing(
        &sync,
        &confirmation,
        &tombstone,
        PairingWrite::Failed {
            detail: "ceremony_failure".into(),
        },
    );
    {
        let snap = sync.lock().unwrap();
        assert_eq!(snap.pairing.phase, PairingPhase::Failed);
        assert_eq!(snap.pairing.detail.as_deref(), Some("ceremony_failure"));
        assert_eq!(snap.pairing.binding, "");
    }
}

/// Launch test: saved pairing, answer confirmed empty, launch_resume once.
/// Nothing is accepted by journal listener. Phase is AwaitingConfirmation.
/// Tray tooltip matches expected text. Confirm on same uploader uploads.
#[tokio::test]
async fn mark_confirmation_launch_resume_awaits_until_confirmed_then_uploads() {
    let (cert, key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, key)));
    let accepts = Arc::new(AtomicUsize::new(0));
    let upload_received = Arc::new(AtomicBool::new(false));

    let accepts_clone = accepts.clone();
    let upload_flag = upload_received.clone();
    let server_task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            accepts_clone.fetch_add(1, Ordering::SeqCst);
            let acceptor = acceptor.clone();
            let upload_flag = upload_flag.clone();
            tokio::spawn(async move {
                if let Ok(mut tls) = acceptor.accept(stream).await {
                    let (stream_id, req) = read_framed_request(&mut tls).await;
                    let req_str = String::from_utf8_lossy(&req);
                    if req_str.contains("/app/devices/ingest") {
                        upload_flag.store(true, Ordering::SeqCst);
                    }
                    let resp = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 38\r\n\r\n{\"status\":\"ok\",\"segment\":\"000000_300\"}"
                        .to_vec();
                    write_response(&mut tls, stream_id, resp).await;
                }
            });
        }
    });

    let dir = TempDir::new("launch-resume-await");
    let segments_dir = dir.path().join("segments");
    let day_dir = segments_dir.join("20260930");
    std::fs::create_dir_all(&day_dir).unwrap();
    std::fs::write(day_dir.join("000000_300.tar.gz"), b"dummy-data").unwrap();

    let state_path = dir.path().join("pairing.json");
    let confirmation = Arc::new(Mutex::new(String::new()));
    let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
    let jv = Arc::new(JournalVersionController::new(dir.path().join("jv.json")));
    let cfg = SyncConfig {
        device_label: "test".into(),
        period_secs: 300,
        segments_root: segments_dir,
        state_path: state_path.clone(),
        local_offset: Arc::new(UtcOffset),
        journal_version: jv,
        facts_fn: Arc::new(pl_transport_win::RawDeviceFacts::default),
        confirmation: confirmation.clone(),
        tombstone: Arc::new(Mutex::new(None)),
        #[cfg(feature = "awaiting-hold")]
        awaiting_hold: None,
    };

    let cred = direct_credential(pin, port);
    let binding = JournalIdentity::from_credential(&cred).client_cert_sha256;
    let paired = PairedState {
        credential: Some(cred),
        ..Default::default()
    };
    paired.save(&state_path).unwrap();

    let ans_path = answer_path(&state_path);
    write_answer(&ans_path, &AnswerState::default()).unwrap();

    let mut slot = UploaderSlot::new();
    let access = pl_transport_win::service::launch_resume(&cfg, &sync, &mut slot).await;
    assert!(access.is_some());
    let access_mutex = Arc::new(tokio::sync::Mutex::new(access));
    let slot_mutex = Arc::new(tokio::sync::Mutex::new(slot));

    tokio::time::sleep(Duration::from_millis(100)).await;

    // No uploads before confirmation
    assert!(!upload_received.load(Ordering::SeqCst));

    let snapshot = sync.lock().unwrap().clone();
    assert_eq!(snapshot.pairing.phase, PairingPhase::AwaitingConfirmation);

    let tray =
        observer_model::classify_tray(observer_model::AppPhase::Observing, &snapshot, None, None);
    assert_eq!(
        tray.1,
        "solstone · waiting for you to confirm your journal's mark"
    );

    // session::answer Confirm on same uploader uploads
    answer(
        PairingAction::Confirm,
        &binding,
        &cfg,
        &sync,
        &access_mutex,
        &slot_mutex,
    )
    .await
    .expect("answer confirm");

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(upload_received.load(Ordering::SeqCst));

    server_task.abort();
}

/// 1. Absent answer, invalid pairing.json, ceremony fails -> empty answer on disk, replace with pre-gate cred A -> launch_resume leaves AwaitingConfirmation (not grandfathered).
#[tokio::test]
async fn session_pair_invalid_pairing_json_creates_empty_answer_file_and_leaves_unconfirmed_on_resume(
) {
    let dir = TempDir::new("pair-invalid-json");
    let state_path = dir.path().join("pairing.json");
    let ans_path = answer_path(&state_path);
    std::fs::write(&state_path, b"invalid json content").unwrap();

    let confirmation = Arc::new(Mutex::new(String::new()));
    let tombstone = Arc::new(Mutex::new(None));
    let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
    let jv = Arc::new(JournalVersionController::new(dir.path().join("jv.json")));
    let cfg = SyncConfig {
        device_label: "test".into(),
        period_secs: 300,
        segments_root: dir.path().join("segments"),
        state_path: state_path.clone(),
        local_offset: Arc::new(UtcOffset),
        journal_version: jv,
        facts_fn: Arc::new(pl_transport_win::RawDeviceFacts::default),
        confirmation: confirmation.clone(),
        tombstone: tombstone.clone(),
        #[cfg(feature = "awaiting-hold")]
        awaiting_hold: None,
    };

    let slot = Arc::new(tokio::sync::Mutex::new(UploaderSlot::new()));
    let access = Arc::new(tokio::sync::Mutex::new(None));

    let bad_link = "https://go.solstone.app/p#invalid".to_string();
    let res = pl_transport_win::session::pair(
        &bad_link,
        &cfg,
        sync.clone(),
        &slot,
        &access,
        || async {},
        || async {},
    )
    .await;
    assert!(res.is_err());

    let ans = read_answer(&ans_path)
        .unwrap()
        .expect("answer file must exist");
    assert_eq!(ans.confirmed, "");
    assert_eq!(ans.rejected, "");
    assert_eq!(std::fs::read(&state_path).unwrap(), b"invalid json content");

    let (cert, _key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let cred_a = direct_credential(pin, 1);
    let paired_a = PairedState {
        credential: Some(cred_a),
        ..Default::default()
    };
    paired_a.save(&state_path).unwrap();

    let mut uploader_slot = UploaderSlot::new();
    let resumed_access =
        pl_transport_win::service::launch_resume(&cfg, &sync, &mut uploader_slot).await;
    assert!(resumed_access.is_some());
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        sync.lock().unwrap().pairing.phase,
        PairingPhase::AwaitingConfirmation
    );
    assert_eq!(*confirmation.lock().unwrap(), "");
}

/// 2. Same unreadable pairing.json, answer file absent, and pairing-answer.json.tmp pre-created as directory -> returns Err, pairing.json unchanged.
#[tokio::test]
async fn session_pair_answer_tmp_is_dir_returns_error_and_leaves_pairing_json_unchanged() {
    let dir = TempDir::new("pair-ans-tmp-dir");
    let state_path = dir.path().join("pairing.json");
    std::fs::write(&state_path, b"invalid json content").unwrap();

    std::fs::create_dir_all(dir.path().join("pairing-answer.json.tmp")).unwrap();

    let confirmation = Arc::new(Mutex::new(String::new()));
    let tombstone = Arc::new(Mutex::new(None));
    let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
    let jv = Arc::new(JournalVersionController::new(dir.path().join("jv.json")));
    let cfg = SyncConfig {
        device_label: "test".into(),
        period_secs: 300,
        segments_root: dir.path().join("segments"),
        state_path: state_path.clone(),
        local_offset: Arc::new(UtcOffset),
        journal_version: jv,
        facts_fn: Arc::new(pl_transport_win::RawDeviceFacts::default),
        confirmation: confirmation.clone(),
        tombstone: tombstone.clone(),
        #[cfg(feature = "awaiting-hold")]
        awaiting_hold: None,
    };

    let slot = Arc::new(tokio::sync::Mutex::new(UploaderSlot::new()));
    let access = Arc::new(tokio::sync::Mutex::new(None));

    let bad_link = "https://go.solstone.app/p#invalid".to_string();
    let res = pl_transport_win::session::pair(
        &bad_link,
        &cfg,
        sync.clone(),
        &slot,
        &access,
        || async {},
        || async {},
    )
    .await;
    assert!(res.is_err());
    assert_eq!(std::fs::read(&state_path).unwrap(), b"invalid json content");
}

/// 3. Absent answer file, unreadable pairing.json, then pair that returns Ok -> empty answer file exists before saved B is loaded -> launch_resume leaves B AwaitingConfirmation.
#[tokio::test]
async fn session_pair_success_creates_empty_answer_before_saving_credential_and_leaves_awaiting_on_resume(
) {
    let dir = TempDir::new("pair-ok-empty-ans");
    let state_path = dir.path().join("pairing.json");
    let ans_path = answer_path(&state_path);
    std::fs::write(&state_path, b"invalid json content").unwrap();

    let mock_state = Arc::new(support::relay_pairing::MockState::normal().with_same_tls_ca());
    let origin = support::relay_pairing::spawn_mock_relay(mock_state.clone()).await;
    let link = support::relay_pairing::relay_form_link(
        &origin,
        &support::relay_pairing::PAIR_SECRET,
        &mock_state.json_ca.spki_pin(),
    );

    let confirmation = Arc::new(Mutex::new(String::new()));
    let tombstone = Arc::new(Mutex::new(None));
    let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
    let jv = Arc::new(JournalVersionController::new(dir.path().join("jv.json")));
    let cfg = SyncConfig {
        device_label: "test".into(),
        period_secs: 300,
        segments_root: dir.path().join("segments"),
        state_path: state_path.clone(),
        local_offset: Arc::new(UtcOffset),
        journal_version: jv,
        facts_fn: Arc::new(pl_transport_win::RawDeviceFacts::default),
        confirmation: confirmation.clone(),
        tombstone: tombstone.clone(),
        #[cfg(feature = "awaiting-hold")]
        awaiting_hold: None,
    };

    let slot = Arc::new(tokio::sync::Mutex::new(UploaderSlot::new()));
    let access = Arc::new(tokio::sync::Mutex::new(None));

    let res = pl_transport_win::session::pair(
        &link,
        &cfg,
        sync.clone(),
        &slot,
        &access,
        || async {},
        || async {},
    )
    .await;
    assert!(res.is_ok());

    let ans = read_answer(&ans_path)
        .unwrap()
        .expect("answer file must exist");
    assert_eq!(ans.confirmed, "");

    let paired = PairedState::load(&state_path).unwrap();
    assert!(paired.credential.is_some());

    let mut uploader_slot = UploaderSlot::new();
    let resumed_access =
        pl_transport_win::service::launch_resume(&cfg, &sync, &mut uploader_slot).await;
    assert!(resumed_access.is_some());
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        sync.lock().unwrap().pairing.phase,
        PairingPhase::AwaitingConfirmation
    );
    assert_eq!(*confirmation.lock().unwrap(), "");
}

/// 4. Failed re-pair over confirmed A -> answer bytes identical, launch_resume sends for A (gate open).
#[tokio::test]
async fn session_pair_failed_repair_over_confirmed_leaves_answer_bytes_and_sends_for_incumbent() {
    let dir = TempDir::new("pair-failed-repair");
    let state_path = dir.path().join("pairing.json");
    let ans_path = answer_path(&state_path);

    let (cert, _key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let cred_a = direct_credential(pin, 1);
    let digest_a = JournalIdentity::from_credential(&cred_a).client_cert_sha256;
    let paired_a = PairedState {
        credential: Some(cred_a),
        ..Default::default()
    };
    paired_a.save(&state_path).unwrap();

    let ans = AnswerState {
        confirmed: digest_a.clone(),
        rejected: String::new(),
    };
    write_answer(&ans_path, &ans).unwrap();
    let ans_bytes_before = std::fs::read(&ans_path).unwrap();

    let confirmation = Arc::new(Mutex::new(digest_a.clone()));
    let tombstone = Arc::new(Mutex::new(None));
    let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
    let jv = Arc::new(JournalVersionController::new(dir.path().join("jv.json")));
    let cfg = SyncConfig {
        device_label: "test".into(),
        period_secs: 300,
        segments_root: dir.path().join("segments"),
        state_path: state_path.clone(),
        local_offset: Arc::new(UtcOffset),
        journal_version: jv,
        facts_fn: Arc::new(pl_transport_win::RawDeviceFacts::default),
        confirmation: confirmation.clone(),
        tombstone: tombstone.clone(),
        #[cfg(feature = "awaiting-hold")]
        awaiting_hold: None,
    };

    let slot = Arc::new(tokio::sync::Mutex::new(UploaderSlot::new()));
    let access = Arc::new(tokio::sync::Mutex::new(None));

    let bad_link = "https://go.solstone.app/p#invalid".to_string();
    let res = pl_transport_win::session::pair(
        &bad_link,
        &cfg,
        sync.clone(),
        &slot,
        &access,
        || async {},
        || async {},
    )
    .await;
    assert!(res.is_err());

    let ans_bytes_after = std::fs::read(&ans_path).unwrap();
    assert_eq!(ans_bytes_before, ans_bytes_after);

    let mut uploader_slot = UploaderSlot::new();
    let resumed_access =
        pl_transport_win::service::launch_resume(&cfg, &sync, &mut uploader_slot).await;
    assert!(resumed_access.is_some());
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(*confirmation.lock().unwrap(), digest_a);
    assert_eq!(sync.lock().unwrap().pairing.phase, PairingPhase::Paired);
}

/// 5. Unreadable answer file -> session::pair does not rewrite it; phase stays awaiting; bytes unchanged.
#[tokio::test]
async fn session_pair_unreadable_answer_file_returns_error_and_does_not_rewrite() {
    let dir = TempDir::new("pair-unreadable-ans");
    let state_path = dir.path().join("pairing.json");
    let ans_path = answer_path(&state_path);

    let (cert, _key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let cred = direct_credential(pin, 1);
    let paired = PairedState {
        credential: Some(cred),
        ..Default::default()
    };
    paired.save(&state_path).unwrap();

    let unreadable_bytes = b"not valid json {{{";
    std::fs::write(&ans_path, unreadable_bytes).unwrap();

    let confirmation = Arc::new(Mutex::new(String::new()));
    let tombstone = Arc::new(Mutex::new(None));
    let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
    let jv = Arc::new(JournalVersionController::new(dir.path().join("jv.json")));
    let cfg = SyncConfig {
        device_label: "test".into(),
        period_secs: 300,
        segments_root: dir.path().join("segments"),
        state_path: state_path.clone(),
        local_offset: Arc::new(UtcOffset),
        journal_version: jv,
        facts_fn: Arc::new(pl_transport_win::RawDeviceFacts::default),
        confirmation: confirmation.clone(),
        tombstone: tombstone.clone(),
        #[cfg(feature = "awaiting-hold")]
        awaiting_hold: None,
    };

    let slot = Arc::new(tokio::sync::Mutex::new(UploaderSlot::new()));
    let access = Arc::new(tokio::sync::Mutex::new(None));

    let bad_link = "https://go.solstone.app/p#invalid".to_string();
    let res = pl_transport_win::session::pair(
        &bad_link,
        &cfg,
        sync.clone(),
        &slot,
        &access,
        || async {},
        || async {},
    )
    .await;
    assert!(res.is_err());

    let mut uploader_slot = UploaderSlot::new();
    let resumed_access =
        pl_transport_win::service::launch_resume(&cfg, &sync, &mut uploader_slot).await;
    assert!(resumed_access.is_some());
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        sync.lock().unwrap().pairing.phase,
        PairingPhase::AwaitingConfirmation
    );
    assert_eq!(std::fs::read(&ans_path).unwrap(), unreadable_bytes);
}
