// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use solstone_vault::engine::TurnOnError;
use solstone_vault::{
    Answer, Delivery, Engine, ListedVault, Phase, Refusal, ScanOutcome, State, Status,
};
use tokio::sync::{mpsc, oneshot, watch, Notify};

pub const SOURCE: &str = "obsidian";
pub const CAPABILITY: &str = "obsidian_notes";

const TICK: Duration = Duration::from_secs(2);
const CAPABILITY_TTL: Duration = Duration::from_secs(5 * 60);
const CAPABILITY_RETRY: Duration = Duration::from_secs(30);
const VAULT_CHECK_RETRY: Duration = Duration::from_secs(30);
const CLOSE_BATCH: usize = 500;

/// Whether the owner has paused. Scanning goes on; delivery waits.
pub type Paused = Arc<dyn Fn() -> bool + Send + Sync>;

/// A boxed future, so the journal seam stays object-safe across edition limits.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What the app says about the source to its window.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ObsidianDocument {
    pub v: u32,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<Refusal>,
}

pub fn decode_document(text: &str) -> Option<ObsidianDocument> {
    serde_json::from_str(text)
        .ok()
        .filter(|doc: &ObsidianDocument| doc.v == 1)
}

fn off_status() -> Status {
    Status {
        state: State::Off,
        vault: None,
        warning: None,
        last_checked_ms: None,
        waiting: 0,
        first_sync: None,
        not_on_device: 0,
        too_large: 0,
        attention: None,
    }
}

/// Whether the paired journal has said it accepts note changes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Capability {
    Supported,
    NotListed,
    /// Not paired, or no answer.
    Unknown,
}

/// What became of one attempt to send a waiting file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliveryResult {
    Answer(Answer),
    /// No answer from the journal to act on.
    NoAnswer,
    /// The file is no longer on this computer.
    FileMissing,
}

pub enum VaultList {
    Listed(Vec<ListedVault>),
    Unavailable,
}

/// The journal, as far as the source needs it. The app implements this over
/// its paired upload path; nothing here knows which journal is on the other end.
pub trait Journal: Send + Sync + 'static {
    fn capability(&self) -> BoxFuture<'_, Capability>;
    fn vault_list(&self) -> BoxFuture<'_, VaultList>;
    /// Send one file.
    fn deliver<'a>(&'a self, delivery: &'a Delivery) -> BoxFuture<'a, DeliveryResult>;
    /// Whether the journal this was last asked of is still the paired one.
    fn pairing_unchanged(&self) -> BoxFuture<'_, bool>;
}

enum Command {
    TurnOn {
        path: String,
        reply: oneshot::Sender<ObsidianDocument>,
    },
    TurnOff(oneshot::Sender<ObsidianDocument>),
    Continue(oneshot::Sender<ObsidianDocument>),
    Cancel(oneshot::Sender<ObsidianDocument>),
}

/// The running source, as the rest of the app talks to it. Default is a source
/// that is not available.
#[derive(Clone, Default)]
pub struct ObsidianHandle {
    inner: Option<Arc<HandleInner>>,
}

struct HandleInner {
    commands: mpsc::Sender<Command>,
    status: Arc<Mutex<Status>>,
    cancel: Arc<AtomicBool>,
    cancel_notify: Arc<Notify>,
    on: watch::Receiver<bool>,
    restart: Arc<AtomicBool>,
    restart_notify: Arc<Notify>,
}

impl ObsidianHandle {
    fn call_status(&self) -> Status {
        self.inner.as_ref().map_or_else(off_status, |inner| {
            inner
                .status
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone()
        })
    }

    /// Whether the source is on, and when that changes. Always off for a source
    /// that isn't available.
    pub fn on_changes(&self) -> watch::Receiver<bool> {
        self.inner
            .as_ref()
            .map_or_else(|| watch::channel(false).1, |inner| inner.on.clone())
    }

    pub fn document(&self) -> ObsidianDocument {
        ObsidianDocument {
            v: 1,
            status: self.call_status(),
            refusal: None,
        }
    }

