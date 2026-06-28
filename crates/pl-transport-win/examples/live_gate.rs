// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Live cross-repo pair + ingest gate (operator-direct, not part of `make ci`).
//!
//! Drives the real production path against a real `solstone` journal: parse a
//! pair-link, pair over framed-mTLS, register the observer, fabricate one tiny
//! sealed segment, then run a single [`UploadCoordinator`] tick (ingest +
//! reconcile-by-sha256). Prints `LIVE_GATE_PASS` only when the journal confirms
//! the segment landed with matching sha256.
//!
//! Usage: `SOLSTONE_PAIR_LINK='https://go.solstone.app/p#…' cargo run -p
//! pl-transport-win --example live_gate`. Because rustls is cross-platform, this
//! runs on the Linux dev host against the box's own journal — no Windows needed.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use observer_model::{SyncSnapshot, SCREEN_FILE_NAME};
use observer_pl::wire::HeartbeatEvent;
use pl_transport_win::client::ObserverClient;
use pl_transport_win::coordinator::UploadCoordinator;
use pl_transport_win::pairing;
use pl_transport_win::sealed::LocalSealedStore;

const PERIOD_SECS: u64 = 300;
const PLATFORM: &str = "windows";

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let link = std::env::var("SOLSTONE_PAIR_LINK")
        .expect("set SOLSTONE_PAIR_LINK to a fresh pair-link from `pair-start`");
    let device_label =
        std::env::var("SOLSTONE_DEVICE_LABEL").unwrap_or_else(|_| "win-w2-live-test".to_string());

    // Fabricate one sealed segment aligned to the current clock boundary, so the
    // derived day/segment keys look like a real capture. The screen payload is
    // env-tunable:
    //   - `SOLSTONE_REAL_SCREEN_FILE=/path/to.mp4` — upload a REAL captured,
    //     encoded segment (the encoder-arc end-to-end proof: a genuine H.264 mp4
    //     lands in the journal by sha256 and `journal describe` ingests it; a
    //     >1 MiB file also exercises WINDOW flow-control live).
    //   - `SOLSTONE_FAKE_SCREEN_BYTES=N` — N deterministic synthetic bytes.
    //   - default — a tiny marker inside the 1 MiB initial window (W2 gate).
    let screen_bytes: usize = std::env::var("SOLSTONE_FAKE_SCREEN_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let screen_payload: Vec<u8> = if let Ok(path) = std::env::var("SOLSTONE_REAL_SCREEN_FILE") {
        std::fs::read(&path)
            .unwrap_or_else(|e| panic!("read SOLSTONE_REAL_SCREEN_FILE {path}: {e}"))
    } else if screen_bytes > 0 {
        // Deterministic, non-trivial bytes so the journal's sha256 reconcile is a
        // real byte-identity check across the whole multi-MiB body.
        (0..screen_bytes).map(|i| (i % 251) as u8).collect()
    } else {
        b"solstone-windows-w2-live-screen".to_vec()
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let index = now / PERIOD_SECS;
    let root = std::env::temp_dir()
        .join(format!("w2-live-{}", std::process::id()))
        .join("segments");
    let seg_dir = root.join(index.to_string());
    std::fs::create_dir_all(&seg_dir).unwrap();
    std::fs::write(seg_dir.join(SCREEN_FILE_NAME), &screen_payload).unwrap();
    std::fs::write(
        seg_dir.join("system-audio.pcm"),
        b"solstone-windows-w2-live-audio",
    )
    .unwrap();
    println!(
        "fabricated sealed segment at {} (screen payload {} bytes{})",
        seg_dir.display(),
        screen_payload.len(),
        if screen_bytes > 1 << 20 {
            " — exercises WINDOW flow-control"
        } else {
            ""
        }
    );

    // 1. Pair over framed-mTLS (CA-fp pinned).
    let credential = pairing::pair_from_link(&link, &device_label)
        .await
        .expect("pairing failed");
    println!(
        "PAIRED: journal='{}' instance={} endpoints={:?}",
        credential.home_label, credential.instance_id, credential.endpoints
    );

    // 2. Register the observer.
    let mut client = ObserverClient::new(credential).expect("client build failed");
    let registration = client
        .register(
            PLATFORM,
            &device_label,
            "desktop",
            env!("CARGO_PKG_VERSION"),
            None,
        )
        .await
        .expect("register failed");
    println!(
        "REGISTERED: stream='{}' key_prefix={} ingest_url={:?}",
        registration.name,
        &registration.key[..registration.key.len().min(8)],
        registration.ingest_url
    );

    // 3. Heartbeat once.
    client
        .heartbeat(&HeartbeatEvent::status(false))
        .await
        .expect("heartbeat failed");
    println!("HEARTBEAT: ok");

    // 4. Upload + reconcile via the real coordinator.
    let client = Arc::new(client);
    let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
    let store = Box::new(LocalSealedStore::new(&root, PERIOD_SECS));
    let retention = std::sync::Arc::new(std::sync::RwLock::new(
        observer_retention::RetentionConfig::default(),
    ));
    let coordinator = UploadCoordinator::new(
        client.clone(),
        store,
        sync.clone(),
        PLATFORM,
        PERIOD_SECS,
        retention,
    );
    let confirmed = coordinator.tick().await.expect("upload tick failed");
    let snapshot = sync.lock().unwrap().clone();
    println!(
        "UPLOAD: confirmed={} uploaded={} pending={} failed={} last={:?} err={:?}",
        confirmed,
        snapshot.upload.uploaded_segments,
        snapshot.upload.pending_segments,
        snapshot.upload.failed_segments,
        snapshot.upload.last_uploaded_segment,
        snapshot.upload.last_error,
    );

    let _ = std::fs::remove_dir_all(root.parent().unwrap());
    assert!(
        confirmed >= 1 && snapshot.upload.uploaded_segments >= 1,
        "segment did not confirm-land in the journal (reconcile by sha256 failed)"
    );
    println!("LIVE_GATE_PASS");
}
