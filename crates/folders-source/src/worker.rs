// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Background worker and handle for the folder source on Windows.
//! Follows the shared `solstone_folder_sync::worker` shape with pairing-change
//! generation guards, immediate snapshots, and change notifications for the
//! tray and shared-sources publisher.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use solstone_folder_sync::worker::{
    self as folder_worker, CapabilityCache, CheckSchedule, Event, Wake,
};
use source_counters::SharedCounters;
use tokio::sync::{mpsc, oneshot, watch};

use crate::engine::{DeliveryOutcome, Engine, OverlapContext};
use crate::platform::FolderPlatform;
use crate::types::{
    FacetOption, FacetsAvailability, FolderActionRequest, FolderDeclaration, FolderMode,
    FoldersDocument, PathCheckResult,
};
use crate::wire::FolderTransport;

const LOOK_EVERY: Duration = Duration::from_secs(5);
const CAPABILITY_RETRY: Duration = Duration::from_secs(60);

/// Callback returning whether whole-computer capture pause is currently active.
pub type Paused = Arc<dyn Fn() -> bool + Send + Sync>;

/// Callback returning the current Obsidian vault & screenshots paths for
/// overlap detection.
pub type OverlapProvider = Arc<dyn Fn() -> OverlapContext + Send + Sync>;

enum Command {
    CheckPath {
        path: String,
        reply: oneshot::Sender<PathCheckResult>,
    },
    Action {
        req: FolderActionRequest,
        reply: oneshot::Sender<FoldersDocument>,
    },
}

#[derive(Clone, Default)]
struct Published {
    doc: FoldersDocument,
    declarations: Vec<FolderDeclaration>,
}

/// Handle to the running folder source worker. Default is an unavailable handle.
#[derive(Clone, Default)]
pub struct FoldersHandle {
    inner: Option<Arc<HandleInner>>,
}

struct HandleInner {
    commands: mpsc::Sender<Command>,
    published: Arc<Mutex<Published>>,
    changes: watch::Receiver<u64>,
    generation: Arc<AtomicU64>,
    wake: Arc<Wake>,
}

impl FoldersHandle {
    #[must_use]
    pub fn document(&self) -> FoldersDocument {
        let Some(ref inner) = self.inner else {
            return FoldersDocument {
                v: 1,
                ..FoldersDocument::default()
            };
        };
        inner
            .published
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .doc
            .clone()
    }

    /// Current per-folder declarations (`needs_attention` -> `paused` -> `on`)
    /// for `PUT /app/network/api/clients/self/sources`.
    #[must_use]
    pub fn declared_folders(&self) -> Vec<FolderDeclaration> {
        let Some(ref inner) = self.inner else {
            return Vec::new();
        };
        inner
            .published
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .declarations
            .clone()
    }

    /// Whether any configured folder currently needs attention.
    #[must_use]
    pub fn needs_attention(&self) -> bool {
        self.document()
            .folders
            .iter()
            .any(|f| f.attention.is_some())
    }

    /// Whether any configured folder is currently active (`On`) or paused.
    #[must_use]
    pub fn any_active(&self) -> bool {
        self.document()
            .folders
            .iter()
            .any(|f| matches!(f.mode, FolderMode::On | FolderMode::Paused))
    }

    /// Monotonic revision counter that ticks whenever the snapshot or
    /// declarations change (for tray refresh and `shared_sources::observe`).
    #[must_use]
    pub fn state_changes(&self) -> watch::Receiver<u64> {
        self.inner
            .as_ref()
            .map_or_else(|| watch::channel(0).1, |inner| inner.changes.clone())
    }

    pub async fn check_path(&self, path: String) -> PathCheckResult {
        let Some(ref inner) = self.inner else {
            return PathCheckResult::Refused { path };
        };
        let (tx, rx) = oneshot::channel();
        if inner
            .commands
            .send(Command::CheckPath {
                path: path.clone(),
                reply: tx,
            })
            .await
            .is_err()
        {
            return PathCheckResult::Refused { path };
        }
        rx.await.unwrap_or(PathCheckResult::Refused { path })
    }

