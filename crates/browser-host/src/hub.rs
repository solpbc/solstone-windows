// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The app side of the native host: one session per connected browser, the
//! live capture gate, and update quiescence.
//!
//! A session is a local-pipe connection from a host relay. It carries the
//! relay's `local_hello` (launch shape and mode), then the extension's own
//! frames. Every extension frame is decoded with the shared contract codec
//! here, in the app; the relay never interprets them.
//!
//! The capture gate is a live authorization: `state` is re-sent every renewal
//! interval with a freshness deadline, so an app that goes away closes the gate
//! in the browser by silence. Capture is `permitted` only while the app is
//! paired, custody is bound to that journal's generation, intake has room, and
//! the owner has not paused. A held journal mark does not close capture: those
//! batches are kept here and sent once the owner confirms the mark.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use native_browser_frame::constants::{
    CONTROL_MAX, FRESHNESS_MS_MAX, HANDSHAKE_MS_BUDGET, STATE_RENEWAL_MS_INTERVAL, WIRE_PROTOCOL,
};
use native_browser_frame::{decode, encode, DecodeOutcome, Direction};
use serde::Serialize;
use serde_json::{json, Map, Value};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, Notify};

use crate::argv::{BrandHint, Mode};
use crate::custody::{BatchInput, BatchResult, CustodyStatus, OutboxEntry, RetiredSummary, Store};
use crate::wire::{write_frame, FrameReader};

/// Largest `local_hello` the relay sends.
const LOCAL_HELLO_MAX: usize = 512;
/// How long an update waits for connected hosts to leave.
pub const QUIESCE_WAIT: Duration = Duration::from_secs(10);
/// How long a quiesce requested over the pipe holds before sessions resume.
pub const QUIESCE_LEASE: Duration = Duration::from_secs(120);
/// Concurrent sessions admitted (two browsers × a few profiles is the real case).
pub const MAX_SESSIONS: usize = 16;

/// What the rest of the app says about pairing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pairing {
    /// Not paired, or pairing in progress.
    NotPaired,
    /// Paired. `identity` is the strict journal identity of the current
    /// credential, `None` if it could not be read.
    Paired { identity: Option<String> },
}

/// The inputs to the capture gate, published by the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gates {
    pub pairing: Pairing,
    pub paused: bool,
}

impl Default for Gates {
    fn default() -> Self {
        Self {
            pairing: Pairing::NotPaired,
            paused: false,
        }
    }
}

#[derive(Debug, Clone)]
enum Publication {
    State,
    Boundary,
    Bye {
        reason: &'static str,
        target: Option<u64>,
    },
}

/// A connected browser, for the owner-facing row and the health dump.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ConnectedBrowser {
    pub brand: String,
    pub connected_at_ms: u64,
}

#[derive(Debug, Clone)]
struct SessionEntry {
    brand: String,
    inst: String,
    connected_at_ms: u64,
}

/// The browser path's status for the health dump.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BrowserStatus {
    pub capture: &'static str,
    pub delivery: &'static str,
    pub failure: Option<&'static str>,
    pub connected: Vec<ConnectedBrowser>,
    pub custody: CustodyStatus,
}

pub type Clock = Box<dyn Fn() -> u64 + Send + Sync>;

pub struct HubConfig {
    /// Admit the development ids (never set by a release build).
    pub development: bool,
    pub app_version: String,
}

pub struct Hub {
    cfg: HubConfig,
    clock: Clock,
    store: Mutex<Store>,
    gates: Mutex<Gates>,
    failure: Mutex<Option<&'static str>>,
    quiescing: AtomicBool,
    sessions: Mutex<HashMap<u64, SessionEntry>>,
    next_session: AtomicU64,
    sessions_changed: Notify,
    tx: broadcast::Sender<Publication>,
}

impl Hub {
    pub fn new(cfg: HubConfig, store: Store, clock: Clock) -> Arc<Self> {
        let (tx, _) = broadcast::channel(64);
        Arc::new(Self {
            cfg,
            clock,
            store: Mutex::new(store),
            gates: Mutex::new(Gates::default()),
            failure: Mutex::new(None),
            quiescing: AtomicBool::new(false),
            sessions: Mutex::new(HashMap::new()),
            next_session: AtomicU64::new(1),
            sessions_changed: Notify::new(),
            tx,
        })
    }

