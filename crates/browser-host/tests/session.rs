// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The native-host protocol end to end over in-memory streams: relay ↔ hub,
//! the capture gate, dedup, quiescence and a journal switch.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

use browser_host::argv::{BrandHint, Invocation, Mode};
use browser_host::custody::{OutboxEntry, Policy, Store};
use browser_host::hub::{Gates, Hub, HubConfig, Pairing};
use browser_host::relay::{self, RelayEnd};
use browser_host::upload::{deliver_pending, Journal, UploadOutcome};
use browser_host::wire::{write_frame, FrameReader};
use native_browser_frame::{decode, encode, DecodeOutcome, Direction};
use observer_model::about::NativeAboutSnapshot;
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncWrite, DuplexStream};

const T0: u64 = 1_790_000_100_000;
const JOURNAL_A: &str = "sha256:a";
const JOURNAL_B: &str = "sha256:b";

struct Rig {
    hub: Arc<Hub>,
    clock: Arc<AtomicU64>,
    _dir: tempfile::TempDir,
}

fn rig(development: bool) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(AtomicU64::new(T0));
    let c = Arc::clone(&clock);
    let store = Store::open(
        dir.path(),
        Policy::default(),
        Box::new(|s, l| (format!("d{s}"), format!("s{s}_{l}"))),
        T0,
    );
    let hub = Hub::new(
        HubConfig {
            development,
            app_version: "2.0.16".into(),
            // Even if startup has a cached journal line, the first host
            // publication is unknown until the destination gate is refreshed.
            about: NativeAboutSnapshot::new(
                "windows",
                "11 26100",
                "arm64",
                "journal 2.0.16",
                true,
                Some(1_700_000_000),
            )
            .unwrap(),
        },
        store,
        Box::new(move || c.load(Ordering::SeqCst)),
    );
    Rig {
        hub,
        clock,
        _dir: dir,
    }
}

fn paired(identity: &str) -> Gates {
    Gates {
        pairing: Pairing::Paired {
            identity: Some(identity.into()),
        },
        paused: false,
    }
}

/// A raw client speaking the relay's side of the pipe.
struct Client {
    r: FrameReader<tokio::io::ReadHalf<DuplexStream>>,
    w: tokio::io::WriteHalf<DuplexStream>,
}

impl Client {
    async fn send(&mut self, v: &Value) {
        let bytes = if v["type"] == "local_hello" {
            serde_json::to_vec(v).unwrap()
        } else {
            encode(v).unwrap()
        };
        write_frame(&mut self.w, Direction::ExtensionToHost, &bytes)
            .await
            .unwrap();
    }

    async fn recv(&mut self) -> Option<Value> {
        let frame = tokio::time::timeout(Duration::from_secs(30), self.r.next())
            .await
            .expect("reply in time")
            .ok()??;
        Some(serde_json::from_slice(&frame).unwrap())
    }

    /// Next message that is not a periodic `state`.
    async fn recv_skipping_state(&mut self) -> Option<Value> {
        loop {
            let v = self.recv().await?;
            if v["type"] != "state" {
                return Some(v);
            }
        }
    }
}

fn connect(hub: &Arc<Hub>) -> Client {
    let (a, b) = tokio::io::duplex(1 << 20);
    tokio::spawn(Arc::clone(hub).serve(b));
    let (r, w) = tokio::io::split(a);
    Client {
        r: FrameReader::new(r, Direction::HostToExtension),
        w,
    }
}

fn hello(brand: &str, inst: &str) -> Value {
    json!({"type": "hello", "protocol": 1, "version": "0.2.0", "brand": brand, "inst": inst})
}

fn local(brand: &str, mode: &str) -> Value {
    json!({"type": "local_hello", "brand": brand, "mode": mode})
}

fn batch(generation: &str, batch_id: u32, records: Value) -> Value {
    json!({
        "type": "batch",
        "destination_generation": generation,
        "inst": "inst-1",
        "batch_id": format!("{batch_id:032x}"),
        "queued_at_ms": T0,
        "records": records,
    })
}

fn snapshot(ctx: &str, text: &str) -> Value {
    json!([{"t": "segment_start", "ts": T0, "ctx": ctx, "site": "example.com", "blocks": [{"id": "b1", "text": text}]}])
}

async fn handshake(hub: &Arc<Hub>, brand: &str, mode: &str) -> (Client, Value) {
    let mut c = connect(hub);
    c.send(&local(
        if brand == "firefox" {
            "firefox"
        } else {
            "chromium"
        },
        mode,
    ))
    .await;
    c.send(&hello(brand, "inst-1")).await;
    let ack = c.recv().await.expect("hello_ack");
    (c, ack)
}

