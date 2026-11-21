// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

pub use solstone_folder_sync::worker::Journal;
use solstone_folder_sync::worker::{
    self as folder_worker, CapabilityCache, CheckSchedule, Event, Wake,
};
pub use solstone_folder_sync::worker::{BoxFuture, Capability};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use observer_model::ScreenshotsState;
use serde::Serialize;
use tokio::sync::{mpsc, oneshot, watch};

use crate::engine::{prepare, Engine, Pause, Prepared, Sent, Status, Upload};
use crate::rules::{route_answer, Answer, Span};

pub const SOURCE: &str = "screenshots";
pub const CAPABILITY: &str = "screenshots";

/// The most screenshots sent in one look.
pub const SENDS_PER_LOOK: usize = 8;
/// A journal that doesn't take screenshots is asked again after this.
pub const CAPABILITY_RETRY: Duration = Duration::from_secs(60);
/// How often the pause is read. A pause skips from the moment it starts.
const TICK: Duration = Duration::from_secs(1);
const SCAN_IDLE: Duration = Duration::from_secs(15);

/// The computer's pause, read on every tick.
pub type Paused = Arc<dyn Fn() -> Option<Pause> + Send + Sync>;

/// What the journal said to one upload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Delivery {
    Answered {
        http_status: u16,
        status: Option<String>,
        reason_code: Option<String>,
    },
    /// No answer to act on: unreachable, or the journal's mark isn't confirmed.
    NoAnswer,
}

/// What the app says about the source to its window.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ScreenshotsDocument {
    pub v: u32,
    pub status: Status,
}

enum Command {
    TurnOn(oneshot::Sender<ScreenshotsDocument>),
    TurnOff(oneshot::Sender<ScreenshotsDocument>),
    ChooseFolder(String, oneshot::Sender<ScreenshotsDocument>),
    Confirm(Span, oneshot::Sender<ScreenshotsDocument>),
    TryAgain(oneshot::Sender<ScreenshotsDocument>),
}

/// The running source, as the rest of the app talks to it. Default is a source
/// that is not available.
#[derive(Clone, Default)]
pub struct ScreenshotsHandle {
    inner: Option<Arc<HandleInner>>,
}

struct HandleInner {
    commands: mpsc::Sender<Command>,
    status: Arc<Mutex<Option<Status>>>,
    state: watch::Receiver<Option<ScreenshotsState>>,
    generation: Arc<AtomicU64>,
    wake: Arc<Wake>,
}

impl ScreenshotsHandle {
    pub fn status(&self) -> Option<Status> {
        self.inner.as_ref().and_then(|inner| {
            inner
                .status
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone()
        })
    }

    pub fn document(&self) -> Option<ScreenshotsDocument> {
        self.status()
            .map(|status| ScreenshotsDocument { v: 1, status })
    }

    /// The source's state word, and when it changes.
    pub fn state_changes(&self) -> watch::Receiver<Option<ScreenshotsState>> {
        self.inner
            .as_ref()
            .map_or_else(|| watch::channel(None).1, |inner| inner.state.clone())
    }

    async fn call(
        &self,
        build: impl FnOnce(oneshot::Sender<ScreenshotsDocument>) -> Command,
    ) -> Option<ScreenshotsDocument> {
        let inner = self.inner.as_ref()?;
        let (reply, answer) = oneshot::channel();
        if inner.commands.send(build(reply)).await.is_err() {
            return self.document();
        }
        answer.await.ok().or_else(|| self.document())
    }

    pub async fn turn_on(&self) -> Option<ScreenshotsDocument> {
        self.call(Command::TurnOn).await
    }

    pub async fn turn_off(&self) -> Option<ScreenshotsDocument> {
        self.call(Command::TurnOff).await
    }

    pub async fn choose_folder(&self, path: String) -> Option<ScreenshotsDocument> {
        self.call(|reply| Command::ChooseFolder(path, reply)).await
    }

    pub async fn confirm(&self, span: Span) -> Option<ScreenshotsDocument> {
        self.call(|reply| Command::Confirm(span, reply)).await
    }