    fn now(&self) -> u64 {
        (self.clock)()
    }

    fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn publish(&self, p: Publication) {
        let _ = self.tx.send(p);
    }

    /// Accept new gate inputs. A different journal identity retires custody
    /// before anything else happens, and every session is told to reconnect so
    /// the extension learns the new generation.
    pub fn update_gates(&self, gates: Gates) {
        let changed = {
            let mut current = self.gates.lock().unwrap_or_else(|p| p.into_inner());
            if *current == gates {
                false
            } else {
                *current = gates.clone();
                true
            }
        };
        if !changed {
            return;
        }
        if let Pairing::Paired {
            identity: Some(identity),
        } = &gates.pairing
        {
            let now = self.now();
            let mut store = self.store();
            let had_generation = store.generation().is_some();
            let changed = store.ensure_generation(identity, now);
            drop(store);
            // A first binding needs no reconnect: `state` carries it. Replacing a
            // generation does, so no session keeps stamping the old one.
            if changed {
                // A failure belonged to the previous journal's delivery.
                *self.failure.lock().unwrap_or_else(|p| p.into_inner()) = None;
            }
            if changed && had_generation {
                self.publish(Publication::Bye {
                    reason: "replaced",
                    target: None,
                });
            }
        }
        self.publish(Publication::State);
    }

    /// Advance the period clock (call about once a second).
    pub fn tick(&self) {
        let now = self.now();
        if self.store().tick(now) {
            self.publish(Publication::Boundary);
            self.publish(Publication::State);
        }
    }

    /// The uploader's latest delivery result: `None` when the last pass
    /// delivered (or had nothing to do), else a contract failure code.
    pub fn set_delivery_failure(&self, failure: Option<&'static str>) {
        let mut current = self.failure.lock().unwrap_or_else(|p| p.into_inner());
        if *current != failure {
            *current = failure;
            drop(current);
            self.publish(Publication::State);
        }
    }

    pub fn identity(&self) -> Option<String> {
        self.store().identity().map(str::to_string)
    }

    pub fn outbox(&self) -> Vec<OutboxEntry> {
        self.store().outbox()
    }

    pub fn delivered(&self, entry: &OutboxEntry) -> std::io::Result<()> {
        let result = self.store().delivered(entry);
        self.publish(Publication::State);
        result
    }

    pub fn retired(&self) -> RetiredSummary {
        self.store().retired()
    }

    pub fn discard_retired(&self) -> RetiredSummary {
        self.store().discard_retired()
    }