#[tokio::test]
async fn a_paired_app_permits_capture_accepts_and_dedups() {
    let rig = rig(false);
    rig.hub.update_gates(paired(JOURNAL_A));
    let (mut c, ack) = handshake(&rig.hub, "chrome", "production").await;
    assert_eq!(ack["type"], "hello_ack");
    assert_eq!(ack["capture"], "permitted");
    assert_eq!(ack["delivery"], "idle");
    assert_eq!(ack["freshness_ms"], 15000);
    let about = NativeAboutSnapshot::from_value(&ack["about"]).unwrap();
    assert_eq!(about.journal_line, "journal unknown");
    assert!(!about.journal_current);
    assert_eq!(about.journal_seen_at_epoch_secs, None);
    let encoded = encode(&ack).unwrap();
    assert!(matches!(
        decode(&encoded, Direction::HostToExtension),
        DecodeOutcome::Accept(_)
    ));
    let generation = ack["destination_generation"].as_str().unwrap().to_string();
    let period = ack["period_id"].as_str().unwrap().to_string();

    c.send(&batch(&generation, 1, snapshot("c1", "hello")))
        .await;
    let reply = c.recv_skipping_state().await.unwrap();
    assert_eq!(reply["result"], "accepted");
    assert_eq!(reply["period_id"], period.as_str());
    assert_eq!(reply["destination_generation"], generation);

    c.send(&batch(&generation, 1, snapshot("c1", "hello")))
        .await;
    let reply = c.recv_skipping_state().await.unwrap();
    assert_eq!(reply["result"], "duplicate");
    assert_eq!(reply["period_id"], period.as_str());
    assert_eq!(reply["destination_generation"], generation);
    assert_eq!(rig.hub.status().delivery, "kept_locally");
}

#[tokio::test]
async fn about_snapshot_updates_publish_once_and_destination_change_resets_first() {
    let rig = rig(false);
    rig.hub.update_gates(paired(JOURNAL_A));
    let (mut client, ack) = handshake(&rig.hub, "chrome", "production").await;
    assert_eq!(ack["about"]["journal_line"], "journal unknown");
    let old_generation = ack["destination_generation"].as_str().unwrap().to_string();
    client
        .send(&batch(
            &old_generation,
            90,
            snapshot("c1", "pending across destination change"),
        ))
        .await;
    assert_eq!(
        client.recv_skipping_state().await.unwrap()["result"],
        "accepted"
    );
    let pending = rig.hub.status().custody.held_bytes;
    assert!(pending > 0);

    let about = NativeAboutSnapshot::new(
        "windows",
        "11 26100",
        "arm64",
        "journal 1.2.3 · ubuntu 24.04 · x86_64",
        true,
        Some(1_700_000_000),
    )
    .unwrap();
    rig.hub.update_about_snapshot(about.clone());
    let state = loop {
        let state = client.recv().await.unwrap();
        if state["about"]["journal_line"] != "journal unknown" {
            break state;
        }
    };
    assert_eq!(state["type"], "state");
    assert_eq!(state["about"]["journal_line"], about.journal_line);
    assert_eq!(state["about"]["journal_current"], true);
    assert_eq!(state["about"].as_object().unwrap().len(), 7);

    let newer = NativeAboutSnapshot {
        journal_seen_at_epoch_secs: Some(1_700_000_001),
        ..about.clone()
    };
    rig.hub.update_about_snapshot(newer);
    assert_eq!(
        client.recv().await.unwrap()["about"]["journal_seen_at_epoch_secs"],
        1_700_000_001
    );
    rig.hub.update_about_snapshot(NativeAboutSnapshot {
        journal_seen_at_epoch_secs: Some(1_700_000_001),
        ..about
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), client.recv())
            .await
            .is_err()
    );

    rig.hub.update_gates(paired(JOURNAL_B));
    let changed = client.recv().await.unwrap();
    assert_eq!(changed["type"], "state");
    assert_ne!(changed["destination_generation"], old_generation);
    assert_eq!(changed["about"]["journal_line"], "journal unknown");
    assert_eq!(rig.hub.status().custody.held_bytes, pending);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), client.recv())
            .await
            .is_err()
    );
    let (_replacement, ack) = handshake(&rig.hub, "chrome", "production").await;
    assert_eq!(ack["about"]["journal_line"], "journal unknown");
    assert_eq!(ack["about"]["journal_current"], false);
    assert!(ack["about"]["journal_seen_at_epoch_secs"].is_null());
    assert_ne!(ack["destination_generation"], old_generation);
}

#[test]
fn unavailable_ack_carries_a_closed_unknown_snapshot() {
    let ack = relay::unavailable_ack();
    let about = NativeAboutSnapshot::from_value(&ack["about"]).unwrap();
    assert_eq!(about.journal_line, "journal unknown");
    assert_eq!(about.journal_seen_at_epoch_secs, None);
    assert_eq!(ack["about"].as_object().unwrap().len(), 7);
    let bytes = encode(&ack).unwrap();
    assert!(matches!(
        decode(&bytes, Direction::HostToExtension),
        DecodeOutcome::Accept(_)
    ));
}