    async fn call(
        &self,
        build: impl FnOnce(oneshot::Sender<ObsidianDocument>) -> Command,
    ) -> ObsidianDocument {
        let Some(inner) = &self.inner else {
            return self.document();
        };
        let (reply, answer) = oneshot::channel();
        if inner.commands.send(build(reply)).await.is_err() {
            return self.document();
        }
        answer.await.unwrap_or_else(|_| self.document())
    }

    pub async fn turn_on(&self, path: String) -> ObsidianDocument {
        self.call(|reply| Command::TurnOn { path, reply }).await
    }

    pub async fn turn_off(&self) -> ObsidianDocument {
        if let Some(inner) = &self.inner {
            inner.cancel.store(true, Ordering::Release);
            inner.cancel_notify.notify_one();
        }
        self.call(Command::TurnOff).await
    }

    pub async fn continue_warning(&self) -> ObsidianDocument {
        self.call(Command::Continue).await
    }

    pub async fn cancel_warning(&self) -> ObsidianDocument {
        self.call(Command::Cancel).await
    }

    /// The owner paired a journal: the vault's current notes go to it once, as
    /// their first send. Returns at once; the source does it with its own work,
    /// before it scans again.
    pub fn restart_send(&self) {
        let Some(inner) = &self.inner else { return };
        // A scan under way stops early, so the restart isn't kept waiting.
        inner.cancel.store(true, Ordering::Release);
        inner.restart.store(true, Ordering::Release);
        inner.restart_notify.notify_one();
    }
}

/// The source's task state.
pub(crate) struct Worker<J: Journal> {
    engine: Option<Engine>,
    journal: J,
    status: Arc<Mutex<Status>>,
    paused: Paused,
    commands: mpsc::Receiver<Command>,
    cancel: Arc<AtomicBool>,
    cancel_notify: Arc<Notify>,
    on: watch::Sender<bool>,
    restart: Arc<AtomicBool>,
    restart_notify: Arc<Notify>,
    next_scan: Instant,
    next_vault_check: Instant,
    capability: Option<bool>,
    capability_checked: Option<Instant>,
}

impl<J: Journal> Worker<J> {
    pub(crate) fn new(
        engine: Option<Engine>,
        journal: J,
        paused: Paused,
    ) -> (ObsidianHandle, Self) {
        let (commands, receiver) = mpsc::channel(8);
        let initial = engine
            .as_ref()
            .map_or_else(off_status, |engine| engine.status(false, None));
        let status = Arc::new(Mutex::new(initial));
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_notify = Arc::new(Notify::new());
        let (on, on_rx) = watch::channel(false);
        let restart = Arc::new(AtomicBool::new(false));
        let restart_notify = Arc::new(Notify::new());
        let handle = ObsidianHandle {
            inner: Some(Arc::new(HandleInner {
                commands,
                status: Arc::clone(&status),
                cancel: Arc::clone(&cancel),
                cancel_notify: Arc::clone(&cancel_notify),
                on: on_rx,
                restart: Arc::clone(&restart),
                restart_notify: Arc::clone(&restart_notify),
            })),
        };
        let worker = Self {
            engine,
            journal,
            status,
            paused,
            commands: receiver,
            cancel,
            cancel_notify,
            on,
            restart,
            restart_notify,
            next_scan: Instant::now(),
            next_vault_check: Instant::now(),
            capability: None,
            capability_checked: None,
        };
        (handle, worker)
    }

    pub(crate) fn set_engine(&mut self, engine: Engine) {
        self.engine = Some(engine);
    }