    pub async fn action(&self, req: FolderActionRequest) -> FoldersDocument {
        let Some(ref inner) = self.inner else {
            return self.document();
        };
        let (tx, rx) = oneshot::channel();
        if inner
            .commands
            .send(Command::Action { req, reply: tx })
            .await
            .is_err()
        {
            return self.document();
        }
        rx.await.unwrap_or_else(|_| self.document())
    }

    /// The owner confirmed a newly paired journal: restart first setup on
    /// active/paused folders and ignore any in-flight delivery reply.
    pub fn restart_setup(&self) {
        let Some(ref inner) = self.inner else {
            return;
        };
        inner.generation.fetch_add(1, Ordering::AcqRel);
        inner.wake.restart();
    }

    /// Nudge the worker immediately (e.g. when whole-computer pause toggles).
    pub fn wake(&self) {
        if let Some(ref inner) = self.inner {
            inner.wake.nudge();
        }
    }
}

pub(crate) fn spawn_worker<T: FolderTransport>(
    state_dir: PathBuf,
    platform: Arc<dyn FolderPlatform>,
    transport: Arc<T>,
    paused: Paused,
    overlap_provider: OverlapProvider,
    counters: Option<SharedCounters>,
) -> (
    FoldersHandle,
    impl std::future::Future<Output = ()> + Send + 'static,
) {
    let initial = Published {
        doc: FoldersDocument {
            v: 1,
            paired: transport.is_paired(),
            offline: transport.is_offline(),
            ..FoldersDocument::default()
        },
        declarations: Vec::new(),
    };
    let published = Arc::new(Mutex::new(initial));
    let (cmd_tx, cmd_rx) = mpsc::channel(32);
    let (changes_tx, changes_rx) = watch::channel(0u64);
    let generation = Arc::new(AtomicU64::new(0));
    let wake = Arc::new(Wake::default());

    let handle = FoldersHandle {
        inner: Some(Arc::new(HandleInner {
            commands: cmd_tx,
            published: Arc::clone(&published),
            changes: changes_rx,
            generation: Arc::clone(&generation),
            wake: Arc::clone(&wake),
        })),
    };

    let fut = async move {
        let opened = tokio::task::spawn_blocking(move || {
            let mut engine = Engine::open(state_dir, platform);
            if let Some(c) = counters {
                engine.set_intake_hook(Arc::new(move |id, at_secs| {
                    let _ = c.add_folder_items(&[id.to_owned()], at_secs);
                }));
            }
            engine
        })
        .await;
        let Ok(engine) = opened else {
            tracing::warn!(target: "folders", "folder source failed to open");
            return;
        };
        run_loop(
            engine,
            transport,
            published,
            changes_tx,
            cmd_rx,
            generation,
            wake,
            paused,
            overlap_provider,
        )
        .await;
    };

    (handle, fut)
}