    /// Read the folder again now, including what couldn't be read before.
    pub async fn try_again(&self) -> Option<ScreenshotsDocument> {
        self.call(Command::TryAgain).await
    }

    /// The owner confirmed a newly paired journal: first setup starts again.
    /// A send already under way records nothing. Returns at once.
    pub fn restart_setup(&self) {
        let Some(inner) = &self.inner else { return };
        inner.generation.fetch_add(1, Ordering::AcqRel);
        inner.wake.restart();
    }

    /// Look again soon: the pause changed.
    pub fn nudge(&self) {
        if let Some(inner) = &self.inner {
            inner.wake.nudge();
        }
    }
}

/// The source's task state.
pub(crate) struct Worker<J: Journal<Upload = Upload, Reply = Delivery>> {
    engine: Option<Engine>,
    journal: J,
    paused: Paused,
    status: Arc<Mutex<Option<Status>>>,
    state: watch::Sender<Option<ScreenshotsState>>,
    commands: mpsc::Receiver<Command>,
    generation: Arc<AtomicU64>,
    wake: Arc<Wake>,
    next_scan: CheckSchedule,
    capability: CapabilityCache,
}

impl<J: Journal<Upload = Upload, Reply = Delivery>> Worker<J> {
    pub(crate) fn new(journal: J, paused: Paused) -> (ScreenshotsHandle, Self) {
        let (commands, receiver) = mpsc::channel(8);
        let status = Arc::new(Mutex::new(None));
        let (state, state_rx) = watch::channel(None);
        let generation = Arc::new(AtomicU64::new(0));
        let wake = Arc::new(Wake::default());
        let handle = ScreenshotsHandle {
            inner: Some(Arc::new(HandleInner {
                commands,
                status: Arc::clone(&status),
                state: state_rx,
                generation: Arc::clone(&generation),
                wake: Arc::clone(&wake),
            })),
        };
        let worker = Self {
            engine: None,
            journal,
            paused,
            status,
            state,
            commands: receiver,
            generation,
            wake,
            next_scan: CheckSchedule::default(),
            capability: CapabilityCache::default(),
        };
        (handle, worker)
    }

    /// The engine shares the handle's generation, so a re-pair is seen even
    /// while a send is in flight.
    pub(crate) fn set_engine(&mut self, mut engine: Engine) {
        engine.share_generation(Arc::clone(&self.generation));
        self.engine = Some(engine);
    }

