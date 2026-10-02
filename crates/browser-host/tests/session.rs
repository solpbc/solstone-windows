// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The native-host protocol end to end over in-memory streams: relay ↔ hub,
//! the capture gate, dedup, quiescence and a journal switch.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use browser_host::argv::{BrandHint, Invocation, Mode};
use browser_host::custody::{OutboxEntry, Policy, Store};
use browser_host::hub::{Gates, Hub, HubConfig, Pairing};
use browser_host::relay::{self, RelayEnd};
use browser_host::upload::{deliver_once, Journal, UploadOutcome};
use browser_host::wire::{write_frame, FrameReader};
use native_browser_frame::{encode, Direction};
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
    let generation = ack["destination_generation"].as_str().unwrap().to_string();
    let period = ack["period_id"].as_str().unwrap().to_string();

    c.send(&batch(&generation, 1, snapshot("c1", "hello")))
        .await;
    let reply = c.recv_skipping_state().await.unwrap();
    assert_eq!(reply["result"], "accepted");
    assert_eq!(reply["period_id"], period.as_str());

    c.send(&batch(&generation, 1, snapshot("c1", "hello")))
        .await;
    let reply = c.recv_skipping_state().await.unwrap();
    assert_eq!(reply["result"], "duplicate");
    assert_eq!(reply["period_id"], period.as_str());
    assert_eq!(rig.hub.status().delivery, "kept_locally");
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
    received: Received,
    outcome: UploadOutcome,
}

impl Journal for FakeJournal {
    fn identity(&self) -> Option<&str> {
        Some(&self.identity)
    }

    async fn upload(&self, entry: &OutboxEntry, body: Vec<u8>) -> UploadOutcome {
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
        received: Arc::new(Mutex::new(Vec::new())),
        outcome: UploadOutcome::Delivered,
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
    let pass = deliver_once(&rig.hub, &held).await;
    assert_eq!(pass.delivered, 0);
    assert_eq!(rig.hub.status().delivery, "kept_locally");

    let open = journal(JOURNAL_A);
    let pass = deliver_once(&rig.hub, &open).await;
    assert_eq!(pass.delivered, 1);
    let got = open.received.lock().unwrap();
    assert!(String::from_utf8_lossy(&got[0].2).contains("WAITING_ON_MARK"));
    assert_eq!(rig.hub.status().delivery, "idle");
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
    deliver_once(&rig.hub, &failing).await;
    let st = rig.hub.status();
    assert_eq!(
        (st.delivery, st.failure),
        ("failed", Some("relay_unavailable"))
    );
    assert_eq!(rig.hub.outbox().len(), 1);
}

#[tokio::test]
async fn re_pairing_to_another_journal_delivers_none_of_the_old_text() {
    let rig = rig(false);
    rig.hub.update_gates(paired(JOURNAL_A));
    let (mut c, ack) = handshake(&rig.hub, "chrome", "production").await;
    let gen_a = ack["destination_generation"].as_str().unwrap().to_string();
    c.send(&batch(&gen_a, 1, snapshot("c1", "F6A_MARKER")))
        .await;
    c.recv_skipping_state().await;
    rig.clock.store(T0 + 300_000, Ordering::SeqCst);
    rig.hub.tick();
    // Journal A unreachable: A's text stays held.
    deliver_once(
        &rig.hub,
        &FakeJournal {
            outcome: UploadOutcome::Failed("relay_unavailable"),
            ..journal(JOURNAL_A)
        },
    )
    .await;

    // Re-pair to B: every session is told to reconnect.
    rig.hub.update_gates(paired(JOURNAL_B));
    let bye = c.recv_skipping_state().await.unwrap();
    assert_eq!(bye["reason"], "replaced");
    assert_eq!(rig.hub.status().custody.retired.generations, 1);

    // A view of B never sees A's text, even before the extension reconnects.
    let b = journal(JOURNAL_B);
    assert_eq!(deliver_once(&rig.hub, &b).await.delivered, 0);

    let (mut c, ack) = handshake(&rig.hub, "chrome", "production").await;
    let gen_b = ack["destination_generation"].as_str().unwrap().to_string();
    assert_ne!(gen_a, gen_b);
    c.send(&batch(&gen_a, 2, snapshot("c1", "late A"))).await;
    let r = c.recv_skipping_state().await.unwrap();
    assert_eq!(
        (r["reason"].as_str(), r["class"].as_str()),
        (Some("stale_generation"), Some("permanent"))
    );
    c.send(&batch(&gen_b, 3, snapshot("c1", "F6B_NEW"))).await;
    assert_eq!(c.recv_skipping_state().await.unwrap()["result"], "accepted");
    rig.clock.store(T0 + 600_000, Ordering::SeqCst);
    rig.hub.tick();

    // A stale view of A can't deliver B's text either.
    assert_eq!(
        deliver_once(&rig.hub, &journal(JOURNAL_A)).await.delivered,
        0
    );
    assert_eq!(deliver_once(&rig.hub, &b).await.delivered, 1);
    let got = b.received.lock().unwrap();
    assert_eq!(got.len(), 1);
    let text = String::from_utf8_lossy(&got[0].2);
    assert!(text.contains("F6B_NEW") && !text.contains("F6A_MARKER"));
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
    deliver_once(&rig.hub, &failing).await;
    assert_eq!(rig.hub.status().delivery, "failed");
    // Re-pairing elsewhere retires what was held: nothing is held, nothing failed.
    rig.hub.update_gates(paired(JOURNAL_B));
    let st = rig.hub.status();
    assert_eq!((st.delivery, st.failure), ("idle", None));
}

#[tokio::test]
async fn a_pause_outranks_a_full_spool() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(
        dir.path(),
        Policy {
            spool_bytes: 400,
            ..Policy::default()
        },
        Box::new(|s, l| (format!("d{s}"), format!("s{s}_{l}"))),
        T0,
    );
    let hub = Hub::new(
        HubConfig {
            development: false,
            app_version: "t".into(),
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
