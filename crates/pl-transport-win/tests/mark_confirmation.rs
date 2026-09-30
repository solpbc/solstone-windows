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
use pl_transport_win::coordinator::TEST_AWAITING_HOLD;
use pl_transport_win::credential::{CasKey, PairedState};
use pl_transport_win::service::{
    publish_pairing, run_uploader, BoundKind, PairingWrite, SyncConfig,
};
use pl_transport_win::session::{answer, PairingAction};
use pl_transport_win::slot::UploaderSlot;
use pl_transport_win::unknown_journals::mark_spec_for_jid;
use pl_transport_win::{CredentialAccess, JournalVersionController};
use spl_core::frame::{Frame, FrameDecoder, FLAG_CLOSE, FLAG_DATA, FLAG_WINDOW, RECOMMENDED_CHUNK};
use spl_core::mux::INITIAL_WINDOW;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use support::journal_fake::{direct_credential, read_framed_request, self_signed, server_config};

static TEST_PATH_COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let unique = TEST_PATH_COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = PathBuf::from("/var/tmp").join(format!(
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
                    credit +=
                        i64::from(u32::from_be_bytes(frame.payload[..4].try_into().unwrap()));
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

/// Acceptance 1: Gated routes do not dial while awaiting confirmation.
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

/// Acceptance 2: Rebuilding access keeps the closed gate until opened.
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

/// Acceptance 3: An awaiting tick does not count as a failure.
#[tokio::test]
async fn mark_confirmation_awaiting_tick_is_not_a_failure() {
    let (cert, _key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let dir = TempDir::new("awaiting-tick");
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
        state_path,
        local_offset: Arc::new(UtcOffset),
        journal_version: jv,
        facts_fn: Arc::new(pl_transport_win::RawDeviceFacts::default),
        confirmation: confirmation.clone(),
    };

    let cred = direct_credential(pin, 1);
    let paired = PairedState {
        credential: Some(cred),
        ..Default::default()
    };
    let access = CredentialAccess::bind(&paired, &cfg, sync.clone(), None).unwrap();

    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(pl_transport_win::SlotExit::Run);
    let wake = Arc::new(tokio::sync::Notify::new());
    let uploader_task = tokio::spawn(run_uploader(access, cfg, sync.clone(), cancel_rx, wake));

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Check snapshot values
    {
        let snap = sync.lock().unwrap();
        assert_eq!(snap.upload.failed_segments, 0);
        assert!(snap.upload.last_error.is_none());
        assert_eq!(snap.upload.last_successful_sync, None);
        assert_eq!(snap.upload.recent_error_count, 0);
    }

    let _ = cancel_tx.send(pl_transport_win::SlotExit::Shutdown);
    let _ = uploader_task.await;
}

/// Acceptance 4: Kicking the slot wakes a 300s backoff immediately.
#[tokio::test]
async fn mark_confirmation_kick_wakes_a_300s_backoff() {
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

    let dir = TempDir::new("kick-backoff");
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

    // Give setup_uploader a moment to run and set snapshot binding
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Confirm via answer()
    tokio::time::timeout(
        Duration::from_secs(2),
        answer(
            PairingAction::Confirm,
            &binding,
            &cfg,
            &sync,
            &access_mutex,
            &slot,
        ),
    )
    .await
    .expect("answer timeout")
    .expect("answer confirm");

    // Upload occurs immediately without waiting 300s
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(upload_received.load(Ordering::SeqCst));

    server_task.abort();
}

/// Acceptance 5: Kicking under a paused clock wakes the loop and completes upload.
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
    let cfg = SyncConfig {
        device_label: "test".into(),
        period_secs: 300,
        segments_root: segments_dir,
        state_path: state_path.clone(),
        local_offset: Arc::new(UtcOffset),
        journal_version: jv,
        facts_fn: Arc::new(pl_transport_win::RawDeviceFacts::default),
        confirmation: confirmation.clone(),
    };

    let cred = direct_credential(pin, port);
    let binding = JournalIdentity::from_credential(&cred).client_cert_sha256;
    let paired = PairedState {
        credential: Some(cred),
        ..Default::default()
    };
    paired.save(&state_path).unwrap();

    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    *TEST_AWAITING_HOLD.lock().unwrap() = Some((entered.clone(), release.clone()));

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

    // Wait until coordinator tick enters the awaiting hold
    entered.notified().await;

    // Confirm while held
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

/// Acceptance 6: Reject deletes the DER ID and cleans up state.
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

    let delete_rec = delete_received.clone();
    let slot_clone = slot.clone();
    let lock_held = lock_held_during_delete.clone();

    let server_task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            let delete_rec = delete_rec.clone();
            let slot_clone = slot_clone.clone();
            let lock_held = lock_held.clone();
            tokio::spawn(async move {
                if let Ok(mut tls) = acceptor.accept(stream).await {
                    let (stream_id, req) = read_framed_request(&mut tls).await;
                    let req_str = String::from_utf8_lossy(&req);
                    if req_str.contains("/app/network/api/clients/") {
                        // Check if uploader slot is held during delete
                        if slot_clone.try_lock().is_err() {
                            lock_held.store(true, Ordering::SeqCst);
                        }
                        delete_rec.lock().unwrap().push(req_str.to_string());
                    }
                    let resp = b"HTTP/1.1 204 No Content\r\n\r\n".to_vec();
                    write_response(&mut tls, stream_id, resp).await;
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
    };

    let cred = direct_credential(pin, port);
    let binding = JournalIdentity::from_credential(&cred).client_cert_sha256;
    let paired = PairedState {
        credential: Some(cred.clone()),
        ..Default::default()
    };
    paired.save(&state_path).unwrap();

    let access = CredentialAccess::bind(&paired, &cfg, sync.clone(), None).unwrap();
    let access_mutex = Arc::new(tokio::sync::Mutex::new(Some(access.clone())));

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
    let der_certs = spl_transport::tls::parse_certs(&cred.client_cert_pem).unwrap();
    let expected_client_id = format!(
        "sha256:{}",
        spl_core::ca::sha256_hex(der_certs[0].as_ref())
    );
    assert!(delete_req.contains(&expected_client_id));

    // pairing.json deleted, segments remain, phase is NotPaired(MARK_REJECTED_DETAIL)
    assert!(!state_path.exists());
    assert!(day_dir.join("000000_300.tar.gz").exists());
    {
        let snap = sync.lock().unwrap();
        assert_eq!(snap.pairing.phase, PairingPhase::NotPaired);
        assert_eq!(snap.pairing.detail.as_deref(), Some(MARK_REJECTED_DETAIL));
    }

    server_task.abort();
}

/// Acceptance 7: Stale answer changes nothing.
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

/// Acceptance 8: Fresh pairing publishes AwaitingConfirmation before bind.
#[tokio::test]
async fn mark_confirmation_pair_publishes_awaiting_before_bind() {
    let (cert, _key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let _dir = TempDir::new("pair-publish");
    let confirmation = Arc::new(Mutex::new(String::new()));
    let sync = Arc::new(Mutex::new(SyncSnapshot::default()));

    let cred = direct_credential(pin, 1);
    let binding = JournalIdentity::from_credential(&cred).client_cert_sha256;

    // Publish bound with empty confirmation
    publish_pairing(
        &sync,
        &confirmation,
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

/// Acceptance 9: Launch finish precedes grandfather.
#[tokio::test]
async fn mark_confirmation_launch_finish_precedes_grandfather() {
    let (cert, _key) = self_signed();
    let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
    let dir = TempDir::new("launch-finish");
    let state_path = dir.path().join("pairing.json");
    let ans_path = answer_path(&state_path);
    let confirmation = Arc::new(Mutex::new(String::new()));
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

    let skip_resume = settle_rejected_on_launch(&state_path, &confirmation, &sync).await;
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

/// Acceptance 10: Pair mark gates the save in integration ops.
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
    assert!(!env.state_tmp_path().exists(), "pairing.json.tmp must be absent");
    assert!(!answer_path(&env.state_path).exists(), "answer file must be unchanged");
    assert_eq!(delete_paths.lock().unwrap().len(), 1, "exactly one DELETE request");
    let delete_path = delete_paths.lock().unwrap()[0].clone();
    assert!(
        delete_path.starts_with("/app/network/api/clients/sha256:"),
        "DELETE path must be /app/network/api/clients/sha256:..., got {delete_path}"
    );
    assert_eq!(
        evidence.remote_residue.unwrap().journal_pairing_identity,
        pl_transport_win::integration::report::Residue::None,
        "journal_pairing_identity must be None on 404 delete"
    );

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
    let ans_state = read_answer(&ans_path).unwrap().expect("answer state must exist");
    let paired = PairedState::load(&env.state_path).unwrap();
    let cred = paired.credential.as_ref().unwrap();
    let digest = JournalIdentity::from_credential(cred).client_cert_sha256;
    assert_eq!(ans_state.confirmed, digest);

    // 5. Client built by client_for can ingest (gate is open)
    let client = pl_transport_win::integration::ops::client_for(&env, &paired, None).expect("client_for");
    assert!(client.gate_open(), "gate must be open for confirmed pairing");
    let (response, _) = client
        .ingest("000000_300", "20260930", test_file_parts(), None)
        .await
        .expect("ingest must succeed");
    assert_eq!(response.status, observer_pl::ingest::IngestStatus::Ok);

    direct_task.abort();
}

/// Acceptance 11: Metadata routes dial while awaiting confirmation.
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

