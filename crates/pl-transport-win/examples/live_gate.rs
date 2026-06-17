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

use observer_model::SyncSnapshot;
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
    // derived day/segment keys look like a real capture. The screen payload size
    // is env-tunable: the default tiny marker stays inside the 1 MiB initial mux
    // window (the W2 small-segment gate); set `SOLSTONE_FAKE_SCREEN_BYTES` to a
    // multi-MiB value to exercise WINDOW flow-control end-to-end against the real
    // journal — the realistic-payload proof for the encoder arc, decoupled from
    // the encoder itself (an encoded ~37.5 MB segment will far exceed 1 MiB).
    let screen_bytes: usize = std::env::var("SOLSTONE_FAKE_SCREEN_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let screen_payload: Vec<u8> = if screen_bytes > 0 {
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
    std::fs::write(seg_dir.join("screen.bin"), &screen_payload).unwrap();
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
    client.heartbeat(false).await.expect("heartbeat failed");
    println!("HEARTBEAT: ok");

    // 4. Upload + reconcile via the real coordinator.
    let client = Arc::new(client);
    let sync = Arc::new(Mutex::new(SyncSnapshot::default()));
    let store = Box::new(LocalSealedStore::new(&root, PERIOD_SECS));
    let coordinator =
        UploadCoordinator::new(client.clone(), store, sync.clone(), PLATFORM, PERIOD_SECS);
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