#[allow(clippy::too_many_arguments)]
async fn run_loop<T: FolderTransport>(
    engine: Engine,
    transport: Arc<T>,
    published: Arc<Mutex<Published>>,
    changes_tx: watch::Sender<u64>,
    mut commands: mpsc::Receiver<Command>,
    generation: Arc<AtomicU64>,
    wake: Arc<Wake>,
    paused: Paused,
    overlap_provider: OverlapProvider,
) {
    let mut state = Some(engine);
    let mut cap_cache = CapabilityCache::default();
    let mut schedule = CheckSchedule::default();
    let mut facets: Vec<FacetOption> = Vec::new();
    let mut facets_state = FacetsAvailability::Unpaired;
    let mut supported_check = CheckSchedule::default();
    let mut rev: u64 = 0;

    // Publish initial disk state immediately before first network check.
    let computer_paused = paused();
    let _ = publish_snapshot(
        &mut state,
        &published,
        &changes_tx,
        &mut rev,
        &facets,
        facets_state,
        computer_paused,
        &*transport,
        cap_cache.listed(),
    )
    .await;

    loop {
        if wake.take_restart() {
            cap_cache.clear();
            supported_check.defer(Duration::ZERO);
            facets.clear();
            facets_state = if transport.is_paired() {
                FacetsAvailability::Offline
            } else {
                FacetsAvailability::Unpaired
            };
            let _ = folder_worker::with_state(&mut state, |eng| eng.restart_setup()).await;
        }

        if schedule.due() {
            let listed =
                transport.is_paired() && cap_cache.check(&*transport, CAPABILITY_RETRY).await;
            if listed && supported_check.due() {
                if let Some(sup) = transport.fetch_supported().await {
                    let _ = folder_worker::with_state(&mut state, move |eng| {
                        eng.set_supported_list(sup);
                    })
                    .await;
                    supported_check.defer(Duration::from_secs(300));
                } else {
                    supported_check.defer(CAPABILITY_RETRY);
                }
            }
            let _ = folder_worker::with_state(&mut state, |eng| eng.scan_all()).await;
            if listed {
                while !wake.restart_pending() {
                    let paused_now = paused();
                    let gen_before = generation.load(Ordering::Acquire);
                    let next = folder_worker::with_state(&mut state, move |eng| {
                        eng.next_delivery(paused_now)
                    })
                    .await
                    .ok()
                    .flatten()
                    .flatten();
                    let Some(delivery) = next else {
                        break;
                    };
                    let reply = folder_worker::deliver(&*transport, &delivery, None)
                        .await
                        .flatten();
                    // Discard late outcome if pairing restarted while upload was in flight.
                    if generation.load(Ordering::Acquire) != gen_before {
                        break;
                    }
                    let outcome = reply.unwrap_or(DeliveryOutcome::Hold);
                    let held = matches!(outcome, DeliveryOutcome::Hold);
                    let _ = folder_worker::with_state(&mut state, move |eng| {
                        eng.record_outcome(&delivery, outcome);
                    })
                    .await;
                    if held {
                        break;
                    }
                }
            }
            schedule.defer(LOOK_EVERY);
        }

        let computer_paused = paused();
        let _ = publish_snapshot(
            &mut state,
            &published,
            &changes_tx,
            &mut rev,
            &facets,
            facets_state,
            computer_paused,
            &*transport,
            cap_cache.listed(),
        )
        .await;

        let delay = schedule.remaining().min(LOOK_EVERY);
        match folder_worker::wait(&mut commands, &wake, delay).await {
            Event::Closed => break,
            Event::Wake => continue,
            Event::Command(Command::CheckPath { path, reply }) => {
                let ctx = overlap_provider();
                let p = PathBuf::from(path);
                let result =
                    folder_worker::with_state(&mut state, move |eng| eng.check_path(&p, &ctx))
                        .await
                        .ok()
                        .flatten()
                        .unwrap_or(PathCheckResult::Refused {
                            path: String::new(),
                        });
                let _ = reply.send(result);
            }
            Event::Command(Command::Action { req, reply }) => {
                match req {
                    FolderActionRequest::RefreshFacets => {
                        let (st, list) = transport.fetch_facets().await;
                        facets_state = st;
                        facets = list;
                    }
                    FolderActionRequest::Preview { id, span } => {
                        let id_clone = id.clone();
                        let preview = folder_worker::with_state(&mut state, move |eng| {
                            eng.preview_folder_with_items(&id_clone, span)
                        })
                        .await
                        .ok()
                        .flatten()
                        .flatten();
                        if let Some((summary, items)) = preview
                            && transport.is_paired()
                            && !items.is_empty()
                            && items.len() as u64 == summary.total_items
                            && let Some(journal_est) =
                                transport.fetch_estimate(id.clone(), items).await
                        {
                            let id_set = id.clone();
                            let _ = folder_worker::with_state(&mut state, move |eng| {
                                eng.set_journal_estimate(&id_set, journal_est);
                            })
                            .await;
                        }
                        if matches!(
                            facets_state,
                            FacetsAvailability::Unpaired | FacetsAvailability::Offline
                        ) && transport.is_paired()
                        {
                            let (st, list) = transport.fetch_facets().await;
                            facets_state = st;
                            facets = list;
                        }
                    }
                    other => {
                        let _ = folder_worker::with_state(&mut state, move |eng| {
                            apply_action(eng, other);
                        })
                        .await;
                        schedule.defer(Duration::ZERO);
                    }
                }
                let paused_now = paused();
                let snap = publish_snapshot(
                    &mut state,
                    &published,
                    &changes_tx,
                    &mut rev,
                    &facets,
                    facets_state,
                    paused_now,
                    &*transport,
                    cap_cache.listed(),
                )
                .await;
                let _ = reply.send(snap);
            }
        }
    }
}