    /// Run the engine off the async runtime.
    async fn with_engine<R: Send + 'static>(
        &mut self,
        call: impl FnOnce(&mut Engine) -> R + Send + 'static,
    ) -> Option<R> {
        match folder_worker::with_state(&mut self.engine, call).await {
            Ok(result) => result,
            Err(error) => {
                tracing::error!(target: "screenshots", %error, "screenshots source stopped");
                None
            }
        }
    }

    fn pause(&self) -> Option<Pause> {
        (self.paused)()
    }

    pub(crate) fn publish(&self) {
        let Some(engine) = &self.engine else { return };
        let status = engine.status(
            self.pause().is_some(),
            self.capability.listed() == Some(false),
        );
        let state = status.state;
        *self.status.lock().unwrap_or_else(|p| p.into_inner()) = Some(status);
        self.state.send_if_modified(|current| {
            let changed = *current != Some(state);
            *current = Some(state);
            changed
        });
    }

    fn reply(&self, reply: oneshot::Sender<ScreenshotsDocument>) {
        self.publish();
        let status = self
            .status
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Some(status) = status {
            let _ = reply.send(ScreenshotsDocument { v: 1, status });
        }
    }

    pub(crate) async fn run(mut self) {
        while self.engine.is_some() {
            self.restart_if_asked().await;
            while let Ok(command) = self.commands.try_recv() {
                self.handle(command).await;
            }
            let busy = self.step().await;
            self.publish();
            let wait = if busy { Duration::ZERO } else { TICK };
            let event = folder_worker::wait(&mut self.commands, &self.wake, wait).await;
            match event {
                Event::Command(command) => self.handle(command).await,
                Event::Closed => break,
                Event::Wake => {}
            }
        }
    }

    async fn handle(&mut self, command: Command) {
        match command {
            Command::TurnOn(reply) => {
                self.with_engine(Engine::turn_on).await;
                self.after_change().await;
                self.reply(reply);
            }
            Command::TurnOff(reply) => {
                self.with_engine(Engine::turn_off).await;
                self.reply(reply);
            }
            Command::ChooseFolder(path, reply) => {
                self.with_engine(move |engine| engine.choose_folder(path))
                    .await;
                self.after_change().await;
                self.reply(reply);
            }
            Command::Confirm(span, reply) => {
                self.with_engine(move |engine| engine.confirm(span)).await;
                self.next_scan.defer(Duration::ZERO);
                self.reply(reply);
            }
            Command::TryAgain(reply) => {
                self.after_change().await;
                self.reply(reply);
            }
        }
    }

    /// Look at the folder now, so the count is there when the window asks.
    async fn after_change(&mut self) {
        let pause = self.pause();
        self.with_engine(move |engine| {
            engine.reconcile_pause(pause);
            engine.scan();
        })
        .await;
        self.next_scan.defer(SCAN_IDLE);
    }

    pub(crate) async fn restart_if_asked(&mut self) {
        if !self.wake.take_restart() {
            return;
        }
        self.with_engine(Engine::restart_setup).await;
        self.capability.clear();
        self.next_scan.defer(Duration::ZERO);
        self.publish();
    }

    /// One turn of the source. Returns whether there's more to send right away.
    pub(crate) async fn step(&mut self) -> bool {
        let pause = self.pause();
        self.with_engine(move |engine| engine.reconcile_pause(pause))
            .await;
        if !self.engine.as_ref().is_some_and(Engine::is_enabled) {
            return false;
        }
        if self.next_scan.due() {
            self.with_engine(Engine::scan).await;
            self.next_scan.defer(SCAN_IDLE);
        }
        if !self.engine.as_ref().is_some_and(Engine::is_confirmed) {
            return false;
        }
        if !self.journal_takes_screenshots().await {
            return false;
        }
        self.deliver_look().await
    }

    /// Whether the paired journal takes screenshots, asking again after a no,
    /// now and then after a yes, and whenever the pairing has changed.
    async fn journal_takes_screenshots(&mut self) -> bool {
        self.capability.check(&self.journal, CAPABILITY_RETRY).await
    }

    /// Send up to [`SENDS_PER_LOOK`]. A screenshot that has to wait ends the
    /// look; the next one tries it again. True when the look was full.
    async fn deliver_look(&mut self) -> bool {
        for _ in 0..SENDS_PER_LOOK {
            let Some(job) = self.engine.as_ref().and_then(Engine::next_send) else {
                return false;
            };
            let Some(platform) = self.engine.as_ref().map(Engine::platform) else {
                return false;
            };
            let prepared = {
                let job = job.clone();
                tokio::task::spawn_blocking(move || prepare(platform.as_ref(), &job))
                    .await
                    .unwrap_or(Prepared::Retry)
            };
            let sent = match prepared {
                Prepared::Upload(upload) => {
                    if self.wake.restart_pending() {
                        return false;
                    }
                    match folder_worker::deliver(&self.journal, &upload, None).await {
                        Some(Delivery::Answered {
                            http_status,
                            status,
                            reason_code,
                        }) => Sent::Answer(
                            route_answer(http_status, status.as_deref(), reason_code.as_deref()),
                            Some(upload.file_hash.clone()),
                        ),
                        Some(Delivery::NoAnswer) | None => Sent::Retry,
                    }
                }
                Prepared::Refused => Sent::Refused,
                Prepared::Missing => Sent::Missing,
                Prepared::Retry => Sent::Retry,
            };
            let waits = matches!(sent, Sent::Retry | Sent::Answer(Answer::Held, _));
            {
                let job = job.clone();
                self.with_engine(move |engine| engine.record(&job, sent))
                    .await;
            }
            self.publish();
            if waits || self.wake.restart_pending() {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests;