#[test]
fn future_envelope_keys_round_trip_while_about_projection_stays_closed() {
    let native: Value = serde_json::from_str(include_str!(
        "../../../contracts/solstone-core-about/bundle/native-about.json"
    ))
    .unwrap();
    let future = native["envelopes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|envelope| envelope.get("future_root").is_some())
        .unwrap();
    let encoded = encode(future).unwrap();
    let text = String::from_utf8(encoded.clone()).unwrap();
    assert!(text.find("\"about\"").unwrap() > text.find("\"period_id\"").unwrap());
    let DecodeOutcome::Accept(decoded) = decode(&encoded, Direction::HostToExtension) else {
        panic!("future envelope root key should remain extensible");
    };
    NativeAboutSnapshot::from_value(&decoded["about"]).unwrap();

    let nested_extra = native["envelopes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|envelope| envelope["about"].get("hostname").is_some())
        .unwrap();
    let encoded = encode(nested_extra).unwrap();
    let DecodeOutcome::Accept(decoded) = decode(&encoded, Direction::HostToExtension) else {
        panic!("core envelope should accept an opaque about object");
    };
    assert!(NativeAboutSnapshot::from_value(&decoded["about"]).is_err());
}

#[tokio::test]
async fn unpaired_and_paused_close_the_gate_and_resume_reopens_it() {
    let rig = rig(false);
    let (mut c, ack) = handshake(&rig.hub, "edge", "production").await;
    assert_eq!(ack["capture"], "not_paired");
    assert!(ack["destination_generation"].is_null() && ack["period_id"].is_null());
    c.send(&batch("whatever", 1, snapshot("c1", "x"))).await;
    let reply = c.recv_skipping_state().await.unwrap();
    assert_eq!(reply["result"], "rejected");
    assert_eq!(reply["reason"], "resource_exhausted");
    assert_eq!(reply["class"], "retryable");
    assert_eq!(reply["destination_generation"], "whatever");

    rig.hub.update_gates(paired(JOURNAL_A));
    assert_eq!(c.recv().await.unwrap()["capture"], "permitted");
    rig.hub.update_gates(Gates {
        paused: true,
        ..paired(JOURNAL_A)
    });
    assert_eq!(c.recv().await.unwrap()["capture"], "paused");
    rig.hub.update_gates(paired(JOURNAL_A));
    assert_eq!(c.recv().await.unwrap()["capture"], "permitted");
}

#[tokio::test]
async fn development_ids_and_brand_mismatches_are_refused() {
    let rig = rig(false);
    rig.hub.update_gates(paired(JOURNAL_A));
    let mut c = connect(&rig.hub);
    c.send(&local("chromium", "development")).await;
    c.send(&hello("chrome", "inst-1")).await;
    assert!(c.recv().await.is_none(), "dev mode closes without a reply");

    let mut c = connect(&rig.hub);
    c.send(&local("firefox", "production")).await;
    c.send(&hello("chrome", "inst-1")).await;
    assert!(
        c.recv().await.is_none(),
        "brand must match the launch shape"
    );

    let dev = rig_dev_paired();
    let (_c, ack) = handshake(&dev.hub, "firefox", "development").await;
    assert_eq!(ack["capture"], "permitted");
}

fn rig_dev_paired() -> Rig {
    let r = rig(true);
    r.hub.update_gates(paired(JOURNAL_A));
    r
}

#[tokio::test]
async fn an_incompatible_protocol_names_which_side_is_behind() {
    let rig = rig(false);
    let mut c = connect(&rig.hub);
    c.send(&local("chromium", "production")).await;
    c.send(&json!({"type": "hello", "protocol": 2, "version": "9.0.0", "brand": "chrome", "inst": "i"}))
        .await;
    let reply = c.recv().await.unwrap();
    assert_eq!(reply["type"], "unsupported");
    assert_eq!(reply["behind"], "app");
    assert!(c.recv().await.is_none());
}

#[tokio::test]
async fn the_clock_boundary_is_announced() {
    let rig = rig(false);
    rig.hub.update_gates(paired(JOURNAL_A));
    let (mut c, ack) = handshake(&rig.hub, "chrome", "production").await;
    let p0 = ack["period_id"].as_str().unwrap().to_string();
    rig.clock.store(T0 + 300_000, Ordering::SeqCst);
    rig.hub.tick();
    let b = c.recv_skipping_state().await.unwrap();
    assert_eq!(b["type"], "boundary");
    assert_ne!(b["period_id"], p0.as_str());
}

#[tokio::test]
async fn update_quiescence_says_bye_waits_for_hosts_and_refuses_new_ones() {
    let rig = rig(false);
    rig.hub.update_gates(paired(JOURNAL_A));
    let (mut c1, _) = handshake(&rig.hub, "chrome", "production").await;
    let (mut c2, _) = handshake(&rig.hub, "firefox", "production").await;
    assert_eq!(rig.hub.session_count(), 2);

    let hub = Arc::clone(&rig.hub);
    let quiesce = tokio::spawn(async move { hub.quiesce(Duration::from_secs(5)).await });
    for c in [&mut c1, &mut c2] {
        let bye = c.recv_skipping_state().await.unwrap();
        assert_eq!(bye, json!({"type": "bye", "reason": "update"}));
        assert!(c.recv().await.is_none());
    }
    assert!(quiesce.await.unwrap(), "every host left");
    let mut late = connect(&rig.hub);
    late.send(&local("chromium", "production")).await;
    assert!(late.recv().await.is_none(), "no session during quiescence");

    rig.hub.resume();
    let (_c, ack) = handshake(&rig.hub, "chrome", "production").await;
    assert_eq!(ack["capture"], "permitted");
}

#[tokio::test]
async fn a_second_session_for_the_same_browser_replaces_the_first() {
    let rig = rig(false);
    rig.hub.update_gates(paired(JOURNAL_A));
    let (mut old, _) = handshake(&rig.hub, "chrome", "production").await;
    let (_new, _) = handshake(&rig.hub, "chrome", "production").await;
    let bye = old.recv_skipping_state().await.unwrap();
    assert_eq!(bye["reason"], "replaced");
    assert_eq!(rig.hub.session_count(), 1);
}

/// What a fake journal received: `(day, segment, body)`.
type Received = Arc<Mutex<Vec<(String, String, Vec<u8>)>>>;

#[derive(Clone)]
struct FakeJournal {
    identity: String,
    connection: Arc<()>,
    received: Received,
    outcome: UploadOutcome,
    block: Option<(Arc<Notify>, Arc<Notify>)>,
    assert_failure: Option<Arc<Hub>>,
}

impl Journal for FakeJournal {
    fn same_connection(&self, current: &Self) -> bool {
        Arc::ptr_eq(&self.connection, &current.connection)
    }

    async fn upload(&self, entry: &OutboxEntry, body: Vec<u8>) -> UploadOutcome {
        if let Some((entered, release)) = &self.block {
            entered.notify_one();
            release.notified().await;
        }
        if self.outcome == UploadOutcome::Held {
            if let Some(hub) = &self.assert_failure {
                assert_eq!(hub.status().failure, Some("relay_unavailable"));
            }
        }
        if self.outcome == UploadOutcome::Delivered {
            self.received
                .lock()
                .unwrap()
                .push((entry.day.clone(), entry.segment.clone(), body));
        }
        self.outcome
    }
}

fn journal(identity: &str) -> FakeJournal {
    FakeJournal {
        identity: identity.into(),
        connection: Arc::new(()),
        received: Arc::new(Mutex::new(Vec::new())),
        outcome: UploadOutcome::Delivered,
        block: None,
        assert_failure: None,
    }
}

#[tokio::test]
async fn a_held_mark_keeps_text_locally_until_sending_opens() {
    let rig = rig(false);
    rig.hub.update_gates(paired(JOURNAL_A));
    let (mut c, ack) = handshake(&rig.hub, "chrome", "production").await;
    let generation = ack["destination_generation"].as_str().unwrap().to_string();
    c.send(&batch(&generation, 1, snapshot("c1", "WAITING_ON_MARK")))
        .await;
    c.recv_skipping_state().await;
    rig.clock.store(T0 + 300_000, Ordering::SeqCst);
    rig.hub.tick();

    let held = FakeJournal {
        outcome: UploadOutcome::Held,
        ..journal(JOURNAL_A)
    };
    let pass = deliver_pending(&rig.hub, || async { Some(held.clone()) }).await;
    assert_eq!(pass.delivered, 0);
    assert_eq!(rig.hub.status().delivery, "kept_locally");

    let open = journal(JOURNAL_A);
    let pass = deliver_pending(&rig.hub, || async { Some(open.clone()) }).await;
    assert_eq!(pass.delivered, 1);
    let got = open.received.lock().unwrap();
    assert!(String::from_utf8_lossy(&got[0].2).contains("WAITING_ON_MARK"));
    assert_eq!(rig.hub.status().delivery, "idle");
}

#[tokio::test]
async fn same_length_corruption_never_reaches_the_journal() {
    let rig = rig(false);
    rig.hub.update_gates(paired(JOURNAL_A));
    let (mut c, ack) = handshake(&rig.hub, "chrome", "production").await;
    let generation = ack["destination_generation"].as_str().unwrap().to_string();
    c.send(&batch(&generation, 1, snapshot("c1", "ORIGINAL_TEXT")))
        .await;
    c.recv_skipping_state().await;
    rig.clock.store(T0 + 300_000, Ordering::SeqCst);
    rig.hub.tick();
    let entry = rig.hub.outbox().pop().unwrap();
    let mut bytes = std::fs::read(entry.pages_path()).unwrap();
    let i = bytes
        .windows(13)
        .position(|v| v == b"ORIGINAL_TEXT")
        .unwrap();
    bytes[i..i + 13].copy_from_slice(b"MODIFIED_TEXT");
    std::fs::write(entry.pages_path(), bytes).unwrap();
    let open = journal(JOURNAL_A);
    let pass = deliver_pending(&rig.hub, || async { Some(open.clone()) }).await;
    assert_eq!(pass.delivered, 0);
    assert!(open.received.lock().unwrap().is_empty());
    assert_eq!(rig.hub.status().failure, Some("local_io"));
    assert!(entry.pages_path().exists());
    // A rejected local payload releases its upload reservation for retry.
    assert!(rig.hub.reserve(&entry));
    rig.hub.release(&entry);
}

#[tokio::test]
async fn a_failed_upload_reports_failed_and_keeps_the_period() {
    let rig = rig(false);
    rig.hub.update_gates(paired(JOURNAL_A));
    let (mut c, ack) = handshake(&rig.hub, "chrome", "production").await;
    let generation = ack["destination_generation"].as_str().unwrap().to_string();
    c.send(&batch(&generation, 1, snapshot("c1", "x"))).await;
    c.recv_skipping_state().await;
    rig.clock.store(T0 + 300_000, Ordering::SeqCst);
    rig.hub.tick();
    let failing = FakeJournal {
        outcome: UploadOutcome::Failed("relay_unavailable"),
        ..journal(JOURNAL_A)
    };
    deliver_pending(&rig.hub, || async { Some(failing.clone()) }).await;
    let st = rig.hub.status();
    assert_eq!(
        (st.delivery, st.failure),
        ("failed", Some("relay_unavailable"))
    );
    assert_eq!(rig.hub.outbox().len(), 1);
}

#[tokio::test]
async fn pending_text_survives_unpair_and_is_sent_to_the_confirmed_journal() {
    let rig = rig(false);
    rig.hub.update_gates(paired(JOURNAL_A));
    let (mut client, ack) = handshake(&rig.hub, "chrome", "production").await;
    let generation = ack["destination_generation"].as_str().unwrap().to_string();
    client
        .send(&batch(&generation, 1, snapshot("c1", "PENDING_FROM_A")))
        .await;
    assert_eq!(
        client.recv_skipping_state().await.unwrap()["result"],
        "accepted"
    );
    rig.hub.update_gates(Gates::default());
    rig.clock.store(T0 + 700_000, Ordering::SeqCst);
    rig.hub.tick();
    let old_bytes = rig.hub.outbox()[0].size;
    rig.hub.update_gates(paired(JOURNAL_B));
    assert_eq!(rig.hub.status().custody.held_bytes, old_bytes);
    let mut b = journal(JOURNAL_B);
    b.outcome = UploadOutcome::Held;
    assert_eq!(
        deliver_pending(&rig.hub, || async { Some(b.clone()) })
            .await
            .delivered,
        0
    );
    assert!(b.received.lock().unwrap().is_empty());
    b.outcome = UploadOutcome::Delivered;
    assert_eq!(
        deliver_pending(&rig.hub, || async { Some(b.clone()) })
            .await
            .delivered,
        1
    );
    let got = b.received.lock().unwrap();
    assert_eq!(got.len(), 1);
    assert!(String::from_utf8_lossy(&got[0].2).contains("PENDING_FROM_A"));
}

// --- the relay ---------------------------------------------------------------

fn invocation() -> Invocation {
    Invocation {
        brand: BrandHint::Chromium,
        mode: Mode::Production,
    }
}

async fn browser_side<S: AsyncRead + AsyncWrite + Unpin>(
    s: S,
) -> (FrameReader<tokio::io::ReadHalf<S>>, tokio::io::WriteHalf<S>) {
    let (r, w) = tokio::io::split(s);
    (FrameReader::new(r, Direction::HostToExtension), w)
}

#[tokio::test]
async fn with_no_app_the_relay_reports_unavailable_and_exits() {
    let (stdin_ours, _stdin_browser) = tokio::io::duplex(1024);
    let (stdout_ours, stdout_browser) = tokio::io::duplex(1024);
    let end = relay::run::<_, _, DuplexStream>(invocation(), stdin_ours, stdout_ours, None).await;
    assert_eq!(end, RelayEnd::AppAbsent);
    let (mut r, _w) = browser_side(stdout_browser).await;
    let ack: Value = serde_json::from_slice(&r.next().await.unwrap().unwrap()).unwrap();
    assert_eq!(ack["capture"], "unavailable");
    assert_eq!(ack["type"], "hello_ack");
}

#[tokio::test]
async fn the_relay_carries_a_whole_session_and_stops_on_bye() {
    let rig = rig(false);
    rig.hub.update_gates(paired(JOURNAL_A));
    let (app_end, relay_end) = tokio::io::duplex(1 << 20);
    tokio::spawn(Arc::clone(&rig.hub).serve(app_end));

    // The browser's stdio, as two pipes.
    let (stdin_relay, stdin_browser) = tokio::io::duplex(1 << 20);
    let (stdout_relay, stdout_browser) = tokio::io::duplex(1 << 20);
    let relay_task = tokio::spawn(relay::run(
        invocation(),
        stdin_relay,
        stdout_relay,
        Some(relay_end),
    ));

    let (_, mut to_relay) = tokio::io::split(stdin_browser);
    let (mut from_relay, _) = browser_side(stdout_browser).await;
    write_frame(
        &mut to_relay,
        Direction::ExtensionToHost,
        &encode(&hello("chrome", "inst-1")).unwrap(),
    )
    .await
    .unwrap();
    let ack: Value = serde_json::from_slice(&from_relay.next().await.unwrap().unwrap()).unwrap();
    assert_eq!(ack["capture"], "permitted");
    let generation = ack["destination_generation"].as_str().unwrap().to_string();
    write_frame(
        &mut to_relay,
        Direction::ExtensionToHost,
        &encode(&batch(&generation, 9, snapshot("c1", "Café 漢字"))).unwrap(),
    )
    .await
    .unwrap();
    loop {
        let v: Value = serde_json::from_slice(&from_relay.next().await.unwrap().unwrap()).unwrap();
        if v["type"] == "accepted" {
            assert_eq!(v["result"], "accepted");
            break;
        }
    }
    let hub = Arc::clone(&rig.hub);
    let q = tokio::spawn(async move { hub.quiesce(Duration::from_secs(5)).await });
    loop {
        let v: Value = serde_json::from_slice(&from_relay.next().await.unwrap().unwrap()).unwrap();
        if v["type"] == "bye" {
            assert_eq!(v["reason"], "update");
            break;
        }
    }
    assert_eq!(relay_task.await.unwrap(), RelayEnd::Finished);
    assert!(q.await.unwrap());
}

#[tokio::test(start_paused = true)]
async fn the_relay_gives_up_when_the_extension_says_nothing() {
    let rig = rig(false);
    let (app_end, relay_end) = tokio::io::duplex(1 << 20);
    tokio::spawn(Arc::clone(&rig.hub).serve(app_end));
    let (stdin_relay, _stdin_browser) = tokio::io::duplex(1024);
    let (stdout_relay, _stdout_browser) = tokio::io::duplex(1024);
    let end = relay::run(invocation(), stdin_relay, stdout_relay, Some(relay_end)).await;
    assert_eq!(end, RelayEnd::HandshakeTimeout);
}

#[tokio::test]
async fn the_updater_process_can_ask_for_quiescence_over_the_pipe() {
    let rig = rig(false);
    rig.hub.update_gates(paired(JOURNAL_A));
    let (mut c, _) = handshake(&rig.hub, "edge", "production").await;
    let (app_end, updater_end) = tokio::io::duplex(1 << 16);
    tokio::spawn(Arc::clone(&rig.hub).serve(app_end));
    let ask = tokio::spawn(relay::request_quiesce(updater_end));
    assert_eq!(c.recv_skipping_state().await.unwrap()["reason"], "update");
    assert!(c.recv().await.is_none());
    assert_eq!(ask.await.unwrap(), Some(true));
    assert!(rig.hub.is_quiescing());
}

#[tokio::test]
async fn a_failure_is_not_reported_once_nothing_is_held() {
    let rig = rig(false);
    rig.hub.update_gates(paired(JOURNAL_A));
    let (mut c, ack) = handshake(&rig.hub, "chrome", "production").await;
    let generation = ack["destination_generation"].as_str().unwrap().to_string();
    c.send(&batch(&generation, 1, snapshot("c1", "x"))).await;
    c.recv_skipping_state().await;
    rig.clock.store(T0 + 300_000, Ordering::SeqCst);
    rig.hub.tick();
    let failing = FakeJournal {
        outcome: UploadOutcome::Failed("relay_unavailable"),
        ..journal(JOURNAL_A)
    };
    deliver_pending(&rig.hub, || async { Some(failing.clone()) }).await;
    assert_eq!(rig.hub.status().delivery, "failed");
    // Destination change clears the old failure while the pending bytes remain.
    rig.hub.update_gates(paired(JOURNAL_B));
    let st = rig.hub.status();
    assert_eq!((st.delivery, st.failure), ("kept_locally", None));
    assert_eq!(rig.hub.outbox().len(), 1);
}

#[tokio::test]
async fn a_pause_outranks_a_full_spool() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(
        dir.path(),
        Policy {
            spool_bytes: 2000,
            ..Policy::default()
        },
        Box::new(|s, l| (format!("d{s}"), format!("s{s}_{l}"))),
        T0,
    );
    let hub = Hub::new(
        HubConfig {
            development: false,
            app_version: "t".into(),
            about: NativeAboutSnapshot::unknown("windows", "", ""),
        },
        store,
        Box::new(|| T0),
    );
    hub.update_gates(paired(JOURNAL_A));
    let (mut c, ack) = handshake(&hub, "chrome", "production").await;
    let g = ack["destination_generation"].as_str().unwrap().to_string();
    c.send(&batch(&g, 1, snapshot("c1", &"x".repeat(150))))
        .await;
    assert_eq!(c.recv_skipping_state().await.unwrap()["result"], "accepted");
    c.send(&batch(&g, 2, snapshot("c2", &"y".repeat(150))))
        .await;
    assert_eq!(
        c.recv_skipping_state().await.unwrap()["reason"],
        "queue_full"
    );
    assert_eq!(hub.status().capture, "intake_off");
    assert_eq!(hub.status().failure, Some("queue_full"));
    hub.update_gates(Gates {
        paused: true,
        ..paired(JOURNAL_A)
    });
    assert_eq!(hub.status().capture, "paused");
}

/// A journal view whose upload switches the app to another journal mid-flight.
#[derive(Clone)]
struct SwitchingJournal {
    hub: Arc<Hub>,
    connection: Arc<()>,
    current: Arc<Mutex<Arc<()>>>,
}

impl Journal for SwitchingJournal {
    fn same_connection(&self, current: &Self) -> bool {
        Arc::ptr_eq(&self.connection, &current.connection)
    }

    async fn upload(&self, _entry: &OutboxEntry, _body: Vec<u8>) -> UploadOutcome {
        self.hub.update_gates(paired(JOURNAL_B));
        *self.current.lock().unwrap() = Arc::new(());
        UploadOutcome::Failed("relay_unavailable")
    }
}

#[tokio::test]
async fn an_old_journals_failure_finishing_after_a_switch_is_not_reported() {
    let rig = rig(false);
    rig.hub.update_gates(paired(JOURNAL_A));
    let (mut c, ack) = handshake(&rig.hub, "chrome", "production").await;
    let generation = ack["destination_generation"].as_str().unwrap().to_string();
    c.send(&batch(&generation, 1, snapshot("c1", "x"))).await;
    c.recv_skipping_state().await;
    rig.clock.store(T0 + 300_000, Ordering::SeqCst);
    rig.hub.tick();
    let initial = Arc::new(());
    let current = Arc::new(Mutex::new(initial.clone()));
    let hub = Arc::clone(&rig.hub);
    let current_for_load = Arc::clone(&current);
    deliver_pending(&rig.hub, || {
        let hub = Arc::clone(&hub);
        let connection = current_for_load.lock().unwrap().clone();
        let current = Arc::clone(&current_for_load);
        async move {
            Some(SwitchingJournal {
                hub,
                connection,
                current,
            })
        }
    })
    .await;
    let st = rig.hub.status();
    assert_eq!(st.failure, None);
    assert_ne!(st.delivery, "failed");
}

async fn blocked_send_with_replacement(outcome: UploadOutcome) {
    let rig = rig(false);
    rig.hub.update_gates(paired(JOURNAL_A));
    let (mut client, ack) = handshake(&rig.hub, "chrome", "production").await;
    let generation = ack["destination_generation"].as_str().unwrap().to_string();
    client
        .send(&batch(&generation, 1, snapshot("c1", "first period")))
        .await;
    assert_eq!(
        client.recv_skipping_state().await.unwrap()["result"],
        "accepted"
    );
    rig.clock.store(T0 + 300_000, Ordering::SeqCst);
    rig.hub.tick();
    client
        .send(&batch(&generation, 2, snapshot("c2", "second period")))
        .await;
    loop {
        if client.recv().await.unwrap()["type"] == "accepted" {
            break;
        }
    }
    rig.clock.store(T0 + 600_000, Ordering::SeqCst);
    rig.hub.tick();
    assert_eq!(rig.hub.outbox().len(), 2);

    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let old = FakeJournal {
        outcome,
        block: Some((Arc::clone(&entered), Arc::clone(&release))),
        ..journal(JOURNAL_A)
    };
    let current = Arc::new(Mutex::new(old));
    let task_hub = Arc::clone(&rig.hub);
    let task_current = Arc::clone(&current);
    let task = tokio::spawn(async move {
        deliver_pending(&task_hub, || {
            let loaded = task_current.lock().unwrap().clone();
            async move { Some(loaded) }
        })
        .await
    });
    entered.notified().await;

    let mut replacement = journal(JOURNAL_A);
    replacement.outcome = UploadOutcome::Held;
    replacement.assert_failure = Some(Arc::clone(&rig.hub));
    assert_eq!(current.lock().unwrap().identity, replacement.identity);
    assert!(!current.lock().unwrap().same_connection(&replacement));
    *current.lock().unwrap() = replacement.clone();
    rig.hub.set_delivery_failure(Some("relay_unavailable"));
    release.notify_one();
    let pass = task.await.unwrap();
    if outcome == UploadOutcome::Failed("relay_unavailable") {
        assert_eq!(rig.hub.status().failure, Some("relay_unavailable"));
        // The held attempt on the replacement connection checks the flag before
        // returning; it then clears its own current-connection failure state.
        deliver_pending(&rig.hub, || async { Some(replacement.clone()) }).await;
    }
    assert!(replacement.received.lock().unwrap().is_empty());

    let mut confirmed = replacement.clone();
    confirmed.outcome = UploadOutcome::Delivered;
    confirmed.assert_failure = None;
    *current.lock().unwrap() = confirmed.clone();
    let later = deliver_pending(&rig.hub, || async { Some(confirmed.clone()) }).await;
    assert_eq!(
        later.delivered,
        if outcome == UploadOutcome::Delivered {
            1
        } else {
            2
        }
    );
    assert_eq!(rig.hub.outbox().len(), 0);
    assert!(!confirmed.received.lock().unwrap().is_empty());
    if outcome == UploadOutcome::Delivered {
        assert_eq!(pass.delivered, 1);
    } else {
        assert_eq!(pass.delivered, 0);
    }
}

#[tokio::test]
async fn per_send_client_switch_does_not_apply_the_old_result_to_the_new_client() {
    blocked_send_with_replacement(UploadOutcome::Delivered).await;
    blocked_send_with_replacement(UploadOutcome::Failed("relay_unavailable")).await;
}

#[tokio::test]
async fn dropping_a_blocked_delivery_releases_its_reservation_for_discard() {
    let rig = rig(false);
    rig.hub.update_gates(paired(JOURNAL_A));
    let (mut client, ack) = handshake(&rig.hub, "chrome", "production").await;
    let generation = ack["destination_generation"].as_str().unwrap().to_string();
    client
        .send(&batch(&generation, 1, snapshot("c1", "reserved pages")))
        .await;
    assert_eq!(
        client.recv_skipping_state().await.unwrap()["result"],
        "accepted"
    );
    rig.clock.store(T0 + 300_000, Ordering::SeqCst);
    rig.hub.tick();

    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let blocked = FakeJournal {
        block: Some((Arc::clone(&entered), release)),
        ..journal(JOURNAL_A)
    };
    let hub = Arc::clone(&rig.hub);
    let task =
        tokio::spawn(
            async move { deliver_pending(&hub, || async { Some(blocked.clone()) }).await },
        );
    entered.notified().await;
    assert_eq!(rig.hub.discard_waiting(), 0);
    assert!(!rig.hub.status().custody.waiting);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(rig.hub.discard_waiting(), 0);
    assert!(rig.hub.outbox().is_empty());
}

#[tokio::test]
async fn a_held_mark_clears_a_previous_failure() {
    let rig = rig(false);
    rig.hub.update_gates(paired(JOURNAL_A));
    let (mut c, ack) = handshake(&rig.hub, "chrome", "production").await;
    let generation = ack["destination_generation"].as_str().unwrap().to_string();
    c.send(&batch(&generation, 1, snapshot("c1", "x"))).await;
    c.recv_skipping_state().await;
    rig.clock.store(T0 + 300_000, Ordering::SeqCst);
    rig.hub.tick();
    let failing = FakeJournal {
        outcome: UploadOutcome::Failed("relay_unavailable"),
        ..journal(JOURNAL_A)
    };
    deliver_pending(&rig.hub, || async { Some(failing.clone()) }).await;
    assert_eq!(rig.hub.status().delivery, "failed");
    let held = FakeJournal {
        outcome: UploadOutcome::Held,
        ..journal(JOURNAL_A)
    };
    deliver_pending(&rig.hub, || async { Some(held.clone()) }).await;
    assert_eq!(rig.hub.status().delivery, "kept_locally");
}