fn apply_action(engine: &mut Engine, req: FolderActionRequest) {
    match req {
        FolderActionRequest::BeginSetup { path, overlap } => {
            let _ = engine.begin_setup(PathBuf::from(path).as_path(), overlap);
        }
        FolderActionRequest::Repoint { id, path } => {
            let _ = engine.repoint_folder(&id, PathBuf::from(path).as_path());
        }
        FolderActionRequest::SetKinds { id, kinds } => {
            engine.set_kinds(&id, kinds);
        }
        FolderActionRequest::SetFacet { id, facet } => {
            engine.set_facet(&id, facet);
        }
        FolderActionRequest::Confirm { id, span } => {
            let _ = engine.confirm_folder(&id, span);
        }
        FolderActionRequest::Pause { id } => {
            engine.pause_folder(&id);
        }
        FolderActionRequest::Resume { id } => {
            engine.resume_folder(&id);
        }
        FolderActionRequest::TurnOff { id } => {
            engine.turn_off_folder(&id);
        }
        FolderActionRequest::TurnOn { id } => {
            engine.turn_on_folder(&id);
        }
        FolderActionRequest::Remove { id } => {
            engine.remove_folder(&id);
        }
        FolderActionRequest::Retry { .. } => {
            engine.scan_all();
        }
        FolderActionRequest::Preview { .. } | FolderActionRequest::RefreshFacets => {}
    }
}

#[allow(clippy::too_many_arguments)]
async fn publish_snapshot<T: FolderTransport>(
    state: &mut Option<Engine>,
    published: &Arc<Mutex<Published>>,
    changes_tx: &watch::Sender<u64>,
    rev: &mut u64,
    facets: &[FacetOption],
    facets_state: FacetsAvailability,
    computer_paused: bool,
    transport: &T,
    journal_lists: Option<bool>,
) -> FoldersDocument {
    let facets_owned = facets.to_vec();
    let paired = transport.is_paired();
    let offline = transport.is_offline();
    let next = folder_worker::with_state(state, move |eng| {
        let doc = eng.document(
            facets_owned,
            facets_state,
            computer_paused,
            paired,
            offline,
            journal_lists,
        );
        let declarations = eng.declared_folders(computer_paused);
        Published { doc, declarations }
    })
    .await
    .ok()
    .flatten()
    .unwrap_or_else(|| Published {
        doc: FoldersDocument {
            v: 1,
            ..FoldersDocument::default()
        },
        declarations: Vec::new(),
    });

    let changed = {
        let mut guard = published.lock().unwrap_or_else(|p| p.into_inner());
        let diff = guard.doc != next.doc || guard.declarations != next.declarations;
        *guard = next.clone();
        diff
    };
    if changed {
        *rev = rev.saturating_add(1);
        let _ = changes_tx.send(*rev);
    }
    next.doc
}