    /// Run the engine off the async runtime.
    async fn with_engine<R: Send + 'static>(
        &mut self,
        call: impl FnOnce(&mut Engine) -> R + Send + 'static,
    ) -> Option<R> {
        let mut engine = self.engine.take()?;
        let result = tokio::task::spawn_blocking(move || {
            let result = call(&mut engine);
            (engine, result)
        })
        .await;
        match result {
            Ok((engine, result)) => {
                self.engine = Some(engine);
                Some(result)
            }
            Err(error) => {
                tracing::error!(target: "obsidian", %error, "Obsidian vault source stopped");
                None
            }
        }
    }

    fn paused(&self) -> bool {
        (self.paused)()
    }

    pub(crate) async fn run(mut self) {
        loop {
            if self.engine.is_none() {
                break;
            }
            self.restart_if_asked().await;
            while let Ok(command) = self.commands.try_recv() {
                self.handle(command).await;
            }
            let busy = self.step().await;
            self.publish();
            if self.engine.is_none() {
                break;
            }
            let wait = if busy { Duration::ZERO } else { TICK };
            tokio::select! {
                () = self.restart_notify.notified() => {}
                command = self.commands.recv() => {
                    let Some(command) = command else { break };
                    self.handle(command).await;
                }
                () = tokio::time::sleep(wait) => {}
            }
        }
        if let Some(mut engine) = self.engine.take() {
            let _ = tokio::task::spawn_blocking(move || engine.flush()).await;
        }
    }

    pub(crate) fn publish(&self) {
        let Some(engine) = &self.engine else { return };
        let mut status = engine.status(self.paused(), self.capability);
        // The vault check waits for a journal that takes notes.
        if status.state == State::Checking && self.capability == Some(false) {
            status.state = State::WaitingForJournalUpdate;
        }
        let on = status.state != State::Off;
        self.on.send_if_modified(|current| {
            let changed = *current != on;
            *current = on;
            changed
        });
        *self.status.lock().unwrap_or_else(|p| p.into_inner()) = status;
    }

    fn document(&self, refusal: Option<Refusal>) -> ObsidianDocument {
        self.publish();
        ObsidianDocument {
            v: 1,
            status: self
                .status
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone(),
            refusal,
        }
    }

    async fn handle(&mut self, command: Command) {
        self.cancel.store(false, Ordering::Release);
        match command {
            Command::TurnOn { path, reply } => {
                let result = self
                    .with_engine(move |engine| engine.turn_on(std::path::Path::new(&path)))
                    .await;
                let refusal = match result {
                    Some(Err(TurnOnError::Refused(refusal))) => Some(refusal),
                    Some(Err(TurnOnError::Io(error))) => {
                        tracing::warn!(target: "obsidian", %error, "Couldn't turn on the Obsidian source");
                        None
                    }
                    _ => None,
                };
                if refusal.is_none() {
                    self.next_scan = Instant::now();
                    self.next_vault_check = Instant::now();
                    self.capability = None;
                    self.capability_checked = None;
                }
                let _ = reply.send(self.document(refusal));
            }
            Command::TurnOff(reply) => {
                let _ = self.with_engine(|engine| engine.turn_off()).await;
                let _ = reply.send(self.document(None));
            }
            Command::Continue(reply) => {
                let _ = self.with_engine(|engine| engine.continue_warning()).await;
                self.next_scan = Instant::now();
                let _ = reply.send(self.document(None));
            }
            Command::Cancel(reply) => {
                let _ = self.with_engine(|engine| engine.cancel_warning()).await;
                let _ = reply.send(self.document(None));
            }
        }
    }

    async fn restart_if_asked(&mut self) {
        if !self.restart.swap(false, Ordering::AcqRel) {
            return;
        }
        self.cancel.store(false, Ordering::Release);
        if let Some(Err(error)) = self.with_engine(Engine::restart_send).await {
            tracing::warn!(target: "obsidian", %error, "Couldn't start the Obsidian first send again");
        }
        self.next_scan = Instant::now();
        self.next_vault_check = Instant::now();
        self.capability = None;
        self.capability_checked = None;
        self.publish();
    }

    /// One turn of the source. Returns whether there's more to do right away.
    async fn step(&mut self) -> bool {
        if self.engine.as_ref().is_none_or(|engine| !engine.is_on()) {
            return false;
        }
        if self.engine.as_ref().and_then(Engine::phase) == Some(&Phase::Checking) {
            // The first send is only worth warning about for a journal that can
            // take it, so the check waits until this one lists the capability.
            if !self.journal_supports_notes().await {
                return false;
            }
            return self.check_vault_list().await;
        }
        if !self.engine.as_ref().is_some_and(Engine::should_scan) {
            return false;
        }
        let mut busy = false;
        if Instant::now() >= self.next_scan {
            let cancel = Arc::clone(&self.cancel);
            let outcome = self.with_engine(move |engine| engine.scan(&cancel)).await;
            match outcome {
                Some(Ok(ScanOutcome::Cancelled)) => self.cancel.store(false, Ordering::Release),
                Some(Err(error)) => {
                    tracing::warn!(target: "obsidian", %error, "Obsidian scan failed");
                }
                _ => {}
            }
            let duration = self
                .engine
                .as_ref()
                .map_or(Duration::ZERO, Engine::last_scan_duration);
            self.next_scan = Instant::now() + solstone_vault::next_scan_delay(duration);
        }
        if !self.paused() {
            let closed = self
                .with_engine(|engine| engine.close_due_windows(CLOSE_BATCH))
                .await;
            match closed {
                Some(Ok(count)) if count == CLOSE_BATCH => busy = true,
                Some(Err(error)) => {
                    tracing::warn!(target: "obsidian", %error, "Obsidian window close failed");
                }
                _ => {}
            }
            if self.journal_supports_notes().await {
                busy |= self.deliver_one().await;
            }
        }
        busy
    }

    async fn check_vault_list(&mut self) -> bool {
        if Instant::now() < self.next_vault_check {
            return false;
        }
        self.next_vault_check = Instant::now() + VAULT_CHECK_RETRY;
        if let VaultList::Listed(listed) = self.journal.vault_list().await {
            if let Some(Err(error)) = self
                .with_engine(move |engine| engine.apply_vault_list(&listed))
                .await
            {
                tracing::warn!(target: "obsidian", %error, "Couldn't save the Obsidian vault check");
            }
            self.next_scan = Instant::now();
            return true;
        }
        false
    }

    /// Whether the paired journal takes note changes, re-asking now and then or
    /// when the pairing has changed.
    async fn journal_supports_notes(&mut self) -> bool {
        let stale = match self.capability_checked {
            None => true,
            Some(_) if !self.journal.pairing_unchanged().await => true,
            Some(checked) => {
                let ttl = if self.capability == Some(true) {
                    CAPABILITY_TTL
                } else {
                    CAPABILITY_RETRY
                };
                checked.elapsed() >= ttl
            }
        };
        if stale {
            self.capability = match self.journal.capability().await {
                Capability::Supported => Some(true),
                Capability::NotListed => Some(false),
                Capability::Unknown => None,
            };
            self.capability_checked = Some(Instant::now());
        }
        self.capability == Some(true)
    }

    /// Deliver one file if the journal allows it. True if one went.
    async fn deliver_one(&mut self) -> bool {
        if self.cancel.load(Ordering::Acquire) {
            return false;
        }
        let Some(delivery) = self.engine.as_ref().and_then(Engine::next_delivery) else {
            return false;
        };
        // Turning off doesn't wait on the journal. The file stays in the outbox, and
        // if the journal did take it, sending it again answers `duplicate`.
        let result = tokio::select! {
            result = self.journal.deliver(&delivery) => result,
            () = self.cancel_notify.notified() => return false,
        };
        let answer = match result {
            DeliveryResult::NoAnswer => return false,
            DeliveryResult::FileMissing => {
                let recorded = self
                    .with_engine(move |engine| engine.record_file_missing(&delivery))
                    .await;
                return match recorded {
                    Some(Ok(())) => true,
                    Some(Err(error)) => {
                        tracing::warn!(target: "obsidian", %error, "Couldn't record a missing Obsidian file");
                        false
                    }
                    None => false,
                };
            }
            DeliveryResult::Answer(answer) => answer,
        };
        let recorded = self
            .with_engine(move |engine| engine.record_answer(&delivery, answer))
            .await;
        // Only a recorded answer has moved on: a failed delete would offer the same
        // file straight back.
        match recorded {
            Some(Ok(())) => matches!(answer, Answer::Delivered | Answer::Invalid),
            Some(Err(error)) => {
                tracing::warn!(target: "obsidian", %error, "Couldn't record an Obsidian delivery");
                false
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests;
