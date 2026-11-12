// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! File import is a sibling custody loop; closing its page never stops it.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use observer_model::{PairingPhase, SyncSnapshot};
use pl_transport_win::import_client::{ImportFile, ImportReply};
use pl_transport_win::import_store::{self, Intake, Refusal, SavedProgress, Store, VersionGate};
use pl_transport_win::ObserverClient;
use serde::Serialize;
use tauri::{Emitter, Manager};
use tokio::sync::{watch, Notify};

static SERVICE: OnceLock<Arc<Service>> = OnceLock::new();

struct Service {
    store: Mutex<Option<Store>>,
    intake_lock: tokio::sync::Mutex<()>,
    wake: Notify,
    changes: watch::Sender<()>,
    identity: Mutex<Option<(String, String)>>,
    auth_blocked: Mutex<Option<String>>,
}

#[derive(Serialize)]
pub struct Row {
    id: String,
    name: String,
    state: &'static str,
    reason: Option<String>,
    retryable: bool,
    may_have_reached: bool,
    missing_copy: bool,
}

#[derive(Serialize)]
pub struct Document {
    recent: &'static str,
    rows: Vec<Row>,
    intake: Option<Intake>,
    interrupted: bool,
    waiting: u64,
    needs_owner: bool,
    available: bool,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn current_sync(app: &tauri::AppHandle) -> SyncSnapshot {
    app.state::<crate::app::AppState>()
        .sync
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

fn document_for(
    store: Option<&Store>,
    sync: &SyncSnapshot,
    journal: &str,
    auth_blocked: bool,
) -> Document {
    let Some(store) = store else {
        return Document {
            recent: "couldn't read imports on this device. nothing has been removed. restart the solstone app to try again.",
            rows: Vec::new(),
            intake: None,
            interrupted: false,
            waiting: 0,
            needs_owner: true,
            available: false,
        };
    };
    let gate =
        import_store::version_gate(sync.journal_version.as_deref(), sync.journal_version_fresh);
    let version = sync.journal_version.as_deref().unwrap_or("");
    let mut rows = Vec::new();
    // Intake continues independently of the page. Material still being copied
    // is not in the item map yet, but can continue without another owner action.
    let mut pending = store
        .intake
        .as_ref()
        .filter(|intake| intake.copying)
        .map_or(0, |intake| intake.total.saturating_sub(intake.done) as u64);
    let mut attention = 0;
    for item in store.items.values() {
        let refusal = item
            .refusal
            .as_ref()
            .filter(|_| item.refused_here(journal, version));
        let active = store.active.as_deref() == Some(&item.id);
        if refusal.is_some() {
            attention += 1;
        } else {
            pending += 1;
        }
        rows.push(Row {
            id: item.id.clone(),
            name: item.name.clone(),
            state: if active {
                "sending"
            } else if refusal.is_some() {
                "refused"
            } else {
                "waiting"
            },
            reason: refusal.map(|r| r.sentence.clone()),
            retryable: refusal.is_some_and(|r| r.retryable),
            may_have_reached: item.may_have_reached,
            missing_copy: item.missing_copy,
        });
    }
    let recent = if pending > 0 {
        if auth_blocked {
            "your journal stopped accepting this device. imports wait here until you pair again."
        } else {
            match sync.pairing.phase {
            PairingPhase::NotPaired => "imports wait on this device until you connect a journal.",
            PairingPhase::AwaitingConfirmation => "imports wait until you confirm your journal's mark.",
            PairingPhase::Failed => "your journal stopped accepting this device. imports wait here until you pair again.",
            PairingPhase::Pairing => "this device is not paired with your journal. imports wait here until you connect a journal.",
            PairingPhase::Paired => match gate {
                VersionGate::Unknown => "checking your journal.",
                VersionGate::Old => "imports wait until your journal is updated.",
                VersionGate::Supported if store.active.is_some() => "sending to your journal.",
                VersionGate::Supported => "waiting to go to your journal.",
            },
        }
        }
    } else if attention > 0 {
        "imports here won't go on their own. each one says why."
    } else if !journal.is_empty() && store.delivered_to.as_deref() == Some(journal) {
        "in your journal."
    } else {
        "nothing yet."
    };
    Document {
        available: true,
        recent,
        rows,
        intake: store.intake.clone(),
        interrupted: store.interrupted,
        waiting: if gate == VersionGate::Old || auth_blocked {
            0
        } else {
            pending
        },
        needs_owner: attention > 0
            || (pending > 0 && gate == VersionGate::Old)
            || store.interrupted
            || (pending > 0 && auth_blocked),
    }
}

fn current_identity(service: &Service, sync: &SyncSnapshot) -> (String, bool) {
    let journal = service
        .identity
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .filter(|(binding, _)| binding == &sync.pairing.binding)
        .map(|(_, id)| id.clone())
        .unwrap_or_default();
    let blocked = service
        .auth_blocked
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .is_some_and(|b| b == &sync.pairing.binding);
    (journal, blocked)
}

fn publish(app: &tauri::AppHandle, service: &Service) {
    let sync = current_sync(app);
    let (journal, blocked) = current_identity(service, &sync);
    let document = document_for(
        service
            .store
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref(),
        &sync,
        &journal,
        blocked,
    );
    {
        let state = app.state::<crate::app::AppState>();
        let mut sync = state.sync.lock().unwrap_or_else(|p| p.into_inner());
        sync.imports_waiting = document.waiting;
        sync.imports_need_owner = document.needs_owner;
    }
    service.changes.send_replace(());
    if let Err(error) = app.emit("import://changed", &document) {
        tracing::warn!(target: "import", %error, "import state could not reach the window");
    }
}

pub fn status(app: &tauri::AppHandle) -> Option<Document> {
    let service = SERVICE.get()?;
    let sync = current_sync(app);
    let (journal, blocked) = current_identity(service, &sync);
    Some(document_for(
        service
            .store
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref(),
        &sync,
        &journal,
        blocked,
    ))
}

pub fn on_changes() -> Option<watch::Receiver<()>> {
    SERVICE.get().map(|s| s.changes.subscribe())
}

pub fn start(app: tauri::AppHandle) {
    let root = platform_win::local_data_root().join("import");
    let store = match Store::open(root) {
        Ok(store) => Some(store),
        Err(error) => {
            tracing::warn!(target: "import", %error, "import custody could not open");
            None
        }
    };
    let (changes, _) = watch::channel(());
    let service = Arc::new(Service {
        store: Mutex::new(store),
        intake_lock: tokio::sync::Mutex::new(()),
        wake: Notify::new(),
        changes,
        identity: Mutex::new(None),
        auth_blocked: Mutex::new(None),
    });
    let _ = SERVICE.set(service.clone());
    tauri::async_runtime::spawn(async move {
        loop {
            if let Err(error) = deliver_pass(&app, &service).await {
                tracing::warn!(target: "import", %error, "import delivery retained its local copy");
            }
            publish(&app, &service);
            tokio::select! {
                _ = service.wake.notified() => {},
                _ = tokio::time::sleep(Duration::from_secs(30)) => {},
            }
        }
    });
}

async fn client(app: &tauri::AppHandle) -> Option<Arc<ObserverClient>> {
    let state = app.state::<crate::app::AppState>();
    pl_transport_win::access::load_current_client(&state.credential_access).await
}

async fn deliver_pass(app: &tauri::AppHandle, service: &Service) -> std::io::Result<()> {
    let sync = current_sync(app);
    let Some(client) = client(app).await else {
        return Ok(());
    };
    let binding = client.journal_identity().client_cert_sha256;
    if binding == sync.pairing.binding {
        *service.identity.lock().unwrap_or_else(|p| p.into_inner()) =
            Some((binding.clone(), client.credential().instance_id.clone()));
    }
    if service
        .auth_blocked
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        == Some(&binding)
    {
        return Ok(());
    }
    if sync.pairing.phase != PairingPhase::Paired
        || import_store::version_gate(sync.journal_version.as_deref(), sync.journal_version_fresh)
            != VersionGate::Supported
    {
        return Ok(());
    }
    if !client.gate_open() || client.journal_identity().client_cert_sha256 != sync.pairing.binding {
        return Ok(());
    }
    let journal = client.credential().instance_id.clone();
    let version = sync.journal_version.clone().unwrap_or_default();
    let ids: Vec<_> = {
        let store = service.store.lock().unwrap_or_else(|p| p.into_inner());
        let Some(store) = store.as_ref() else {
            return Ok(());
        };
        let mut items: Vec<_> = store
            .items
            .values()
            .filter(|item| {
                !item.refused_here(&journal, &version)
                    && store.confirming_remove.as_deref() != Some(&item.id)
            })
            .collect();
        items.sort_by_key(|item| item.added);
        items.into_iter().map(|item| item.id.clone()).collect()
    };
    for id in ids {
        if !same_client(app, &client).await {
            break;
        }
        let (mut item, payload) = {
            let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
            let Some(store) = guard.as_mut() else {
                break;
            };
            let Some(mut item) = store.items.get(&id).cloned() else {
                continue;
            };
            if store.confirming_remove.as_deref() == Some(&id) {
                continue;
            }
            item.may_have_reached = true;
            store.put(item.clone())?;
            store.active = Some(id.clone());
            (item, store.payload(&id))
        };
        publish(app, service);
        let result: std::io::Result<Option<Result<ImportReply, pl_transport_win::RouteError>>> =
            async {
                if item.progress_for(&journal).is_none() {
                    let reply = client
                        .save_import_file(ImportFile {
                            id: &item.id,
                            name: &item.name,
                            content_type: &item.mime,
                            path: &payload,
                        })
                        .await;
                    match reply {
                        Ok(ImportReply::Saved(saved)) => {
                            if !same_client(app, &client).await {
                                return Ok(None);
                            }
                            item.saved = Some(SavedProgress {
                                journal: journal.clone(),
                                saved,
                            });
                            item.refusal = None;
                            if let Some(store) = service
                                .store
                                .lock()
                                .unwrap_or_else(|p| p.into_inner())
                                .as_mut()
                            {
                                store.put(item.clone())?;
                            }
                        }
                        other => return Ok(Some(other)),
                    }
                }
                if !same_client(app, &client).await {
                    return Ok(None);
                }
                Ok(Some(
                    client
                        .start_import(item.progress_for(&journal).expect("saved progress"))
                        .await,
                ))
            }
            .await;
        {
            let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
            let Some(store) = guard.as_mut() else {
                break;
            };
            store.active = None;
            match result? {
                Some(Ok(ImportReply::Delivered)) => store.delivered(&id, &journal)?,
                Some(Ok(ImportReply::Refused {
                    sentence,
                    retryable,
                    clear_saved,
                })) => {
                    if clear_saved {
                        item.saved = None;
                    }
                    item.refusal = Some(Refusal {
                        journal: journal.clone(),
                        version: version.clone(),
                        sentence,
                        retryable,
                    });
                    store.put(item)?;
                }
                Some(Ok(ImportReply::AuthStop)) => {
                    *service
                        .auth_blocked
                        .lock()
                        .unwrap_or_else(|p| p.into_inner()) = Some(binding.clone());
                    tracing::warn!(target: "import", "journal refused import authentication");
                    break;
                }
                Some(Err(pl_transport_win::RouteError::Transport(
                    pl_transport_win::TransportError::Io(error),
                ))) if error.kind() == std::io::ErrorKind::NotFound => {
                    item.missing_copy = true;
                    item.refusal = Some(Refusal {
                        journal: journal.clone(),
                        version: version.clone(),
                        sentence: "the copy on this device is missing".into(),
                        retryable: false,
                    });
                    store.put(item)?;
                }
                Some(Err(error)) => {
                    tracing::warn!(target: "import", %error, item = %id, "import request will retry");
                }
                Some(Ok(ImportReply::Retry | ImportReply::Saved(_))) => {
                    tracing::warn!(target: "import", item = %id, "import answer did not prove delivery; will retry");
                }
                None => break,
            }
        }
        publish(app, service);
    }
    Ok(())
}

async fn same_client(app: &tauri::AppHandle, asked: &Arc<ObserverClient>) -> bool {
    client(app)
        .await
        .is_some_and(|current| Arc::ptr_eq(&current, asked) && current.gate_open())
}

pub fn add_paths(app: tauri::AppHandle, paths: Vec<PathBuf>) {
    let Some(service) = SERVICE.get().cloned() else {
        return;
    };
    tauri::async_runtime::spawn(async move {
        let _intake = service.intake_lock.lock().await;
        let root = {
            let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
            let Some(store) = guard.as_mut() else {
                return;
            };
            store.intake = Some(Intake {
                copying: true,
                total: paths.len(),
                ..Default::default()
            });
            store.root().to_path_buf()
        };
        publish(&app, &service);
        for path in paths {
            let root = root.clone();
            let result = tauri::async_runtime::spawn_blocking(move || {
                import_store::new_id()
                    .map_err(|_| import_store::IntakeRefusal {
                        name: "file".into(),
                        reason: "this couldn't be read. add it again",
                    })
                    .and_then(|id| import_store::stage(&root, &path, id, now()))
            })
            .await;
            let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
            let Some(store) = guard.as_mut() else {
                return;
            };
            let mut intake = store.intake.take().unwrap_or_default();
            match result {
                Ok(Ok(item)) => {
                    store.items.insert(item.id.clone(), item);
                    intake.accepted += 1;
                }
                Ok(Err(refusal)) => intake.refused.push(refusal),
                Err(error) => {
                    tracing::warn!(target: "import", %error, "import intake interrupted");
                    store.interrupted = true;
                }
            }
            intake.done += 1;
            store.intake = Some(intake);
            drop(guard);
            publish(&app, &service);
        }
        if let Some(store) = service
            .store
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_mut()
        {
            if let Some(intake) = &mut store.intake {
                intake.copying = false;
            }
        }
        publish(&app, &service);
        service.wake.notify_one();
    });
}

pub fn remove(app: &tauri::AppHandle, id: &str) -> std::io::Result<()> {
    if let Some(service) = SERVICE.get() {
        if let Some(store) = service
            .store
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_mut()
        {
            store.remove(id)?;
        }
        publish(app, service);
    }
    Ok(())
}

pub fn retry(app: &tauri::AppHandle, id: &str) -> std::io::Result<()> {
    if let Some(service) = SERVICE.get() {
        if let Some(store) = service
            .store
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_mut()
        {
            store.retry(id)?;
        }
        publish(app, service);
        service.wake.notify_one();
    }
    Ok(())
}

pub fn dismiss_interrupted(app: &tauri::AppHandle) -> std::io::Result<()> {
    if let Some(service) = SERVICE.get() {
        if let Some(store) = service
            .store
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_mut()
        {
            store.dismiss_interrupted()?;
        }
        publish(app, service);
    }
    Ok(())
}

#[derive(Serialize)]
pub struct RemovePrompt {
    name: String,
    may_have_reached: bool,
    missing_copy: bool,
}

pub fn prepare_remove(id: &str) -> Option<RemovePrompt> {
    let service = SERVICE.get()?;
    let mut guard = service.store.lock().unwrap_or_else(|p| p.into_inner());
    let item = guard.as_mut()?.prepare_remove(id)?;
    Some(RemovePrompt {
        name: item.name,
        may_have_reached: item.may_have_reached,
        missing_copy: item.missing_copy
            || guard.as_ref()?.payload(id).try_exists().ok() == Some(false),
    })
}

pub fn cancel_remove(id: Option<&str>) {
    if let Some(service) = SERVICE.get() {
        if let Some(store) = service
            .store
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_mut()
        {
            store.cancel_remove(id);
        }
        service.wake.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiting_counts_only_imports_that_can_continue_without_the_owner() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(root.path().join("import")).unwrap();
        let source = root.path().join("note.txt");
        std::fs::write(&source, b"held").unwrap();
        let mut item =
            import_store::stage(store.root(), &source, import_store::new_id().unwrap(), 1).unwrap();
        store.put(item.clone()).unwrap();
        let mut sync = SyncSnapshot::default();
        sync.pairing.phase = PairingPhase::Paired;
        sync.journal_version = Some("2.0.52".into());
        sync.journal_version_fresh = true;
        assert_eq!(document_for(Some(&store), &sync, "one", false).waiting, 1);
        sync.journal_version = Some("2.0.51".into());
        let old = document_for(Some(&store), &sync, "one", false);
        assert_eq!(old.waiting, 0);
        assert!(old.needs_owner);
        sync.journal_version = Some("2.0.52".into());
        item.refusal = Some(Refusal {
            journal: "one".into(),
            version: "2.0.52".into(),
            sentence: "refused".into(),
            retryable: true,
        });
        store.put(item).unwrap();
        let refused = document_for(Some(&store), &sync, "one", false);
        assert_eq!(refused.waiting, 0);
        assert!(refused.needs_owner);
        assert_eq!(refused.rows[0].state, "refused");
        let repaired = document_for(Some(&store), &sync, "two", false);
        assert_eq!(repaired.waiting, 1);
        assert!(!repaired.needs_owner);
        assert_eq!(repaired.rows[0].state, "waiting");
        let blocked = document_for(Some(&store), &sync, "two", true);
        assert_eq!(blocked.waiting, 0);
        assert!(blocked.needs_owner);
    }

    #[test]
    fn copying_material_prevents_a_false_caught_up_status() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(root.path().join("import")).unwrap();
        store.intake = Some(Intake {
            copying: true,
            done: 1,
            total: 2,
            ..Default::default()
        });
        let mut sync = SyncSnapshot::default();
        sync.pairing.phase = PairingPhase::Paired;
        sync.journal_version = Some("2.0.52".into());
        sync.journal_version_fresh = true;
        let copying = document_for(Some(&store), &sync, "one", false);
        assert_eq!(copying.waiting, 1);
        assert!(copying.rows.is_empty());
        sync.journal_version = Some("2.0.51".into());
        let old = document_for(Some(&store), &sync, "one", false);
        assert_eq!(old.waiting, 0);
        assert!(old.needs_owner);
        store.intake.as_mut().unwrap().done = 2;
        let complete = document_for(Some(&store), &sync, "one", false);
        assert_eq!(complete.waiting, 0);
        assert!(!complete.needs_owner);
    }

    #[test]
    fn unavailable_store_is_not_an_interrupted_addition() {
        let document = document_for(None, &SyncSnapshot::default(), "", false);
        assert!(!document.available);
        assert!(document.needs_owner);
        assert!(!document.interrupted);
        assert!(document.rows.is_empty());
    }
}