    /// Update quiescence: refuse new sessions, tell every connected host to go
    /// (`bye(update)`), finalize the open period, and wait up to `wait` for the
    /// hosts to exit. Returns whether every session closed.
    pub async fn quiesce(&self, wait: Duration) -> bool {
        self.quiescing.store(true, Ordering::SeqCst);
        self.publish(Publication::Bye {
            reason: "update",
            target: None,
        });
        let now = self.now();
        self.store().finalize_now(now);
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let notified = self.sessions_changed.notified();
            if self.session_count() == 0 {
                return true;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return self.session_count() == 0;
            }
        }
    }

    /// The update did not happen: admit sessions again.
    pub fn resume(&self) {
        self.quiescing.store(false, Ordering::SeqCst);
    }

    pub fn is_quiescing(&self) -> bool {
        self.quiescing.load(Ordering::SeqCst)
    }

    pub fn session_count(&self) -> usize {
        self.sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .len()
    }

    pub fn status(&self) -> BrowserStatus {
        let (capture, delivery, failure, _, _, custody) = self.compute();
        let mut connected: Vec<ConnectedBrowser> = self
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .map(|s| ConnectedBrowser {
                brand: s.brand.clone(),
                connected_at_ms: s.connected_at_ms,
            })
            .collect();
        connected.sort_by(|a, b| {
            a.brand
                .cmp(&b.brand)
                .then(a.connected_at_ms.cmp(&b.connected_at_ms))
        });
        BrowserStatus {
            capture,
            delivery,
            failure,
            connected,
            custody,
        }
    }

    #[allow(clippy::type_complexity)]
    fn compute(
        &self,
    ) -> (
        &'static str,
        &'static str,
        Option<&'static str>,
        Option<String>,
        Option<String>,
        CustodyStatus,
    ) {
        let now = self.now();
        let custody = self.store().status(now);
        let gates = self.gates.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let upload_failure = *self.failure.lock().unwrap_or_else(|p| p.into_inner());
        let paired = matches!(gates.pairing, Pairing::Paired { .. });

        // A delivery failure is only true while something is held to deliver.
        let delivery_failure = if custody.failed {
            Some("local_io")
        } else if custody.held_bytes > 0 {
            upload_failure
        } else {
            None
        };
        let delivery = if delivery_failure.is_some() {
            "failed"
        } else if custody.held_bytes > 0 {
            "kept_locally"
        } else if paired {
            "idle"
        } else {
            "unknown"
        };

        let bound = match &gates.pairing {
            Pairing::Paired {
                identity: Some(identity),
            } => custody.generation.is_some() && self.store().identity() == Some(identity.as_str()),
            _ => false,
        };
        let capture = if self.is_quiescing() || custody.failed {
            "unavailable"
        } else if !paired {
            "not_paired"
        } else if !bound {
            "unavailable"
        } else if gates.paused {
            // The owner's own move outranks the app's holds.
            "paused"
        } else if custody.full {
            "intake_off"
        } else {
            "permitted"
        };
        let failure = delivery_failure.or(if custody.full {
            Some("queue_full")
        } else {
            None
        });
        let (generation, period) = if matches!(capture, "unavailable" | "not_paired") {
            (None, None)
        } else {
            (custody.generation.clone(), custody.period_id.clone())
        };
        (capture, delivery, failure, generation, period, custody)
    }

    /// The `hello_ack`/`state` message for the current gate.
    fn state_message(&self, kind: &str) -> Value {
        let (capture, delivery, failure, generation, period, custody) = self.compute();
        let mut m = Map::new();
        m.insert("type".into(), json!(kind));
        m.insert("capture".into(), json!(capture));
        m.insert("delivery".into(), json!(delivery));
        m.insert("freshness_ms".into(), json!(FRESHNESS_MS_MAX));
        m.insert("destination_generation".into(), json!(generation));
        m.insert("period_id".into(), json!(period));
        if let Some(f) = failure {
            m.insert("failure".into(), json!(f));
        }
        m.insert(
            "custody".into(),
            json!({"full": custody.full, "stale": custody.stale}),
        );
        m.insert("version".into(), json!(self.cfg.app_version));
        Value::Object(m)
    }

    fn admits_brand(&self, hint: BrandHint, mode: Mode, brand: &str) -> bool {
        hint.admits(brand) && (mode == Mode::Production || self.cfg.development)
    }

    /// Offer one batch (blocking file I/O; call off the async reactor).
    fn offer(&self, batch: &Value, inst: &str) -> Value {
        let generation = batch["destination_generation"].as_str().unwrap_or("");
        let batch_id = batch["batch_id"].as_str().unwrap_or("");
        let gates = self.gates.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let now = self.now();
        let result = if self.is_quiescing() || !matches!(gates.pairing, Pairing::Paired { .. }) {
            BatchResult::Rejected {
                reason: "resource_exhausted",
            }
        } else {
            let records = batch["records"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let input = BatchInput {
                generation,
                inst,
                batch_id,
                queued_at_ms: batch["queued_at_ms"].as_u64().unwrap_or(0),
                records,
            };
            let mut store = self.store();
            let before = store.period_id().map(str::to_string);
            let result = store.offer(&input, now);
            let after = store.period_id().map(str::to_string);
            drop(store);
            if before != after {
                self.publish(Publication::Boundary);
            }
            if matches!(
                &result,
                BatchResult::Rejected {
                    reason: "queue_full"
                }
            ) || matches!(&result, BatchResult::Accepted { .. })
            {
                self.publish(Publication::State);
            }
            result
        };
        let mut m = Map::new();
        m.insert("type".into(), json!("accepted"));
        match &result {
            BatchResult::Accepted { period_id } => {
                m.insert("result".into(), json!("accepted"));
                m.insert("period_id".into(), json!(period_id));
            }
            BatchResult::Duplicate { period_id } => {
                m.insert("result".into(), json!("duplicate"));
                m.insert("period_id".into(), json!(period_id));
            }
            BatchResult::Rejected { reason } => {
                m.insert("result".into(), json!("rejected"));
                m.insert("reason".into(), json!(reason));
                m.insert("class".into(), json!(BatchResult::class(reason)));
            }
        }
        m.insert("destination_generation".into(), json!(generation));
        m.insert("inst".into(), json!(inst));
        m.insert("batch_id".into(), json!(batch_id));
        Value::Object(m)
    }

    fn register(&self, brand: &str, inst: &str) -> Option<u64> {
        let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        let replaced: Vec<u64> = sessions
            .iter()
            .filter(|(_, s)| s.brand == brand && s.inst == inst)
            .map(|(id, _)| *id)
            .collect();
        if sessions.len() - replaced.len() >= MAX_SESSIONS {
            return None;
        }
        for id in replaced {
            sessions.remove(&id);
            self.publish(Publication::Bye {
                reason: "replaced",
                target: Some(id),
            });
        }
        let id = self.next_session.fetch_add(1, Ordering::SeqCst);
        sessions.insert(
            id,
            SessionEntry {
                brand: brand.to_string(),
                inst: inst.to_string(),
                connected_at_ms: self.now(),
            },
        );
        drop(sessions);
        self.sessions_changed.notify_waiters();
        Some(id)
    }

    fn unregister(&self, id: u64) {
        self.sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&id);
        self.sessions_changed.notify_waiters();
    }

    /// Serve one host connection to completion.
    pub async fn serve<S>(self: Arc<Self>, stream: S)
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        if self.is_quiescing() {
            return;
        }
        let (r, mut w) = tokio::io::split(stream);
        let mut reader = FrameReader::new(r, Direction::ExtensionToHost);
        let deadline = tokio::time::Instant::now() + Duration::from_millis(HANDSHAKE_MS_BUDGET);

        // The relay's local hello: launch shape and mode.
        let Ok(Ok(Some(local))) = tokio::time::timeout_at(deadline, reader.next()).await else {
            return;
        };
        if local.len() > LOCAL_HELLO_MAX {
            return;
        }
        let Ok(local) = serde_json::from_slice::<Value>(&local) else {
            return;
        };
        if local["type"] == "local_quiesce" {
            // The app's own updater, run as a separate process (`--apply-update`),
            // asks before it applies. Same-user only, like every pipe client.
            let closed = self.quiesce(QUIESCE_WAIT).await;
            let reply = json!({"type": "local_quiesced", "closed": closed});
            if let Ok(bytes) = serde_json::to_vec(&reply) {
                let _ = write_frame(&mut w, Direction::HostToExtension, &bytes).await;
            }
            // If no update follows (the apply failed), admit browsers again.
            let hub = Arc::clone(&self);
            tokio::spawn(async move {
                tokio::time::sleep(QUIESCE_LEASE).await;
                hub.resume();
            });
            return;
        }
        let (Some(hint), Some(mode)) = (
            local["brand"].as_str().and_then(BrandHint::parse),
            local["mode"].as_str().and_then(Mode::parse),
        ) else {
            return;
        };
        if local["type"] != "local_hello" || (mode == Mode::Development && !self.cfg.development) {
            return;
        }

        // The extension's hello.
        let Ok(Ok(Some(first))) = tokio::time::timeout_at(deadline, reader.next()).await else {
            return;
        };
        if first.len() > CONTROL_MAX {
            return;
        }
        let hello = match decode(&first, Direction::ExtensionToHost) {
            DecodeOutcome::Accept(v) if v["type"] == "hello" => v,
            DecodeOutcome::Unsupported { behind, .. } => {
                let reply =
                    json!({"type": "unsupported", "protocol": WIRE_PROTOCOL, "behind": behind});
                let _ = send(&mut w, &reply).await;
                tracing::info!(target: "browser", component = "session", outcome = "unsupported", behind = %behind, "browser hello");
                return;
            }
            _ => return,
        };
        let brand = hello["brand"].as_str().unwrap_or("").to_string();
        let inst = hello["inst"].as_str().unwrap_or("").to_string();
        if !self.admits_brand(hint, mode, &brand) {
            return;
        }
        let mut rx = self.tx.subscribe();
        let Some(id) = self.register(&brand, &inst) else {
            return;
        };
        tracing::info!(target: "browser", component = "session", outcome = "connected", brand = %brand, "browser session");

        let ack = self.state_message("hello_ack");
        let mut sent_generation = ack["destination_generation"].as_str().map(str::to_string);
        let mut sent_period = ack["period_id"].as_str().map(str::to_string);
        let mut ok = send(&mut w, &ack).await;

        let renewal = Duration::from_millis(STATE_RENEWAL_MS_INTERVAL);
        let mut renew = tokio::time::interval_at(tokio::time::Instant::now() + renewal, renewal);
        while ok {
            tokio::select! {
                frame = reader.next() => {
                    let Ok(Some(frame)) = frame else { break };
                    let batch = match decode(&frame, Direction::ExtensionToHost) {
                        DecodeOutcome::Accept(v) if v["type"] == "batch" && v["inst"] == inst.as_str() => v,
                        _ => break,
                    };
                    let hub = Arc::clone(&self);
                    let inst = inst.clone();
                    let Ok(reply) = tokio::task::spawn_blocking(move || hub.offer(&batch, &inst)).await else { break };
                    ok = send(&mut w, &reply).await;
                }
                publication = rx.recv() => {
                    match publication {
                        Ok(Publication::Bye { reason, target }) if target.is_none_or(|t| t == id) => {
                            let _ = send(&mut w, &json!({"type": "bye", "reason": reason})).await;
                            break;
                        }
                        Ok(Publication::Bye { .. }) => {}
                        Ok(Publication::Boundary) => {
                            let state = self.state_message("state");
                            let generation = state["destination_generation"].as_str().map(str::to_string);
                            let period = state["period_id"].as_str().map(str::to_string);
                            if let (Some(g), Some(p)) = (&generation, &period) {
                                if generation == sent_generation && period != sent_period {
                                    ok = send(&mut w, &json!({"type": "boundary", "destination_generation": g, "period_id": p})).await;
                                    sent_period = period;
                                }
                            }
                        }
                        Ok(Publication::State) | Err(broadcast::error::RecvError::Lagged(_)) => {
                            let state = self.state_message("state");
                            sent_generation = state["destination_generation"].as_str().map(str::to_string);
                            sent_period = state["period_id"].as_str().map(str::to_string);
                            ok = send(&mut w, &state).await;
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = renew.tick() => {
                    let state = self.state_message("state");
                    sent_generation = state["destination_generation"].as_str().map(str::to_string);
                    sent_period = state["period_id"].as_str().map(str::to_string);
                    ok = send(&mut w, &state).await;
                }
            }
        }
        self.unregister(id);
        tracing::info!(target: "browser", component = "session", outcome = "closed", brand = %brand, "browser session");
    }
}

/// Encode a host message, check it against the contract, and write it.
async fn send<W: AsyncWrite + Unpin>(w: &mut W, message: &Value) -> bool {
    let Ok(bytes) = encode(message) else {
        tracing::warn!(target: "browser", component = "session", outcome = "encode_failed", "host message");
        return false;
    };
    if !matches!(
        decode(&bytes, Direction::HostToExtension),
        DecodeOutcome::Accept(_)
    ) {
        tracing::warn!(target: "browser", component = "session", outcome = "invalid_host_message", kind = %message["type"], "host message");
        return false;
    }
    write_frame(w, Direction::HostToExtension, &bytes)
        .await
        .is_ok()
}
